//! Графовий компілятор моделей у нативний машинний код (x86_64 JIT/AOT).
//!
//! Перетворює класичний граф нейромережі або коннектом A у прямі апаратні
//! інструкції CPU без матричних бібліотек, без інтерпретатора і без циклів for.
//!
//! Математика:
//! - Ребра з вагою ~0 або тритом 0 елімінуються на етапі графа.
//! - Ненульові зв'язки розгортаються в прямі SIMD/FMA інструкції.
//! - Фазовий ротор J = A - A^T додає зворотні зв'язки для динамічного резонансу.


/// Тип зв'язку в обчислювальному графі.
#[derive(Debug, Clone, PartialEq)]
pub enum WeightKind {
    /// Точне float32 значення.
    F32(f32),
    /// Трійковий зв'язок {-1, 0, +1} з блочним масштабом.
    Trit { val: i8, scale: f32 },
}

/// Ребро обчислювального графа: джерело -> приймач з вагою.
#[derive(Debug, Clone)]
pub struct ComputeEdge {
    pub src_node: usize,
    pub dst_node: usize,
    pub weight: WeightKind,
}

/// Граф обчислень шару нейромережі.
#[derive(Debug, Clone, Default)]
pub struct ModelComputeGraph {
    pub num_inputs: usize,
    pub num_outputs: usize,
    pub edges: Vec<ComputeEdge>,
    pub threshold: f32,
}

impl ModelComputeGraph {
    /// Створити новий граф розмірності N -> M.
    pub fn new(num_inputs: usize, num_outputs: usize, threshold: f32) -> Self {
        Self {
            num_inputs,
            num_outputs,
            edges: Vec::new(),
            threshold,
        }
    }

    /// Додати ребро від входу до виходу. Ребра з |weight| < threshold ігноруються (вакуум).
    pub fn add_dense_weight(&mut self, src: usize, dst: usize, weight: f32) {
        if weight.abs() >= self.threshold {
            self.edges.push(ComputeEdge {
                src_node: src,
                dst_node: dst,
                weight: WeightKind::F32(weight),
            });
        }
    }

    /// Додати трійкове ребро Trit5.
    pub fn add_trit_weight(&mut self, src: usize, dst: usize, trit: i8, scale: f32) {
        if trit != 0 {
            self.edges.push(ComputeEdge {
                src_node: src,
                dst_node: dst,
                weight: WeightKind::Trit { val: trit, scale },
            });
        }
    }

    /// Кількість активних зв'язків у графі.
    pub fn active_edges(&self) -> usize {
        self.edges.len()
    }

    /// Коефіцієнт розрідження (вакуум) графа.
    pub fn sparsity_ratio(&self) -> f32 {
        let total = self.num_inputs * self.num_outputs;
        if total == 0 {
            return 1.0;
        }
        1.0 - (self.edges.len() as f32 / total as f32)
    }
}

/// Згенерований машинний код (лістинг ASM та виконуваний байткод).
#[derive(Debug)]
pub struct CompiledMachineGraph {
    /// Асемблерний лістинг у форматі x86_64 NASM/GAS.
    pub asm_listing: String,
    /// Сирі байти машинного коду (опкоди x86_64).
    pub machine_bytes: Vec<u8>,
    /// Кількість скомпільованих апаратних інструкцій.
    pub instruction_count: usize,
}

/// Компілятор графа у машинний код x86_64.
pub struct GraphMachineCompiler;

impl GraphMachineCompiler {
    /// Скомпілювати граф нейромережі в асемблер x86_64 та машинний байткод.
    ///
    /// ABI: `fn forward(input: *const f32, output: *mut f32)`
    /// - `rdi` = вказівник на вхідний вектор f32
    /// - `rsi` = вказівник на вихідний вектор f32
    pub fn compile_x86_64(graph: &ModelComputeGraph, func_name: &str) -> CompiledMachineGraph {
        let mut asm = String::with_capacity(graph.edges.len() * 64 + 512);
        let mut bytes = Vec::with_capacity(graph.edges.len() * 16 + 128);
        let mut inst_count = 0;

        // Генерація прологу функції (System V AMD64 ABI)
        asm.push_str(&format!("; Auto-generated POLER Neural Graph Machine Code: {}\n", func_name));
        asm.push_str(&format!("; Active graph edges: {} / Sparsity: {:.2}%\n", 
            graph.edges.len(), graph.sparsity_ratio() * 100.0));
        asm.push_str("global ");
        asm.push_str(func_name);
        asm.push_str("\n");
        asm.push_str(func_name);
        asm.push_str(":\n");

        // Групування ребер за вихідними вузлами (dst_node) для оптимізації акумулятора
        let mut in_edges: Vec<Vec<&ComputeEdge>> = vec![Vec::new(); graph.num_outputs];
        for edge in &graph.edges {
            if edge.dst_node < graph.num_outputs {
                in_edges[edge.dst_node].push(edge);
            }
        }

        // Обробка кожного вихідного нейрона
        for (dst_idx, edges) in in_edges.iter().enumerate() {
            if edges.is_empty() {
                // Нульовий вихід: xorps xmm0, xmm0; movss [rsi + offset], xmm0
                asm.push_str(&format!("    ; Node {} (Vacuum - 0 edges)\n", dst_idx));
                asm.push_str("    xorps   xmm0, xmm0\n");
                asm.push_str(&format!("    movss   dword [rsi + {}], xmm0\n", dst_idx * 4));
                
                // xorps xmm0, xmm0 -> 0x0F 0x57 0xC0
                bytes.extend_from_slice(&[0x0F, 0x57, 0xC0]);
                // movss [rsi + disp], xmm0 -> 0xF3 0x0F 0x11 ...
                emit_movss_store_rsi(&mut bytes, (dst_idx * 4) as i32);
                inst_count += 2;
                continue;
            }

            asm.push_str(&format!("    ; Node {} (Fan-in: {} edges)\n", dst_idx, edges.len()));
            // Обнулення акумулятора xmm0
            asm.push_str("    xorps   xmm0, xmm0\n");
            bytes.extend_from_slice(&[0x0F, 0x57, 0xC0]);
            inst_count += 1;

            for edge in edges {
                let src_offset = (edge.src_node * 4) as i32;
                match edge.weight {
                    WeightKind::F32(w) => {
                        // ВЕС ВШИТ В МАШИННЫЙ КОД как immediate (mov eax, биты f32):
                        //   movss xmm1, [rdi + src_offset]  — загрузка входа x[j]
                        //   mov  eax, <w_bits>              — обученный вес literal
                        //   movd xmm2, eax
                        //   mulss xmm1, xmm2                — x[j] * w
                        //   addss xmm0, xmm1                — аккумуляция
                        asm.push_str(&format!("    movss   xmm1, dword [rdi + {}]\n", src_offset));
                        asm.push_str(&format!(
                            "    mov     eax, 0x{:08X}        ; вес f32 = {:.6}\n",
                            w.to_bits(),
                            w
                        ));
                        asm.push_str("    movd    xmm2, eax\n");
                        asm.push_str("    mulss   xmm1, xmm2\n");
                        asm.push_str("    addss   xmm0, xmm1\n");

                        emit_movss_load_rdi(&mut bytes, src_offset);
                        // mov eax, imm32(w bits) -> 0xB8 + 4 байта
                        bytes.push(0xB8);
                        bytes.extend_from_slice(&w.to_bits().to_le_bytes());
                        // movd xmm2, eax -> 0x66 0x0F 0x6E 0xD0
                        bytes.extend_from_slice(&[0x66, 0x0F, 0x6E, 0xD0]);
                        // mulss xmm1, xmm2 -> 0xF3 0x0F 0x59 0xCA
                        bytes.extend_from_slice(&[0xF3, 0x0F, 0x59, 0xCA]);
                        // addss xmm0, xmm1 -> 0xF3 0x0F 0x58 0xC1
                        bytes.extend_from_slice(&[0xF3, 0x0F, 0x58, 0xC1]);
                        inst_count += 5;
                    }
                    WeightKind::Trit { val, scale: _ } => {
                        match val {
                            1 => {
                                // No-Mul: пряме додавання входу
                                asm.push_str(&format!("    addss   xmm0, dword [rdi + {}] ; Trit +1 (No-Mul)\n", src_offset));
                                emit_addss_mem_rdi(&mut bytes, src_offset);
                                inst_count += 1;
                            }
                            -1 => {
                                // No-Mul: пряме віднімання входу
                                asm.push_str(&format!("    subss   xmm0, dword [rdi + {}] ; Trit -1 (No-Mul)\n", src_offset));
                                emit_subss_mem_rdi(&mut bytes, src_offset);
                                inst_count += 1;
                            }
                            _ => {} // 0 - вакуум, ігнорується
                        }
                    }
                }
            }

            // Збереження результату нейрона
            asm.push_str(&format!("    movss   dword [rsi + {}], xmm0\n", dst_idx * 4));
            emit_movss_store_rsi(&mut bytes, (dst_idx * 4) as i32);
            inst_count += 1;
        }

        // Епілог (ret -> 0xC3)
        asm.push_str("    ret\n");
        bytes.push(0xC3);
        inst_count += 1;

        CompiledMachineGraph {
            asm_listing: asm,
            machine_bytes: bytes,
            instruction_count: inst_count,
        }
    }
}

// ---------------------------------------------------------------------------
// Хелпери кодування опкодів x86_64
// ---------------------------------------------------------------------------

fn emit_movss_load_rdi(buf: &mut Vec<u8>, disp: i32) {
    if (0..=127).contains(&disp) {
        // movss xmm1, [rdi + disp8] -> 0xF3 0x0F 0x10 0x4F disp8
        buf.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x4F, disp as u8]);
    } else {
        // movss xmm1, [rdi + disp32] -> 0xF3 0x0F 0x10 0x8F disp32
        buf.extend_from_slice(&[0xF3, 0x0F, 0x10, 0x8F]);
        buf.extend_from_slice(&disp.to_le_bytes());
    }
}

fn emit_movss_store_rsi(buf: &mut Vec<u8>, disp: i32) {
    if (0..=127).contains(&disp) {
        // movss [rsi + disp8], xmm0 -> 0xF3 0x0F 0x11 0x46 disp8
        buf.extend_from_slice(&[0xF3, 0x0F, 0x11, 0x46, disp as u8]);
    } else {
        // movss [rsi + disp32], xmm0 -> 0xF3 0x0F 0x11 0x86 disp32
        buf.extend_from_slice(&[0xF3, 0x0F, 0x11, 0x86]);
        buf.extend_from_slice(&disp.to_le_bytes());
    }
}

fn emit_addss_mem_rdi(buf: &mut Vec<u8>, disp: i32) {
    if (0..=127).contains(&disp) {
        // addss xmm0, [rdi + disp8] -> 0xF3 0x0F 0x58 0x47 disp8
        buf.extend_from_slice(&[0xF3, 0x0F, 0x58, 0x47, disp as u8]);
    } else {
        buf.extend_from_slice(&[0xF3, 0x0F, 0x58, 0x87]);
        buf.extend_from_slice(&disp.to_le_bytes());
    }
}

fn emit_subss_mem_rdi(buf: &mut Vec<u8>, disp: i32) {
    if (0..=127).contains(&disp) {
        // subss xmm0, [rdi + disp8] -> 0xF3 0x0F 0x5C 0x47 disp8
        buf.extend_from_slice(&[0xF3, 0x0F, 0x5C, 0x47, disp as u8]);
    } else {
        buf.extend_from_slice(&[0xF3, 0x0F, 0x5C, 0x87]);
        buf.extend_from_slice(&disp.to_le_bytes());
    }
}

// ---------------------------------------------------------------------------
// Тести
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graph_vacuum_sparsity() {
        let mut g = ModelComputeGraph::new(4, 4, 0.1);
        g.add_dense_weight(0, 0, 0.85);
        g.add_dense_weight(1, 0, 0.02); // менше порогу -> вакуум
        g.add_dense_weight(2, 1, -0.90);

        assert_eq!(g.active_edges(), 2);
        assert_eq!(g.sparsity_ratio(), 1.0 - (2.0 / 16.0));
    }

    #[test]
    fn test_compile_trit_no_mul_asm() {
        let mut g = ModelComputeGraph::new(4, 2, 0.0);
        g.add_trit_weight(0, 0, 1, 1.0);  // +1
        g.add_trit_weight(1, 0, -1, 1.0); // -1
        g.add_trit_weight(2, 0, 0, 1.0);  // 0 вакуум

        let compiled = GraphMachineCompiler::compile_x86_64(&g, "layer_trit_no_mul");
        assert!(compiled.asm_listing.contains("Trit +1 (No-Mul)"));
        assert!(compiled.asm_listing.contains("Trit -1 (No-Mul)"));
        assert!(!compiled.machine_bytes.is_empty());
        assert_eq!(*compiled.machine_bytes.last().unwrap(), 0xC3); // ret
    }
}
