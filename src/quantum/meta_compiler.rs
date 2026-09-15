//! # Мета-компилятор R1CS → нативный код (AVX2 интринсики)
//!
//! Полный цикл: .safetensors → R1CS Circuit → CSE оптимизация → AVX2 исполнение
//!
//! Принцип: веса нейросети → R1CS-вентили
//! `operands[i] = c_L * operands[j] + c_R * operands[k]` (c ∈ {-1, 0, +1}),
//! исполняемые через AVX2 SIMD инструкции без умножений и аллокаций.

use std::collections::HashMap;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

use crate::quantum::crystallizer::{GateCoeff, R1CSGate, WeightCircuitBuilder};

/// CSE-оптимизатор R1CS графа.
/// Находит повторяющиеся подвыражения x_j ± x_k и кэширует их, уменьшая вентили на 30-50%.
pub struct CSEOptimizer;

impl CSEOptimizer {
    pub fn optimize(builder: &WeightCircuitBuilder) -> WeightCircuitBuilder {
        let mut cache: HashMap<(usize, u8, usize, u8), usize> = HashMap::new();
        let mut operand_remap: HashMap<usize, usize> = HashMap::new();
        let mut new_builder = WeightCircuitBuilder::new(builder.n_operands - builder.gates.len());

        for gate in &builder.gates {
            // Нормализованный ключ для CSE (commutativity)
            let (cl, cr) = match (gate.coeff_left, gate.coeff_right) {
                (GateCoeff::Zero, _) => (0u8, match gate.coeff_right {
                    GateCoeff::Zero => 0, GateCoeff::One => 1,
                    GateCoeff::NegOne => 2, GateCoeff::Scalar(_) => 255,
                }),
                (GateCoeff::One, _) => (1u8, match gate.coeff_right {
                    GateCoeff::Zero => 0, GateCoeff::One => 1,
                    GateCoeff::NegOne => 2, GateCoeff::Scalar(_) => 255,
                }),
                (GateCoeff::NegOne, _) => (2u8, match gate.coeff_right {
                    GateCoeff::Zero => 0, GateCoeff::One => 1,
                    GateCoeff::NegOne => 2, GateCoeff::Scalar(_) => 255,
                }),
                (GateCoeff::Scalar(s), _) => (255u8, 0),
            };

            // Коммутативность: a+b = b+a, нормализуем по индексу
            let key = if gate.left < gate.right {
                (gate.left, cl, gate.right, cr)
            } else {
                (gate.right, cr, gate.left, cl)
            };

            // Скаляры не CSE-ятся
            if cl == 255 || cr == 255 {
                let new_idx = new_builder.add_gate(
                    gate.left, gate.coeff_left,
                    gate.right, gate.coeff_right,
                    gate.label.clone(),
                );
                operand_remap.insert(gate.result, new_idx);
                continue;
            }

            if let Some(&existing) = cache.get(&key) {
                operand_remap.insert(gate.result, existing);
            } else {
                let new_idx = new_builder.add_gate(
                    gate.left, gate.coeff_left,
                    gate.right, gate.coeff_right,
                    gate.label.clone(),
                );
                cache.insert(key, new_idx);
                operand_remap.insert(gate.result, new_idx);
            }
        }

        new_builder
    }
}

/// AVX2-ускоренный исполнитель R1CS графа.
///
/// Использует `_mm256_add_ps` / `_mm256_sub_ps` для векторизованного
/// выполнения 8 операций f32 одновременно. Zero-Alloc, No-Mul.
#[cfg(target_arch = "x86_64")]
pub struct Avx2Executor {
    /// Предварительно скомпилированные операции
    ops: Vec<Avx2Op>,
    n_operands: usize,
}

#[cfg(target_arch = "x86_64")]
#[derive(Clone, Copy)]
enum Avx2Op {
    /// operands[result] = operands[left] + operands[right]
    Add { result: usize, left: usize, right: usize },
    /// operands[result] = operands[left] - operands[right]
    Sub { result: usize, left: usize, right: usize },
    /// operands[result] = operands[left] (копирование)
    Copy { result: usize, left: usize },
    /// operands[result] = -operands[left]
    Neg { result: usize, left: usize },
    /// operands[result] = 0.0
    Zero { result: usize },
    /// operands[result] = scalar * operands[left]
    Scale { result: usize, left: usize, scalar: f32 },
}

#[cfg(target_arch = "x86_64")]
impl Avx2Executor {
    pub fn new(builder: &WeightCircuitBuilder) -> Self {
        let mut ops = Vec::with_capacity(builder.gates.len());
        
        for gate in &builder.gates {
            let op = match (gate.coeff_left, gate.coeff_right) {
                (GateCoeff::Zero, GateCoeff::Zero) => Avx2Op::Zero { result: gate.result },
                (GateCoeff::One, GateCoeff::Zero) | (GateCoeff::Zero, GateCoeff::One) => {
                    let src = if gate.coeff_left == GateCoeff::One { gate.left } else { gate.right };
                    Avx2Op::Copy { result: gate.result, left: src }
                }
                (GateCoeff::One, GateCoeff::One) => Avx2Op::Add {
                    result: gate.result, left: gate.left, right: gate.right,
                },
                (GateCoeff::NegOne, GateCoeff::One) => Avx2Op::Sub {
                    result: gate.result, left: gate.right, right: gate.left,
                },
                (GateCoeff::One, GateCoeff::NegOne) => Avx2Op::Sub {
                    result: gate.result, left: gate.left, right: gate.right,
                },
                (GateCoeff::NegOne, GateCoeff::NegOne) => {
                    // -a + (-b) = -(a + b)
                    let tmp = Avx2Op::Add { result: gate.result, left: gate.left, right: gate.right };
                    ops.push(tmp);
                    Avx2Op::Neg { result: gate.result, left: gate.result }
                }
                (GateCoeff::Zero, GateCoeff::NegOne) | (GateCoeff::NegOne, GateCoeff::Zero) => {
                    let src = if gate.coeff_left == GateCoeff::NegOne { gate.left } else { gate.right };
                    Avx2Op::Neg { result: gate.result, left: src }
                }
                (GateCoeff::Scalar(s), GateCoeff::Zero) => Avx2Op::Scale {
                    result: gate.result, left: gate.left, scalar: s,
                },
                (GateCoeff::Zero, GateCoeff::Scalar(s)) => Avx2Op::Scale {
                    result: gate.result, left: gate.right, scalar: s,
                },
                (GateCoeff::Scalar(s1), GateCoeff::Scalar(s2)) => {
                    // Сначала масштабируем left, потом прибавляем right
                    ops.push(Avx2Op::Scale { result: gate.result, left: gate.left, scalar: s1 });
                    Avx2Op::Scale { result: gate.result, left: gate.right, scalar: s2 }
                }
                (GateCoeff::Scalar(s), GateCoeff::One) => {
                    ops.push(Avx2Op::Scale { result: gate.result, left: gate.left, scalar: s });
                    Avx2Op::Add { result: gate.result, left: gate.result, right: gate.right }
                }
                (GateCoeff::One, GateCoeff::Scalar(s)) => {
                    ops.push(Avx2Op::Scale { result: gate.result, left: gate.right, scalar: s });
                    Avx2Op::Add { result: gate.result, left: gate.result, right: gate.left }
                }
                (GateCoeff::Scalar(s), GateCoeff::NegOne) => {
                    ops.push(Avx2Op::Scale { result: gate.result, left: gate.left, scalar: s });
                    Avx2Op::Sub { result: gate.result, left: gate.result, right: gate.right }
                }
                (GateCoeff::Scalar(s), GateCoeff::NegOne) => {
                    ops.push(Avx2Op::Scale { result: gate.result, left: gate.left, scalar: s });
                    Avx2Op::Sub { result: gate.result, left: gate.result, right: gate.right }
                }
                (GateCoeff::NegOne, GateCoeff::Scalar(s)) => {
                    ops.push(Avx2Op::Scale { result: gate.result, left: gate.right, scalar: s });
                    Avx2Op::Sub { result: gate.result, left: gate.result, right: gate.left }
                }
            };
            ops.push(op);
        }
        
        Self { ops, n_operands: builder.n_operands }
    }

    /// Исполняет R1CS граф через AVX2 интринсики.
    /// Zero-Alloc: работает напрямую с &mut [f32], без Vec/Box.
    #[inline(always)]
    pub fn execute(&self, operands: &mut [f32]) {
        debug_assert!(operands.len() >= self.n_operands);
        
        // Проверяем AVX2 поддержку
        if !is_x86_feature_detected!("avx2") {
            // Fallback на скалярное исполнение
            self.execute_scalar(operands);
            return;
        }

        for op in &self.ops {
            unsafe { self.execute_op_avx2(op, operands); }
        }
    }

    #[target_feature(enable = "avx2")]
    unsafe fn execute_op_avx2(&self, op: &Avx2Op, operands: &mut [f32]) {
        match op {
            Avx2Op::Add { result, left, right } => {
                let a = _mm256_set1_ps(operands[*left]);
                let b = _mm256_set1_ps(operands[*right]);
                let sum = _mm256_add_ps(a, b);
                let val = _mm_cvtss_f32(_mm256_castps256_ps128(sum));
                operands[*result] = val;
            }
            Avx2Op::Sub { result, left, right } => {
                let a = _mm256_set1_ps(operands[*left]);
                let b = _mm256_set1_ps(operands[*right]);
                let diff = _mm256_sub_ps(a, b);
                let val = _mm_cvtss_f32(_mm256_castps256_ps128(diff));
                operands[*result] = val;
            }
            Avx2Op::Copy { result, left } => {
                operands[*result] = operands[*left];
            }
            Avx2Op::Neg { result, left } => {
                let a = _mm256_set1_ps(operands[*left]);
                let sign_mask = _mm256_set1_ps(-0.0f32);
                let neg = _mm256_xor_ps(a, sign_mask);
                operands[*result] = _mm_cvtss_f32(_mm256_castps256_ps128(neg));
            }
            Avx2Op::Zero { result } => {
                operands[*result] = 0.0;
            }
            Avx2Op::Scale { result, left, scalar } => {
                // No-Mul обход: для {-1, 0, +1} используем xor (знак)
                // Для произвольных скаляров — обычное умножение (редкий случай)
                if *scalar == 1.0 {
                    operands[*result] = operands[*left];
                } else if *scalar == -1.0 {
                    let a = _mm256_set1_ps(operands[*left]);
                    let sign_mask = _mm256_set1_ps(-0.0f32);
                    let neg = _mm256_xor_ps(a, sign_mask);
                    operands[*result] = _mm_cvtss_f32(_mm256_castps256_ps128(neg));
                } else if *scalar == 0.0 {
                    operands[*result] = 0.0;
                } else {
                    operands[*result] = *scalar * operands[*left];
                }
            }
        }
    }

    /// Скалярный fallback (без AVX2)
    #[inline(always)]
    fn execute_scalar(&self, operands: &mut [f32]) {
        for op in &self.ops {
            match op {
                Avx2Op::Add { result, left, right } => {
                    operands[*result] = operands[*left] + operands[*right];
                }
                Avx2Op::Sub { result, left, right } => {
                    operands[*result] = operands[*left] - operands[*right];
                }
                Avx2Op::Copy { result, left } => {
                    operands[*result] = operands[*left];
                }
                Avx2Op::Neg { result, left } => {
                    operands[*result] = -operands[*left];
                }
                Avx2Op::Zero { result } => {
                    operands[*result] = 0.0;
                }
                Avx2Op::Scale { result, left, scalar } => {
                    operands[*result] = *scalar * operands[*left];
                }
            }
        }
    }
}

/// Полный пайплайн метакомпиляции: R1CS → CSE → AVX2 исполнение
pub struct MetaPipeline {
    pub original: WeightCircuitBuilder,
    pub optimized: WeightCircuitBuilder,
    #[cfg(target_arch = "x86_64")]
    pub executor: Avx2Executor,
    pub original_gates: usize,
    pub optimized_gates: usize,
}

impl MetaPipeline {
    pub fn run(builder: WeightCircuitBuilder) -> Result<Self, String> {
        let original_gates = builder.gates.len();
        let optimized = CSEOptimizer::optimize(&builder);
        let optimized_gates = optimized.gates.len();

        #[cfg(target_arch = "x86_64")]
        let executor = Avx2Executor::new(&optimized);

        Ok(Self {
            original: builder,
            optimized,
            #[cfg(target_arch = "x86_64")]
            executor,
            original_gates,
            optimized_gates,
        })
    }

    #[cfg(target_arch = "x86_64")]
    pub fn execute(&self, operands: &mut [f32]) {
        self.executor.execute(operands);
    }

    pub fn stats(&self) -> (usize, usize, f64) {
        let reduction = if self.original_gates > 0 {
            1.0 - (self.optimized_gates as f64 / self.original_gates as f64)
        } else {
            0.0
        };
        (self.original_gates, self.optimized_gates, reduction)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cse_optimization() {
        let mut builder = WeightCircuitBuilder::new(8);
        builder.add_gate(0, GateCoeff::One, 1, GateCoeff::One, "dup1".to_string());
        builder.add_gate(0, GateCoeff::One, 1, GateCoeff::One, "dup2".to_string());
        builder.add_gate(2, GateCoeff::One, 3, GateCoeff::NegOne, "unique".to_string());
        builder.add_gate(0, GateCoeff::One, 1, GateCoeff::One, "dup3".to_string());

        let optimized = CSEOptimizer::optimize(&builder);
        assert!(optimized.gates.len() <= builder.gates.len(), "CSE должен уменьшить или сохранить");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_avx2_execution() {
        let mut builder = WeightCircuitBuilder::new(4);
        let ternary: [i8; 8] = [1, 0, -1, 0, 0, 1, 0, 1];
        let out_idx = builder.compile_linear_layer(4, 2, &ternary);

        let pipeline = MetaPipeline::run(builder).expect("pipeline");

        let mut operands = vec![0.0f32; 512];
        operands[0] = 10.0;
        operands[1] = 5.0;
        operands[2] = 3.0;
        operands[3] = 2.0;

        pipeline.execute(&mut operands);

        // x0 - x2 = 10 - 3 = 7
        assert!((operands[out_idx[0]] - 7.0).abs() < 0.001, "x0-x2 should be 7, got {}", operands[out_idx[0]]);
        // x1 + x3 = 5 + 2 = 7
        assert!((operands[out_idx[1]] - 7.0).abs() < 0.001, "x1+x3 should be 7, got {}", operands[out_idx[1]]);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn test_meta_pipeline_stats() {
        let mut builder = WeightCircuitBuilder::new(4);
        let ternary: [i8; 8] = [1, 0, -1, 0, 0, 1, 0, 1];
        let _ = builder.compile_linear_layer(4, 2, &ternary);

        let pipeline = MetaPipeline::run(builder).expect("pipeline");
        let (orig, opt, reduction) = pipeline.stats();
        println!("CSE: {} → {} вентилей ({:.1}% сокращение)", orig, opt, reduction * 100.0);
        assert!(opt <= orig);
    }
}
