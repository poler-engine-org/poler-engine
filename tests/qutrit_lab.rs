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


/// Вещественная часть комплексного вывода принтера («x + yi» → x).
fn re_part(st: &mut CalcState, expr: &str) -> f64 {
    let out = st
        .eval_line(expr)
        .unwrap_or_else(|e| panic!("{expr}: {e}"));
    let s = out.trim();
    let real = s.split(" + ").next().unwrap().split(" - ").next().unwrap();
    real.parse::<f64>()
        .unwrap_or_else(|_| panic!("{expr}: не число: '{out}'"))
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

// ---------------------------------------------------------------------------
// Блоки 12–15 (сессия 4): CGLMP-нарушение локального реализма, кутритный QAOA
// на живом коннектоме (Max-3-Cut, 3^6=729), t5c-мост «кристалл→квант»,
// квантовые прогулки с Loschmidt-эхом. Живые прогоны — download/experiments/.
// ---------------------------------------------------------------------------

/// Настройка CGLMP: |Φ₃⟩ + фазы (α₁=0, α₂=1/2, β₁=1/4, β₂=−1/4) + QFT с
/// взаимно сопряжённых сторон (F⊗F†) + проекторы классов (A−B) mod 3.
fn cglmp_setup(st: &mut CalcState) {
    for line in [
        "let psiB = (1/sqrt(3)) * [1;0;0;0;1;0;0;0;1]",
        // фазы измерений статьи Collins et al. 2002 (d=3)
        "let A2p = [1,0,0; 0,exp(pi/3*i),0; 0,0,exp(2*pi/3*i)]",
        "let B1p = [1,0,0; 0,exp(pi/6*i),0; 0,0,exp(pi/3*i)]",
        "let B2p = [1,0,0; 0,exp(-pi/6*i),0; 0,0,exp(-pi/3*i)]",
        // ВАЖНО (грабли сессии 4): F⊗F†, НЕ F⊗F — иначе разностные корреляции
        // состояния Белла размываются в плоские 1/3 и I3 = 0
        "let FF = kron(F3, dagger(F3))",
        "let s11 = FF * kron(eye(3), B1p) * psiB",
        "let s12 = FF * kron(eye(3), B2p) * psiB",
        "let s21 = FF * kron(A2p, B1p) * psiB",
        "let s22 = FF * kron(A2p, B2p) * psiB",
        // проекторы классов c = (A−B) mod 3
        "let Pc0 = kron(e0,e0)*transpose(kron(e0,e0)) + kron(e1,e1)*transpose(kron(e1,e1)) + kron(e2,e2)*transpose(kron(e2,e2))",
        "let Pcp = kron(e1,e0)*transpose(kron(e1,e0)) + kron(e2,e1)*transpose(kron(e2,e1)) + kron(e0,e2)*transpose(kron(e0,e2))",
        "let Pcm = kron(e2,e0)*transpose(kron(e2,e0)) + kron(e0,e1)*transpose(kron(e0,e1)) + kron(e1,e2)*transpose(kron(e1,e2))",
    ] {
        st.eval_line(line).unwrap_or_else(|e| panic!("{line}: {e}"));
    }
}

#[test]
fn cglmp_violation_of_local_realism() {
    let mut st = lab();
    cglmp_setup(&mut st);
    // q0 = P(A1=B1) = (4+2√3)/9 — вероятность «согласия» оптимальной пары
    near(
        re(&mut st, "trace(dagger(s11)*Pc0*s11)"),
        (4.0 + 2.0 * 3.0_f64.sqrt()) / 9.0,
        1e-12,
        "CGLMP q0 = (4+2√3)/9",
    );
    // q-1 = P(A1=B1−1) = 1/9
    near(
        re(&mut st, "trace(dagger(s11)*Pcm*s11)"),
        1.0 / 9.0,
        1e-12,
        "CGLMP q-1 = 1/9",
    );
    // СИММЕТРИЯ correlP статьи: 4 равенства вероятностей
    near(
        re(&mut st, "trace(dagger(s21)*Pcm*s21) - trace(dagger(s11)*Pc0*s11)"),
        0.0,
        1e-12,
        "P(B1=A2+1) = q0",
    );
    near(
        re(&mut st, "trace(dagger(s12)*Pcp*s12) - trace(dagger(s11)*Pcm*s11)"),
        0.0,
        1e-12,
        "P(B2=A1-1) = q-1",
    );
    // ГЛАВНОЕ: I3 = 4(q0 − q-1) = (4/9)(3+2√3) ≈ 2.872935 > 2 (локальный предел)
    let i3 = re(
        &mut st,
        "trace(dagger(s11)*Pc0*s11) + trace(dagger(s21)*Pcm*s21) + trace(dagger(s22)*Pc0*s22) + trace(dagger(s12)*Pc0*s12) - trace(dagger(s11)*Pcm*s11) - trace(dagger(s21)*Pc0*s21) - trace(dagger(s22)*Pcm*s22) - trace(dagger(s12)*Pcp*s12)",
    );
    near(i3, (4.0 / 9.0) * (3.0 + 2.0 * 3.0_f64.sqrt()), 1e-12, "I3(QM)");
    assert!(i3 > 2.0, "нарушение локального реализма: I3 = {i3} > 2");
    // Шумовой порог: rho(eta*) = eta|Фи><Фи| + (1-eta)I/9, I3(eta*) = 2 ровно.
    // Сторона сопряжения (грабли): trace(Pc*FF*r*FF†), НЕ trace(FF†*r*FF*Pc)
    st.eval_line("let rho = psiB*transpose(psiB)").unwrap();
    st.eval_line("let etas = 2/((4/9)*(3 + 2*sqrt(3)))").unwrap();
    st.eval_line("let rho1 = etas*rho + (1-etas)*(1/9)*eye(9)").unwrap();
    st.eval_line("let r11 = kron(eye(3),B1p)*rho1*dagger(kron(eye(3),B1p))").unwrap();
    st.eval_line("let r21 = kron(A2p,B1p)*rho1*dagger(kron(A2p,B1p))").unwrap();
    st.eval_line("let r22 = kron(A2p,B2p)*rho1*dagger(kron(A2p,B2p))").unwrap();
    st.eval_line("let r12 = kron(eye(3),B2p)*rho1*dagger(kron(eye(3),B2p))").unwrap();
    st.eval_line("let a11 = FF*r11*dagger(FF)").unwrap();
    st.eval_line("let a21 = FF*r21*dagger(FF)").unwrap();
    st.eval_line("let a22 = FF*r22*dagger(FF)").unwrap();
    st.eval_line("let a12 = FF*r12*dagger(FF)").unwrap();
    near(
        re_part(
            &mut st,
            "trace(Pc0*a11) + trace(Pcm*a21) + trace(Pc0*a22) + trace(Pc0*a12) - trace(Pcm*a11) - trace(Pc0*a21) - trace(Pcm*a22) - trace(Pcp*a12)",
        ),
        2.0,
        1e-10,
        "I3(eta*) = 2 — критическая видимость шума",
    );
}

/// Оптимум Ацина (сессия 6): НЕкомпактное состояние
/// |Ψ_mv⟩ = (|00⟩+γ|11⟩+|22⟩)/√(2+γ²), γ=(√11−√3)/2 ≈ 0.792287,
/// СО СТАНДАРТНЫМИ фазовыми лестницами CGLMP даёт точный максимум
/// I₃ = 1+√(11/3) ≈ 2.914854 — Acín-Durt-Gisin-Latorre (quant-ph/0111143).
/// DE-поиск сессии 6 переоткрыл это состояние независимо (γ = 0.792287
/// с 4 знаками) до сверки со статьёй. Четыре пути проверки: контур CGLMP,
/// оператор Белла == статье (позлементно), собственное уравнение, шум.
#[test]
fn acin_optimum_nonmaximally_entangled() {
    let mut st = lab();
    cglmp_setup(&mut st);
    for line in [
        "let gam = (sqrt(11)-sqrt(3))/2",
        "let nn = 2 + gam^2",
        "let mv = (1/sqrt(nn)) * [1;0;0;0;gam;0;0;0;1]",
        "let s11m = FF * kron(eye(3), B1p) * mv",
        "let s12m = FF * kron(eye(3), B2p) * mv",
        "let s21m = FF * kron(A2p, B1p) * mv",
        "let s22m = FF * kron(A2p, B2p) * mv",
        "let M11 = kron(eye(3), B1p)",
        "let M12 = kron(eye(3), B2p)",
        "let M21 = kron(A2p, B1p)",
        "let M22 = kron(A2p, B2p)",
        // оператор Белла из примитивов движка: B = Σ±(A⊗B)†·F F†·Pc·F F·(A⊗B)
        "let Bop = dagger(M11)*dagger(FF)*Pc0*FF*M11 + dagger(M21)*dagger(FF)*Pcm*FF*M21 + dagger(M22)*dagger(FF)*Pc0*FF*M22 + dagger(M12)*dagger(FF)*Pc0*FF*M12 - dagger(M11)*dagger(FF)*Pcm*FF*M11 - dagger(M21)*dagger(FF)*Pc0*FF*M21 - dagger(M22)*dagger(FF)*Pcm*FF*M22 - dagger(M12)*dagger(FF)*Pcp*FF*M12",
        // оператор Белла из статьи (quant-ph/0111143, Eq. bellop) — литерал
        "let Bpaper = [0,0,0,0,2/sqrt(3),0,0,0,2; 0,0,0,0,0,2/sqrt(3),0,0,0; 0,0,0,0,0,0,0,0,0; 0,0,0,0,0,0,0,2/sqrt(3),0; 2/sqrt(3),0,0,0,0,0,0,0,2/sqrt(3); 0,2/sqrt(3),0,0,0,0,0,0,0; 0,0,0,0,0,0,0,0,0; 0,0,0,2/sqrt(3),0,0,0,0,0; 2,0,0,0,2/sqrt(3),0,0,0,0]",
    ] {
        st.eval_line(line).unwrap_or_else(|e| panic!("{line}: {e}"));
    }
    // 1) контур CGLMP: I₃ = 1+√(11/3) — точный литературный максимум d=3
    let i3 = re(
        &mut st,
        "trace(dagger(s11m)*Pc0*s11m) + trace(dagger(s21m)*Pcm*s21m) + trace(dagger(s22m)*Pc0*s22m) + trace(dagger(s12m)*Pc0*s12m) - trace(dagger(s11m)*Pcm*s11m) - trace(dagger(s21m)*Pc0*s21m) - trace(dagger(s22m)*Pcm*s22m) - trace(dagger(s12m)*Pcp*s12m)",
    );
    near(i3, 1.0 + (11.0_f64 / 3.0).sqrt(), 1e-12, "I₃(Ацин) = 1+√(11/3)");
    // превышает и локальный предел (2), и компактный максимум Белла
    assert!(i3 > 2.0, "нарушение локального реализма: {i3} > 2");
    assert!(
        i3 > (4.0 / 9.0) * (3.0 + 2.0 * 3.0_f64.sqrt()),
        "некомпактное состояние бьёт компактный максимум 2.8729"
    );
    // 2) оператор Белла движка == оператору статьи позлементно
    near(
        zero_norm(&mut st, "Bop-Bpaper"),
        0.0,
        1e-24,
        "Bop == Bpaper (статья, Eq. bellop)",
    );
    // 3) Rayleigh и собственное уравнение: |Ψ_mv⟩ — собственный вектор B
    near(
        re_part(&mut st, "trace(dagger(mv)*Bop*mv)"),
        1.0 + (11.0_f64 / 3.0).sqrt(),
        1e-12,
        "⟨Ψ|B|Ψ⟩ = λ_max (Rayleigh)",
    );
    near(
        modulus(&mut st, "Bop*mv - (1+sqrt(11/3))*mv"),
        0.0,
        1e-12,
        "B·Ψ = λ·Ψ — собственный вектор (норма невязки)",
    );
    // 4) шумовой порог: η* = 2/(1+√(11/3)) → I₃(η*) = 2 ровно;
    //    η*(Ацин) < η*(Белл): некомпактное состояние ТОЛЕРАНТНЕЕ к шуму
    st.eval_line("let etam = 2/(1+sqrt(11/3))").unwrap();
    st.eval_line("let rhom = mv*transpose(mv)").unwrap();
    st.eval_line("let rhom1 = etam*rhom + (1-etam)*(1/9)*eye(9)").unwrap();
    for (nm, m) in [("r11m", "M11"), ("r12m", "M12"), ("r21m", "M21"), ("r22m", "M22")] {
        st.eval_line(&format!("let {nm} = {m}*rhom1*dagger({m})")).unwrap();
        st.eval_line(&format!("let a{nm} = FF*{nm}*dagger(FF)")).unwrap();
    }
    near(
        re_part(
            &mut st,
            "trace(Pc0*ar11m) + trace(Pcm*ar21m) + trace(Pc0*ar22m) + trace(Pc0*ar12m) - trace(Pcm*ar11m) - trace(Pc0*ar21m) - trace(Pcm*ar22m) - trace(Pcp*ar12m)",
        ),
        2.0,
        1e-10,
        "I₃(η*_Ацин) = 2 — критическая видимость",
    );
    near(
        re(&mut st, "2/(1+sqrt(11/3))"),
        2.0 / (1.0 + (11.0_f64 / 3.0).sqrt()),
        1e-12,
        "η* = 2/(1+√(11/3)) ≈ 0.686141 < η*(Белл) = 0.696152",
    );
}

/// Настройка QAOA: 6 кутритов K4-ядра коннектома мухи (реальные веса).
fn qaoa_setup(st: &mut CalcState) {
    for line in [
        "let I81 = kron(eye(9), eye(9))",
        "let flat = kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1],(1/sqrt(3))*[1;1;1])))))",
        // проекторы равенства EQU_uv (след 243 = ранг 3 в 729-мерии)
        "let EQU01 = kron(P0,kron(P0,I81)) + kron(P1,kron(P1,I81)) + kron(P2,kron(P2,I81))",
        "let EQU02 = kron(P0,kron(eye(3),kron(P0,eye(27)))) + kron(P1,kron(eye(3),kron(P1,eye(27)))) + kron(P2,kron(eye(3),kron(P2,eye(27))))",
        "let EQU04 = kron(P0,kron(eye(9),kron(P0,eye(9)))) + kron(P1,kron(eye(9),kron(P1,eye(9)))) + kron(P2,kron(eye(9),kron(P2,eye(9))))",
        "let EQU05 = kron(P0,kron(eye(27),kron(P0,eye(3)))) + kron(P1,kron(eye(27),kron(P1,eye(3)))) + kron(P2,kron(eye(27),kron(P2,eye(3))))",
        "let EQU12 = kron(eye(3),kron(P0,kron(P0,eye(27)))) + kron(eye(3),kron(P1,kron(P1,eye(27)))) + kron(eye(3),kron(P2,kron(P2,eye(27))))",
        "let EQU13 = kron(eye(3),kron(P0,kron(eye(3),kron(P0,eye(9))))) + kron(eye(3),kron(P1,kron(eye(3),kron(P1,eye(9))))) + kron(eye(3),kron(P2,kron(eye(3),kron(P2,eye(9)))))",
        "let EQU14 = kron(eye(3),kron(P0,kron(eye(9),kron(P0,eye(3))))) + kron(eye(3),kron(P1,kron(eye(9),kron(P1,eye(3))))) + kron(eye(3),kron(P2,kron(eye(9),kron(P2,eye(3)))))",
        "let EQU15 = kron(eye(3),kron(P0,kron(eye(27),P0))) + kron(eye(3),kron(P1,kron(eye(27),P1))) + kron(eye(3),kron(P2,kron(eye(27),P2)))",
        "let EQU24 = kron(eye(9),kron(P0,kron(eye(3),kron(P0,eye(3))))) + kron(eye(9),kron(P1,kron(eye(3),kron(P1,eye(3))))) + kron(eye(9),kron(P2,kron(eye(3),kron(P2,eye(3)))))",
        "let EQU25 = kron(eye(9),kron(P0,kron(eye(9),P0))) + kron(eye(9),kron(P1,kron(eye(9),P1))) + kron(eye(9),kron(P2,kron(eye(9),P2)))",
        "let EQU45 = kron(I81,kron(P0,P0)) + kron(I81,kron(P1,P1)) + kron(I81,kron(P2,P2))",
        // эрмитов миксер: X3 + X3† (грабли сессии 3: X† ≠ X)
        "let G3 = X3 + dagger(X3)",
    ] {
        st.eval_line(line).unwrap_or_else(|e| panic!("{line}: {e}"));
    }
}

#[test]
fn qutrit_qaoa_max3cut_on_fly_connectome() {
    let mut st = lab();
    qaoa_setup(&mut st);
    // (1) ранги проекторов: каждое EQU_uv — ранг 3 в 729-мерии (след 243)
    for e in ["EQU01", "EQU24", "EQU45"] {
        near(re(&mut st, &format!("trace({e})")), 243.0, 1e-9, "след EQU");
    }
    // (2) миксер унитарен: trace(M†M) = 3, det(M) = 1 (G3 бесследов)
    st.eval_line("let M1 = expm(-0.9*i*G3)").unwrap();
    near(re(&mut st, "trace(dagger(M1)*M1)"), 3.0, 1e-9, "миксер унитарен");
    near(re_part(&mut st, "det(M1)"), 1.0, 1e-9, "det миксера");
    // (3) оптимальное назначение x=16 (гаngи {3,4,5}|{0,2}|{1}) режет 4403 из 4435 —
    // индикатор [c_u≠c_v] = ceil(|c_u−c_v|/2); sign(0)=1 в Калькуляторе (грабли!)
    near(
        re(&mut st, "735*ceil(abs(1-0)/2) + 24*ceil(abs(1-1)/2) + 1496*ceil(abs(1-0)/2) + 1222*ceil(abs(1-0)/2) + 251*ceil(abs(2-1)/2) + 231*ceil(abs(2-0)/2) + 206*ceil(abs(2-0)/2) + 194*ceil(abs(2-0)/2) + 40*ceil(abs(1-0)/2) + 28*ceil(abs(1-0)/2) + 8*ceil(abs(0-0)/2)"),
        4403.0,
        1e-9,
        "Max-3-Cut оптимум 4403/4435 (x=16)",
    );
    // (4) QAOA p=1 при (γ,β)=(0.9,0.9): E[cut'] = 2.363073 (норм.), ~80% оптимума.
    // Линейная цепочка переприсвоек s = ... — НЕ вложенные выражения (иначе 2^11)
    st.eval_line("let s = flat").unwrap();
    for (w, n) in [
        (735.0 / 1496.0, "EQU01"), (24.0 / 1496.0, "EQU02"), (1.0, "EQU04"),
        (1222.0 / 1496.0, "EQU05"), (251.0 / 1496.0, "EQU12"), (231.0 / 1496.0, "EQU13"),
        (206.0 / 1496.0, "EQU14"), (194.0 / 1496.0, "EQU15"), (40.0 / 1496.0, "EQU24"),
        (28.0 / 1496.0, "EQU25"), (8.0 / 1496.0, "EQU45"),
    ] {
        st.eval_line(&format!("let s = exp(0.9*{w}*i)*s + (1 - exp(0.9*{w}*i))*({n}*s)")).unwrap();
    }
    st.eval_line("let s = kron(expm(-0.9*i*G3), kron(expm(-0.9*i*G3), kron(expm(-0.9*i*G3), kron(expm(-0.9*i*G3), kron(expm(-0.9*i*G3), expm(-0.9*i*G3))))))*s").unwrap();
    near(re(&mut st, "trace(dagger(s)*s)"), 1.0, 1e-9, "норма QAOA-состояния");
    let ec = [
        (735.0 / 1496.0, "EQU01"), (24.0 / 1496.0, "EQU02"), (1.0, "EQU04"),
        (1222.0 / 1496.0, "EQU05"), (251.0 / 1496.0, "EQU12"), (231.0 / 1496.0, "EQU13"),
        (206.0 / 1496.0, "EQU14"), (194.0 / 1496.0, "EQU15"), (40.0 / 1496.0, "EQU24"),
        (28.0 / 1496.0, "EQU25"), (8.0 / 1496.0, "EQU45"),
    ]
    .iter()
    .map(|(w, n)| format!("{w}*(1 - trace(dagger(s)*{n}*s))"))
    .collect::<Vec<_>>()
    .join(" + ");
    near(re(&mut st, &ec), 2.363073, 1e-4, "E[cut] QAOA p=1 (γ=β=0.9)");
}

#[test]
fn t5c_crystal_to_quantum_bridge() {
    let mut st = lab();
    // Живые триты строки «мысль» кристалла permanent_memory.t5c: [-1,1,-1,-1,-1,-1]
    // (захардкожены для герметичности теста; парсер — scripts/t5c_bridge_session.sh)
    for line in [
        "let F729 = kron(F3, kron(F3, kron(F3, kron(F3, kron(F3, F3)))))",
        "let flat6 = kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1], kron((1/sqrt(3))*[1;1;1],(1/sqrt(3))*[1;1;1])))))",
        // t=−1 → diag(1, ω², ω⁴); t=+1 → diag(1, ω, ω²) (грабли: t=1 — фаза om, не 1!)
        "let D1 = kron([1,0,0;0,om^2,0;0,0,om^4], kron([1,0,0;0,om,0;0,0,om^2], kron([1,0,0;0,om^2,0;0,0,om^4], kron([1,0,0;0,om^2,0;0,0,om^4], kron([1,0,0;0,om^2,0;0,0,om^4],[1,0,0;0,om^2,0;0,0,om^4])))))",
        "let D2 = kron([1,0,0;0,om,0;0,0,om^2], kron([1,0,0;0,om,0;0,0,om^2], kron([1,0,0;0,om^2,0;0,0,om^4], kron([1,0,0;0,om^2,0;0,0,om^4], kron([1,0,0;0,om^2,0;0,0,om^4],[1,0,0;0,om^2,0;0,0,om^4])))))",
        // декод: |−t mod 3⟩: «мысль» → |1,2,1,1,1,1⟩; «кристалл» → |2,2,1,1,1,1⟩
        "let dec1 = kron(e1, kron(e2, kron(e1, kron(e1, kron(e1, e1)))))",
        "let dec2 = kron(e2, kron(e2, kron(e1, kron(e1, kron(e1, e1)))))",
    ] {
        st.eval_line(line).unwrap_or_else(|e| panic!("{line}: {e}"));
    }
    // Мост без потерь: F·D·flat = |−t⟩ с машинной точностью
    near(
        zero_norm(&mut st, "F729*D1*flat6 - dec1"),
        0.0,
        1e-24,
        "t5c-мост «мысль»: F·D·flat = |−t⟩",
    );
    near(
        zero_norm(&mut st, "F729*D2*flat6 - dec2"),
        0.0,
        1e-24,
        "t5c-мост «кристалл»: F·D·flat = |−t⟩",
    );
    // Born-вероятность декода = 1
    near(
        modulus(&mut st, "trace(dagger(dec1)*(F729*D1*flat6))"),
        1.0,
        1e-10,
        "P(декод «мысль») = 1",
    );
    // Ортогональность двух мыслей
    near(
        modulus(&mut st, "trace(dagger(F729*D1*flat6)*(F729*D2*flat6))"),
        0.0,
        1e-10,
        "мысли ортогональны",
    );
    // Суперпозиция двух мыслей → Born-коллапс ровно 50/50
    st.eval_line("let sup = F729 * (D1*flat6 + D2*flat6)/sqrt(2)").unwrap();
    near(
        modulus(&mut st, "trace(dagger(dec1)*sup)"),
        1.0 / 2.0_f64.sqrt(),
        1e-10,
        "P(коллапс → «мысль») = 1/2",
    );
    near(
        modulus(&mut st, "trace(dagger(dec2)*sup)"),
        1.0 / 2.0_f64.sqrt(),
        1e-10,
        "P(коллапс → «кристалл») = 1/2",
    );
}

#[test]
fn quantum_walk_loschmidt_revival() {
    let mut st = lab();
    // K4-ядро коннектома мухи, взвешенная смежность /1496, ψ₀ = |хаб⟩
    st.eval_line("let Hf = (1/1496)*[-0,735,24,0,1496,1222; 735,-0,251,231,206,194; 24,251,-0,0,40,28; 0,231,0,-0,0,0; 1496,206,40,0,-0,8; 1222,194,28,0,8,-0]").unwrap();
    st.eval_line("let hub = [1;0;0;0;0;0]").unwrap();
    // унитарность эволюции: норма сохраняется
    for t in [0.5, 1.0, 2.5, 4.0] {
        near(
            re(&mut st, &format!("trace(dagger(schrodinger(Hf, hub, {t}))*schrodinger(Hf, hub, {t}))")),
            1.0,
            1e-9,
            "унитарность прогулки",
        );
    }
    // Сигнал ПОКИДАЕТ хаб (L(1.0) ≈ 0.038 < 0.1)…
    let l1 = re(&mut st, "abs(trace(dagger(hub)*schrodinger(Hf, hub, 1.0)))^2");
    assert!(l1 < 0.1, "L(1.0) = {l1} — сигнал должен почти уйти из хаба");
    // …и РЕВАЙВИТ: L(2.5) ≈ 0.830 > 0.8 — квантовая память сигнала
    let l25 = re(&mut st, "abs(trace(dagger(hub)*schrodinger(Hf, hub, 2.5)))^2");
    assert!(l25 > 0.8, "L(2.5) = {l25} — квантовый ревайвал в хабе");
    near(l25, 0.830, 0.01, "высота ревайвала");
}
