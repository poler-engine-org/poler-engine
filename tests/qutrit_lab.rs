//! Qutrit-лаборатория: троичная квантовая физика в Калькуляторе Всего.
//!
//! Открытия полевой сессии v0.63.0 (см. docs/UNDOCUMENTED.md, раздел
//! «Qutrit-лаборатория») переведены в регрессию: ω = e^(2πi/3), алгебра
//! Вейля clock/shift, Гелл-Манн su(3), QFT кутрита F3 и F3⊗F3, спин-1
//! (бозон) против кубита (фермион), фазовый вихрь, трит-мост и
//! квантовое блуждание по треугольнику.
//!
//! Сессия 3 добавляет: SUM-гейт и состояние Белла |Φ₃⁺⟩ (блок 9),
//! частичный след селекторными матрицами со смешанностью 1/3,
//! стабилизаторы и 3-цикл Беллов под X⊗X† (блок 10), QPE-трит-считывание
//! и 81-мерный трит-конвейер «19 = 1T01 → фазы → QFT-декод» (блок 11).
//!
//! Все проверки сведены к вещественным скалярам через abs/trace/det:
//! принтер Калькулятора печатает комплексные числа строкой, поэтому
//! тест оперирует только модулями, следами и нуль-нормами
//! trace(M†M) = Σ|m_ij|² (равна нулю тогда и только тогда, когда M = 0).

use poler_engine::calc::CalcState;

const SETUP: &[&str] = &[
    "let om = exp(2*pi/3*i)",
    "let Z3 = [1,0,0; 0,om,0; 0,0,om^2]",
    "let X3 = [0,0,1; 1,0,0; 0,1,0]",
    "let F3 = (1/sqrt(3)) * [1,1,1; 1,om,om^2; 1,om^2,om]",
    "let l1 = [0,1,0; 1,0,0; 0,0,0]",
    "let l2 = [0,-i,0; i,0,0; 0,0,0]",
    "let l3 = [1,0,0; 0,-1,0; 0,0,0]",
    "let l4 = [0,0,1; 0,0,0; 1,0,0]",
    "let l5 = [0,0,-i; 0,0,0; i,0,0]",
    "let l6 = [0,0,0; 0,0,1; 0,1,0]",
    "let l7 = [0,0,0; 0,0,-i; 0,i,0]",
    "let l8 = (1/sqrt(3)) * [1,0,0; 0,1,0; 0,0,-2]",
    "let Sx = (1/sqrt(2)) * [0,1,0; 1,0,1; 0,1,0]",
    "let Sy = (1/sqrt(2)) * [0,-i,0; i,0,-i; 0,i,0]",
    "let Sz = [1,0,0; 0,0,0; 0,0,-1]",
    "let N3 = [0,0,0; 0,1,0; 0,0,2]",
    "let psi0 = (1/sqrt(3)) * [1; om; om^2]",
    // Блоки 9–11 (сессия 3): SUM-гейт, частичный след, QPE-конвейер
    "let e0 = [1; 0; 0]",
    "let e1 = [0; 1; 0]",
    "let e2 = [0; 0; 1]",
    "let P0 = [1,0,0; 0,0,0; 0,0,0]",
    "let P1 = [0,0,0; 0,1,0; 0,0,0]",
    "let P2 = [0,0,0; 0,0,0; 0,0,1]",
    "let SUM = kron(P0, eye(3)) + kron(P1, X3) + kron(P2, X3*X3)",
];

fn lab() -> CalcState {
    let mut st = CalcState::new();
    for line in SETUP {
        st.eval_line(line)
            .unwrap_or_else(|e| panic!("{line}: {e}"));
    }
    st
}

/// Значение выражения как вещественный скаляр (принтер даёт «x.y»).
fn re(st: &mut CalcState, expr: &str) -> f64 {
    let out = st
        .eval_line(expr)
        .unwrap_or_else(|e| panic!("{expr}: {e}"));
    out.trim()
        .parse::<f64>()
        .unwrap_or_else(|_| panic!("{expr}: вывод не вещественный скаляр: '{out}'"))
}

/// Модуль комплексного результата: abs() Калькулятора возвращает f64.
fn modulus(st: &mut CalcState, expr: &str) -> f64 {
    re(st, &format!("abs({expr})"))
}

/// Нуль-тест матрицы/вектора: trace(M†M) = Σ|m_ij|² = 0 ⟺ M = 0.
fn zero_norm(st: &mut CalcState, expr: &str) -> f64 {
    re(st, &format!("trace(dagger({expr}) * ({expr}))"))
}

fn near(x: f64, target: f64, tol: f64, what: &str) {
    assert!(
        (x - target).abs() < tol,
        "{what}: получилось {x}, ожидалось {target} (допуск {tol})"
    );
}

// ---------------------------------------------------------------------------
// Блок 1. Фундамент: ω = e^(2πi/3)
// ---------------------------------------------------------------------------

#[test]
fn qutrit_omega_fundament() {
    let mut st = lab();
    near(modulus(&mut st, "om^3 - 1"), 0.0, 1e-12, "ω³ = 1");
    near(modulus(&mut st, "1 + om + om^2"), 0.0, 1e-12, "1 + ω + ω² = 0");
    near(modulus(&mut st, "om + om^2 + 1"), 0.0, 1e-12, "ω + ω² = −1");
    near(modulus(&mut st, "om"), 1.0, 1e-12, "|ω| = 1");
}

// ---------------------------------------------------------------------------
// Блок 2. Алгебра Вейля: Z·X = ω·X·Z и спектр сдвига
// ---------------------------------------------------------------------------

#[test]
fn qutrit_weyl_clock_shift() {
    let mut st = lab();
    // Соотношение Вейля (верная конвенция): нулевая матрица
    near(
        zero_norm(&mut st, "Z3*X3 - om*X3*Z3"),
        0.0,
        1e-26,
        "Weyl: Z·X = ω·X·Z",
    );
    // Негативный контроль: неверная конвенция (ω²) даёт норму O(1)
    let bad = zero_norm(&mut st, "Z3*X3 - om^2*X3*Z3");
    assert!(bad > 1.0, "негативный контроль: {bad} должно быть O(1)");
    // Фазовый обмен X·Z·X⁻¹·Z⁻¹ = ω²·I → trace = 3ω²
    near(
        modulus(&mut st, "trace(X3*Z3*inv(X3)*inv(Z3)) - 3*om^2"),
        0.0,
        1e-12,
        "X·Z·X⁻¹·Z⁻¹ = ω²·I",
    );
}

#[test]
fn qutrit_shift_spectrum_is_cube_roots() {
    let mut st = lab();
    // Спектр циклического сдвига = {1, ω, ω²}: det(X − λI) = 0 для корней
    near(
        modulus(&mut st, "det(X3 - eye(3))"),
        0.0,
        1e-10,
        "1 ∈ spec(X3)",
    );
    near(
        modulus(&mut st, "det(X3 - om*eye(3))"),
        0.0,
        1e-10,
        "ω ∈ spec(X3)",
    );
    near(
        modulus(&mut st, "det(X3 - om^2*eye(3))"),
        0.0,
        1e-10,
        "ω² ∈ spec(X3)",
    );
    // λ = 2 — не собственное значение: det(X−2I) = 7 точно
    let not_eig = modulus(&mut st, "det(X3 - 2*eye(3))");
    assert!(
        (not_eig - 7.0).abs() < 1.0,
        "det(X3 − 2·I) = {not_eig}, ожидалось 7"
    );
}

// ---------------------------------------------------------------------------
// Блок 3. QFT кутрита: F3 и двухкутритное F3⊗F3
// ---------------------------------------------------------------------------

#[test]
fn qutrit_qft_unitary_and_duality() {
    let mut st = lab();
    // Унитарность F3†F3 = I → trace = 3
    near(
        modulus(&mut st, "trace(dagger(F3)*F3) - 3"),
        0.0,
        1e-12,
        "F3†·F3 = I",
    );
    // Двойственность Вейля: QFT переводит сдвиг в часы
    near(
        zero_norm(&mut st, "F3*X3*dagger(F3) - Z3"),
        0.0,
        1e-24,
        "F3·X3·F3† = Z3",
    );
    // Двухкутритное QFT: F3⊗F3 унитарно в 9 измерениях
    st.eval_line("let F9 = kron(F3, F3)").unwrap();
    near(
        modulus(&mut st, "trace(dagger(F9)*F9) - 9"),
        0.0,
        1e-11,
        "(F3⊗F3)†·(F3⊗F3) = I₉",
    );
}

// ---------------------------------------------------------------------------
// Блок 4. Гелл-Манн su(3): структурные константы и Казимир
// ---------------------------------------------------------------------------

#[test]
fn qutrit_gell_mann_su3() {
    let mut st = lab();
    // Коммутаторы с точными структурными константами f_abc
    near(
        zero_norm(&mut st, "l1*l2 - l2*l1 - 2*i*l3"),
        0.0,
        1e-26,
        "[λ1,λ2] = 2i·λ3 (f123 = 1)",
    );
    near(
        zero_norm(&mut st, "l4*l5 - l5*l4 - i*(l3 + sqrt(3)*l8)"),
        0.0,
        1e-26,
        "[λ4,λ5] = i·(λ3 + √3·λ8)",
    );
    near(
        zero_norm(&mut st, "l6*l7 - l7*l6 - i*(-l3 + sqrt(3)*l8)"),
        0.0,
        1e-26,
        "[λ6,λ7] = i·(−λ3 + √3·λ8)",
    );
    // Антикоммутатор {λ1,λ2} = 0
    near(
        zero_norm(&mut st, "l1*l2 + l2*l1"),
        0.0,
        1e-26,
        "{λ1,λ2} = 0",
    );
    // Оператор Казимира: Σλa² = (16/3)·I → trace = 16
    near(
        modulus(
            &mut st,
            "trace(l1*l1 + l2*l2 + l3*l3 + l4*l4 + l5*l5 + l6*l6 + l7*l7 + l8*l8) - 16",
        ),
        0.0,
        1e-12,
        "Казимир Σλ² = (16/3)·I",
    );
    // expm(iθλ2) ∈ SU(3): унитарность и det = 1
    st.eval_line("let U = expm(i * 0.7 * l2)").unwrap();
    near(
        modulus(&mut st, "trace(dagger(U)*U) - 3"),
        0.0,
        1e-12,
        "U†·U = I",
    );
    near(modulus(&mut st, "det(U) - 1"), 0.0, 1e-9, "det U = 1");
}

// ---------------------------------------------------------------------------
// Блок 5. Спин-1: целый спин — бозон; кубит — фермион
// ---------------------------------------------------------------------------

#[test]
fn qutrit_spin1_boson_vs_qubit_fermion() {
    let mut st = lab();
    // Алгебра углового момента [Sx,Sy] = i·Sz
    near(
        zero_norm(&mut st, "Sx*Sy - Sy*Sx - i*Sz"),
        0.0,
        1e-26,
        "[Sx,Sy] = i·Sz",
    );
    // Полный оборот 2π: кутрит возвращается (trace = 3, бозон),
    // кубит набирает −1 (trace = −2, фермион)
    near(
        modulus(&mut st, "trace(expm(-2*pi*i*Sz)) - 3"),
        0.0,
        1e-9,
        "спин-1: R(2π) = +I",
    );
    near(
        modulus(&mut st, "trace(expm(-2*pi*i*0.5*pauli_z())) + 2"),
        0.0,
        1e-9,
        "спин-1/2: R(2π) = −I",
    );
    // На состояниях: e^{-i2πSz}|0⟩ = +|0⟩; e^{-i2π(σz/2)}|0⟩ = −|0⟩
    near(
        zero_norm(&mut st, "schrodinger(Sz, [1; 0; 0], 2*pi) - [1; 0; 0]"),
        0.0,
        1e-20,
        "кутрит: 2π → +ψ",
    );
    near(
        zero_norm(&mut st, "schrodinger(0.5*pauli_z(), [1; 0], 2*pi) + [1; 0]"),
        0.0,
        1e-20,
        "кубит: 2π → −ψ",
    );
}

// ---------------------------------------------------------------------------
// Блок 6. Фазовый вихрь ψ = (1, ω, ω²)/√3
// ---------------------------------------------------------------------------

#[test]
fn qutrit_phase_vortex() {
    let mut st = lab();
    st.eval_line("let flat = (1/sqrt(3)) * [1; 1; 1]").unwrap();
    // Вихрь — собственный вектор сдвига: X3·ψ = ω²·ψ («импульс»)
    near(
        zero_norm(&mut st, "X3*psi0 - om^2*psi0"),
        0.0,
        1e-26,
        "X3·ψ = ω²·ψ",
    );
    // Эволюция под оператором числа: t = 2π/3 раскручивает вихрь в плоскую волну
    near(
        zero_norm(&mut st, "schrodinger(N3, psi0, 2*pi/3) - flat"),
        0.0,
        1e-24,
        "ψ(2π/3) = (1,1,1)/√3",
    );
    // t = 2π: полный возврат вихря (период)
    near(
        zero_norm(&mut st, "schrodinger(N3, psi0, 2*pi) - psi0"),
        0.0,
        1e-24,
        "ψ(2π) = ψ0",
    );
    // QFT вихря локализует импульс: F3·ψ = |2⟩
    near(
        zero_norm(&mut st, "F3*psi0 - [0; 0; 1]"),
        0.0,
        1e-24,
        "F3·ψ = |2⟩",
    );
}

// ---------------------------------------------------------------------------
// Блок 7. Трит-мост: сбалансированная троичность × кубические фазы
// ---------------------------------------------------------------------------

#[test]
fn qutrit_trit_bridge() {
    let mut st = lab();
    // Сбалансированная троичная запись: 19 = 27 − 9 + 1 = «1T01»
    let s19 = st.eval_line("trits(19)").unwrap();
    assert!(
        s19.contains("1T01"),
        "trits(19) = '{s19}', ожидалась запись 1T01"
    );
    near(
        re(&mut st, "trit_val(trits(19))"),
        19.0,
        1e-12,
        "trit_val∘trits = id",
    );
    near(
        re(&mut st, "trit_val(trits(-19))"),
        -19.0,
        1e-12,
        "отрицательные числа в тритах",
    );
    // Троичная логика Клини: отрицание «1TT» (5) — это «T11»
    let not5 = st.eval_line("trit_not(trits(5))").unwrap();
    assert!(
        not5.contains("T11"),
        "trit_not(trits(5)) = '{not5}', ожидалось T11"
    );
    // Фаза несёт трит по модулю 3: ω^19 = ω (19 mod 3 = 1)
    near(modulus(&mut st, "om^19 - om"), 0.0, 1e-12, "ω^19 = ω");
    near(
        modulus(&mut st, "om^trit_val(trits(19)) - om"),
        0.0,
        1e-12,
        "ω^trit_val(trits(19)) = ω",
    );
    // Фазовое «сложение» тритов: ω^a·ω^b = ω^(a+b)
    near(
        modulus(&mut st, "om^7 * om^11 - om^(7+11)"),
        0.0,
        1e-12,
        "ω^a·ω^b = ω^(a+b)",
    );
}

// ---------------------------------------------------------------------------
// Блок 8. Квантовое блуждание по треугольнику: H = X + X† + Z + Z†
// ---------------------------------------------------------------------------

#[test]
fn qutrit_triangle_walk() {
    let mut st = lab();
    st.eval_line("let Hqw = X3 + dagger(X3) + Z3 + dagger(Z3)")
        .unwrap();
    // Эрмитовость гамильтониана — эволюция унитарна
    near(
        zero_norm(&mut st, "dagger(Hqw) - Hqw"),
        0.0,
        1e-26,
        "H = X+X†+Z+Z† эрмитов",
    );
    // Норма состояния сохраняется при блуждании
    near(
        modulus(
            &mut st,
            "trace(dagger(schrodinger(Hqw, [1; 0; 0], 1)) * schrodinger(Hqw, [1; 0; 0], 1)) - 1",
        ),
        0.0,
        1e-12,
        "|ψ(t)|² = 1 при блуждании",
    );
    // Спектр {−2, 1−√3, 1+√3}: след = 0, det = 4 — точно
    near(
        modulus(&mut st, "trace(Hqw)"),
        0.0,
        1e-12,
        "trace H = 0 (сумма спектра)",
    );
    near(
        modulus(&mut st, "det(Hqw) - 4"),
        0.0,
        1e-9,
        "det H = 4 (произведение спектра)",
    );
}

// ---------------------------------------------------------------------------
// Блок 9. SUM-гейт (кутритный CNOT) и состояние Белла |Φ₃⁺⟩
// ---------------------------------------------------------------------------

#[test]
fn qutrit_sum_gate_and_bell_pair() {
    let mut st = lab();
    // SUM = Σ_j P_j ⊗ X3^j — перестановочный гейт: SUM†·SUM = I₉
    near(
        modulus(&mut st, "trace(dagger(SUM)*SUM) - 9"),
        0.0,
        1e-12,
        "SUM†·SUM = I₉ (унитарность)",
    );
    // Квантовый сумматор по модулю 3: |j⟩|k⟩ ↦ |j⟩|(j+k) mod 3⟩
    near(
        zero_norm(&mut st, "SUM*kron(e1,e1) - kron(e1,e2)"),
        0.0,
        1e-26,
        "|1⟩|1⟩ → |1⟩|2⟩",
    );
    near(
        zero_norm(&mut st, "SUM*kron(e2,e2) - kron(e2,e1)"),
        0.0,
        1e-26,
        "|2⟩|2⟩ → |2⟩|1⟩",
    );
    near(
        zero_norm(&mut st, "SUM*kron(e2,e1) - kron(e2,e0)"),
        0.0,
        1e-26,
        "|2⟩|1⟩ → |2⟩|0⟩ (перенос по модулю)",
    );
    // Запутывание: SUM·((F3|0⟩)⊗|0⟩) = |Φ₃⁺⟩ = (|00⟩+|11⟩+|22⟩)/√3
    near(
        zero_norm(
            &mut st,
            "SUM*kron(F3*e0, e0) - (1/sqrt(3))*(kron(e0,e0) + kron(e1,e1) + kron(e2,e2))",
        ),
        0.0,
        1e-24,
        "SUM·(F3|0⟩⊗|0⟩) = |Φ₃⁺⟩",
    );
    // Плот-кодирование: (I⊗X^a)|Φ⟩ → SUM† → flat⊗|a⟩ — трит одной частицей
    near(
        zero_norm(
            &mut st,
            "dagger(SUM)*kron(eye(3), X3)*(SUM*kron(F3*e0,e0)) - kron((1/sqrt(3))*[1;1;1], e1)",
        ),
        0.0,
        1e-24,
        "dense coding: a=1 декодирован",
    );
    near(
        zero_norm(
            &mut st,
            "dagger(SUM)*kron(eye(3), X3*X3)*(SUM*kron(F3*e0,e0)) - kron((1/sqrt(3))*[1;1;1], e2)",
        ),
        0.0,
        1e-24,
        "dense coding: a=2 декодирован",
    );
}

// ---------------------------------------------------------------------------
// Блок 10. Частичный след, энтропия и стабилизаторы пары Белла
// ---------------------------------------------------------------------------

#[test]
fn qutrit_bell_partial_trace_and_stabilizers() {
    let mut st = lab();
    st.eval_line("let bell = SUM*kron(F3*e0, e0)").unwrap();
    st.eval_line("let rho = bell*dagger(bell)").unwrap();
    // Селекторные матрицы: S_m = I₃ ⊗ ⟨m| — частичный след без reshape
    st.eval_line("let S0 = kron(eye(3), [1,0,0])").unwrap();
    st.eval_line("let S1 = kron(eye(3), [0,1,0])").unwrap();
    st.eval_line("let S2 = kron(eye(3), [0,0,1])").unwrap();
    st.eval_line("let rhoA = S0*rho*dagger(S0) + S1*rho*dagger(S1) + S2*rho*dagger(S2)")
        .unwrap();
    // Tr_B(ρ) = I/3 — максимально смешанное состояние подсистемы
    near(
        zero_norm(&mut st, "rhoA - (1/3)*eye(3)"),
        0.0,
        1e-24,
        "Tr_B(ρ) = I/3",
    );
    // Чистоты: глобально чистое, локально максимально смешанное
    near(
        modulus(&mut st, "trace(rho*rho) - 1"),
        0.0,
        1e-12,
        "Tr(ρ²) = 1 (глобальная чистота)",
    );
    near(
        modulus(&mut st, "trace(rhoA*rhoA) - 1/3"),
        0.0,
        1e-12,
        "Tr(ρA²) = 1/3 (смешанность)",
    );
    // Спектр ρA = {1/3, 1/3, 1/3} ⟹ энтропия фон Неймана S = ln 3
    near(
        re(&mut st, "ln(3) + 3*(1/3)*ln(1/3)"),
        0.0,
        1e-12,
        "S = −Σλ·lnλ = ln 3 (λ = 1/3)",
    );
    // Стабилизаторы пары Белла: (Z⊗Z†) и (X⊗X) — у кутритов X† ≠ X!
    near(
        zero_norm(&mut st, "kron(Z3, dagger(Z3))*bell - bell"),
        0.0,
        1e-24,
        "(Z⊗Z†)|Φ₃⁺⟩ = |Φ₃⁺⟩",
    );
    near(
        zero_norm(&mut st, "kron(X3, X3)*bell - bell"),
        0.0,
        1e-24,
        "(X⊗X)|Φ₃⁺⟩ = |Φ₃⁺⟩",
    );
    // Негативный контроль: X⊗X† НЕ стабилизатор (3 нечётно, X† ≠ X)
    let bad = zero_norm(&mut st, "kron(X3, dagger(X3))*bell - bell");
    assert!(
        bad > 1.0,
        "негативный контроль X⊗X†: {bad} должно быть O(1)"
    );
    // Открытие сессии 3: X⊗X† вращает семейство Белла 3-циклом
    near(
        zero_norm(
            &mut st,
            "kron(X3,dagger(X3))*bell - (1/sqrt(3))*(kron(e0,e1) + kron(e1,e2) + kron(e2,e0))",
        ),
        0.0,
        1e-24,
        "W: |Φ₃⁺⟩ → |Ψ₀⟩ = (|01⟩+|12⟩+|20⟩)/√3",
    );
    near(
        zero_norm(
            &mut st,
            "kron(X3,dagger(X3))*(kron(X3,dagger(X3))*bell) - (1/sqrt(3))*(kron(e0,e2) + kron(e1,e0) + kron(e2,e1))",
        ),
        0.0,
        1e-24,
        "W: |Ψ₀⟩ → |Ψ₁⟩ = (|02⟩+|10⟩+|21⟩)/√3",
    );
    near(
        zero_norm(
            &mut st,
            "kron(X3,dagger(X3))*(kron(X3,dagger(X3))*(kron(X3,dagger(X3))*bell)) - bell",
        ),
        0.0,
        1e-24,
        "W³ = I: 3-цикл Беллов замкнут",
    );
    // Отрицательный контроль редукции: product-состояние остаётся чистым
    st.eval_line("let prod = kron((1/sqrt(3))*[1;1;1], (1/sqrt(3))*[1;1;1])")
        .unwrap();
    st.eval_line("let rhoP = prod*dagger(prod)").unwrap();
    st.eval_line(
        "let rhoPA = S0*rhoP*dagger(S0) + S1*rhoP*dagger(S1) + S2*rhoP*dagger(S2)",
    )
    .unwrap();
    near(
        modulus(&mut st, "trace(rhoPA*rhoPA) - 1"),
        0.0,
        1e-12,
        "product-состояние: редукция чистая (нет запутанности)",
    );
    near(
        zero_norm(&mut st, "rhoPA - (1/3)*[1;1;1]*dagger([1;1;1])"),
        0.0,
        1e-24,
        "Tr_B(ρP) = |flat⟩⟨flat|",
    );
}

// ---------------------------------------------------------------------------
// Блок 11. QPE-трит-считывание и 81-мерный трит-конвейер (19 = 1T01)
// ---------------------------------------------------------------------------

#[test]
fn qutrit_qpe_and_trit_lattice() {
    let mut st = lab();
    // Одно-кутритное QPE: |0⟩ → F3 → фаза ω^a → F3† ⇒ |a⟩ — точное считывание
    near(
        zero_norm(&mut st, "dagger(F3)*Z3*((1/sqrt(3))*[1;1;1]) - e1"),
        0.0,
        1e-26,
        "QPE: фаза ω¹ → трит |1⟩",
    );
    near(
        zero_norm(&mut st, "dagger(F3)*(Z3*Z3)*((1/sqrt(3))*[1;1;1]) - e2"),
        0.0,
        1e-26,
        "QPE: фаза ω² → трит |2⟩",
    );
    // Сбалансированный трит −1: ω^(−1) = ω² читается как |2⟩ ≡ −1 (mod 3)
    near(
        zero_norm(&mut st, "dagger(F3)*dagger(Z3)*((1/sqrt(3))*[1;1;1]) - e2"),
        0.0,
        1e-26,
        "QPE: трит −1 → |2⟩ (баланс без потерь)",
    );
    // 81-мерный конвейер: 19 = «1T01» → D4 = Z3⊗Z3†⊗I⊗Z3 → F81†·D4·flat81 = e46
    st.eval_line("let D4 = kron(Z3, kron(dagger(Z3), kron(eye(3), Z3)))")
        .unwrap();
    st.eval_line("let F81 = kron(kron(F3,F3), kron(F3,F3))").unwrap();
    st.eval_line(
        "let flat81 = kron(kron((1/sqrt(3))*[1;1;1],(1/sqrt(3))*[1;1;1]), kron((1/sqrt(3))*[1;1;1],(1/sqrt(3))*[1;1;1]))",
    )
    .unwrap();
    // e46: цифры (1,2,0,1)₃ = 27+18+0+1 = 46 — «1T01» с заменой 2↔T
    near(
        zero_norm(
            &mut st,
            "dagger(F81)*D4*flat81 - kron(e1, kron(e2, kron(e0, e1)))",
        ),
        0.0,
        1e-20,
        "19 = 1T01 → фазы → QFT-декод → e46 (детерминированно)",
    );
    // Born-вероятность правильного декода: |⟨e46|декод⟩|² = 1
    near(
        modulus(
            &mut st,
            "trace(dagger(kron(e1, kron(e2, kron(e0, e1))))*(dagger(F81)*D4*flat81))",
        ),
        1.0,
        1e-10,
        "|⟨e46|декод⟩| = 1 — вероятность 1",
    );
    // Круг замкнулся: трит-запись движка и квантовый декод согласованы
    near(
        re(&mut st, "trit_val(trits(19))"),
        19.0,
        1e-12,
        "round-trip: число → триты → фазы → QFT → число",
    );
}
