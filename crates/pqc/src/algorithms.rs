//! # Алгоритмы для квантового ПК POLER
//!
//! Эталоны, на которых физические машины теряют точность, а идеальный
//! субстрат — нет: QFT (фазы π/2^k), Grover (оракул применён к вектору
//! состояния точно, без декомпозиции в T-гейты), Bernstein–Vazirani,
//! Deutsch–Jozsa, GHZ. Все схемы — [`Circuit`] с QCASM-семантикой.

use crate::error::{PqcError, Result};
use crate::gates::Gate;
use crate::qpc::{Circuit, Op};

/// Пара Белла (|00⟩ + |11⟩)/√2.
pub fn bell() -> Result<Circuit> {
    let mut c = Circuit::new(2)?;
    c.gate(Gate::H { q: 0 })
        .gate(Gate::Cx {
            control: 0,
            target: 1,
        })
        .push(Op::MeasureAll);
    Ok(c)
}

/// GHZ-состояние (|0…0⟩ + |1…1⟩)/√2 на n кубитах.
pub fn ghz(n: usize) -> Result<Circuit> {
    if n == 0 {
        return Err(PqcError::EmptyState);
    }
    let mut c = Circuit::new(n)?;
    c.gate(Gate::H { q: 0 });
    for q in 1..n {
        c.gate(Gate::Cx {
            control: q - 1,
            target: q,
        });
    }
    c.push(Op::MeasureAll);
    Ok(c)
}

/// Квантовое преобразование Фурье на n кубитах.
///
/// Прямая схема (Nielsen & Chuang / qiskit): для j от старшего к младшему —
/// H(j), затем CP(k, j, π/2^{j−k}) для k < j по убыванию. Финальные
/// SWAPs бит-реверса опущены (регистр живёт в переставленной конвенции —
/// стандартная практика; паритет с qiskit верифицируется на той же
/// раскладке гейтов). Обратное QFT — точное дзеркало: обратный порядок
/// операций с инвертированными фазами (H и перестановки самодвойственны).
pub fn qft(n: usize, inverse: bool) -> Result<Circuit> {
    if n == 0 {
        return Err(PqcError::EmptyState);
    }
    let mut ops: Vec<Op> = Vec::new();
    for j in (0..n).rev() {
        ops.push(Op::Gate(Gate::H { q: j }));
        for k in (0..j).rev() {
            let angle = core::f64::consts::PI / (1u64 << (j - k)) as f64;
            ops.push(Op::Gate(Gate::Cp {
                control: k,
                target: j,
                theta: angle,
            }));
        }
    }
    if inverse {
        ops = ops
            .into_iter()
            .rev()
            .map(|op| match op {
                Op::Gate(Gate::Cp {
                    control,
                    target,
                    theta,
                }) => Op::Gate(Gate::Cp {
                    control,
                    target,
                    theta: -theta,
                }),
                other => other,
            })
            .collect();
    }
    let mut c = Circuit::new(n)?;
    for op in ops {
        c.push(op);
    }
    Ok(c)
}

/// Гровер: поиск помеченных состояний. Оракул — точная инверсия фазы
/// (`flipphase` по маске каждого помеченного состояния): идеальная машина
/// не платит за синтез оракула из T-гейтов — привилегия владельца
/// вектора состояния. Диффузор — H^n · (I − 2|0…0⟩⟨0…0|) · H^n
/// (глобальный знак на итерацию отброшен как ненаблюдаемый).
///
/// Возвращает схему и число итераций R = ⌊π/4·√(2^n/M)⌋.
pub fn grover(n: usize, marks: &[usize]) -> Result<(Circuit, usize)> {
    if n == 0 || marks.is_empty() {
        return Err(PqcError::BadArgument {
            what: "grover needs n >= 1 and at least one marked state".into(),
        });
    }
    let dim = 1usize << n;
    for &m in marks {
        if m >= dim {
            return Err(PqcError::BadArgument {
                what: format!("marked state {m} exceeds {n} qubits"),
            });
        }
    }
    let m = marks.len() as f64;
    let r = ((core::f64::consts::PI / 4.0) * (dim as f64 / m).sqrt()).floor() as usize;
    let r = r.max(1);

    let mut c = Circuit::new(n)?;
    for q in 0..n {
        c.gate(Gate::H { q });
    }
    for _ in 0..r {
        // Оракул: точная инверсия фазы помеченных состояний.
        for &mark in marks {
            c.push(Op::FlipIndex { idx: mark });
        }
        // Диффузор: H^n (I − 2|0⟩⟨0|) H^n.
        for q in 0..n {
            c.gate(Gate::H { q });
        }
        c.push(Op::FlipZero);
        for q in 0..n {
            c.gate(Gate::H { q });
        }
    }
    c.push(Op::MeasureAll);
    Ok((c, r))
}

/// Bernstein–Vazirani: восстановление секретного вектора s через
/// f(x) = s·x mod 2. n кубитов данных + 1 анцилла (последний кубит).
pub fn bernstein_vazirani(n: usize, secret: u64) -> Result<Circuit> {
    if n == 0 || n >= 64 {
        return Err(PqcError::BadArgument {
            what: "bernstein-vazirani needs 1..=63 data qubits".into(),
        });
    }
    if secret >> n != 0 {
        return Err(PqcError::BadArgument {
            what: format!("secret {secret} exceeds {n} bits"),
        });
    }
    let total = n + 1;
    let anc = n; // последний кубит
    let mut c = Circuit::new(total)?;
    c.gate(Gate::X { q: anc });
    for q in 0..total {
        c.gate(Gate::H { q });
    }
    // Оракул U_f: |x⟩|y⟩ → |x⟩|y ⊕ s·x⟩ — CNOT от каждого бита секрета.
    for k in 0..n {
        if secret >> k & 1 == 1 {
            c.gate(Gate::Cx {
                control: k,
                target: anc,
            });
        }
    }
    for q in 0..n {
        c.gate(Gate::H { q });
    }
    // Измеряем только биты данных.
    for q in 0..n {
        c.push(Op::Measure { q });
    }
    Ok(c)
}

/// Deutsch–Jozsa: константность/сбалансированность f: {0,1}^n → {0,1}
/// за один вызов оракула. Оракул задаётся масками помеченных состояний
/// (f(x) = 1 ⟺ x ∈ marks): фазовая инверсия — точно.
pub fn deutsch_jozsa(n: usize, marks: &[usize]) -> Result<Circuit> {
    if n == 0 {
        return Err(PqcError::EmptyState);
    }
    let dim = 1usize << n;
    for &m in marks {
        if m >= dim {
            return Err(PqcError::BadArgument {
                what: format!("marked state {m} exceeds {n} qubits"),
            });
        }
    }
    let mut c = Circuit::new(n)?;
    for q in 0..n {
        c.gate(Gate::H { q });
    }
    for &m in marks {
        c.push(Op::FlipIndex { idx: m });
    }
    for q in 0..n {
        c.gate(Gate::H { q });
    }
    c.push(Op::MeasureAll);
    Ok(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qpc::run;

    const TOL: f64 = 1e-10;

    #[test]
    fn qft_of_zero_is_uniform() {
        let c = qft(4, false).unwrap();
        // Без измерений — смотрим вероятности через run(shots=0).
        let rep = run(&c, 0, 1).unwrap();
        for (i, &p) in rep.probabilities.iter().enumerate() {
            assert!(
                (p - 1.0 / 16.0).abs() < TOL,
                "P[{i}] = {p}, expected 1/16"
            );
        }
    }

    #[test]
    fn qft_then_iqft_is_identity() {
        let mut c = qft(5, false).unwrap();
        let inv = qft(5, true).unwrap();
        for op in inv.ops() {
            c.push(op.clone());
        }
        let rep = run(&c, 0, 1).unwrap();
        assert!((rep.probabilities[0] - 1.0).abs() < 1e-9, "back to |0>");
        for (i, &p) in rep.probabilities.iter().enumerate().skip(1) {
            assert!(p < 1e-9, "P[{i}] = {p} must vanish");
        }
        assert!((rep.norm - 1.0).abs() < 1e-12);
    }

    #[test]
    fn qft_phase_structure_on_single_bit() {
        // |1⟩ (x=1) через QFT(2): амплитуды — четвертные корни из −1 / 2,
        // фазы {0, π/2, π, −π/2} в переставленной (бит-реверсной) конвенции.
        let mut c = Circuit::new(2).unwrap();
        c.gate(Gate::X { q: 0 });
        let f = qft(2, false).unwrap();
        for op in f.ops() {
            c.push(op.clone());
        }
        let rep = run(&c, 0, 1).unwrap();
        let a = rep.final_state.amplitudes();
        let mut phases: Vec<f64> = Vec::new();
        for amp in a {
            assert!((amp.norm() - 0.5).abs() < TOL, "uniform magnitude");
            phases.push(amp.im.atan2(amp.re));
        }
        phases.sort_by(|x, y| x.partial_cmp(y).unwrap());
        let expected = [
            -core::f64::consts::FRAC_PI_2,
            0.0,
            core::f64::consts::FRAC_PI_2,
            core::f64::consts::PI,
        ];
        for (got, &want) in phases.iter().zip(expected.iter()) {
            assert!((got - want).abs() < 1e-9, "phases {phases:?}");
        }
    }

    #[test]
    fn grover_finds_marked_state() {
        // 5 кубитов, одно помеченное состояние 0b10110 = 22.
        let (c, r) = grover(5, &[22]).unwrap();
        let rep = run(&c, 0, 1).unwrap();
        let p = rep.probabilities[22];
        assert!(p > 0.95, "P(marked) = {p} after {r} iterations");
    }

    #[test]
    fn grover_two_marks() {
        let (c, _) = grover(6, &[9, 40]).unwrap();
        let rep = run(&c, 0, 1).unwrap();
        let p = rep.probabilities[9] + rep.probabilities[40];
        assert!(p > 0.9, "P(marks) = {p}");
    }

    #[test]
    fn bv_recovers_secret() {
        let secret = 0b1011u64;
        let c = bernstein_vazirani(4, secret).unwrap();
        let rep = run(&c, 0, 1).unwrap();
        // Исход: биты данных несут секрет (анцилла не измерялась, но после
        // H на данных исход детерминирован). Крайний случай: исход с младшими
        // 4 битами = secret и любым значением анциллы.
        let mut best = 0.0f64;
        for (i, &p) in rep.probabilities.iter().enumerate() {
            if i & 0xF == secret as usize {
                best += p;
            }
        }
        assert!(best > 0.999, "P(secret) = {best}");
    }

    #[test]
    fn dj_constant_and_balanced() {
        // Константная f = 0 (нет меток): исход |0…0⟩ с вероятностью 1.
        let c0 = deutsch_jozsa(4, &[]).unwrap();
        let r0 = run(&c0, 0, 1).unwrap();
        assert!((r0.probabilities[0] - 1.0).abs() < TOL);

        // Сбалансированная f = младший бит: 8 нулей из 16.
        let marks: Vec<usize> = (0..16usize).filter(|x| x & 1 == 1).collect();
        let c1 = deutsch_jozsa(4, &marks).unwrap();
        let r1 = run(&c1, 0, 1).unwrap();
        assert!(r1.probabilities[0] < 1e-10, "balanced never gives |0>");
    }
}
