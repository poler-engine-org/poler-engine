//! Ошибки вычислительного ядра `pqc`.

use core::fmt;

/// Результат операций ядра.
pub type Result<T> = core::result::Result<T, PqcError>;

/// Ошибки statevector-движка, анзаца и моста к контейнеру `.pqw`.
#[derive(Debug)]
pub enum PqcError {
    /// Пустое состояние: ноль кубитов / ноль дуг / d_pol = 0.
    EmptyState,
    /// Больше кубитов, чем допускает движок (2^n амплитуд).
    TooManyQubits { requested: usize, max: usize },
    /// Индекс кубита за пределами [0, n_qubits).
    BadQubit { q: usize, n_qubits: usize },
    /// Контроль и цель двухкубитного гейта совпадают.
    SameQubit { control: usize, target: usize },
    /// Фаза вне [−1, 1] или NaN.
    BadPhase(f64),
    /// Born-правило определено только для нормированного состояния.
    NotNormalized { norm: f64 },
    /// Индекс дуги вне [0, d_pol).
    BadArc { index: u32, d_pol: u32 },
    /// Дуги не отсортированы по индексу (или дублируются).
    UnsortedArcs,
    /// Ошибка контейнера `.poler` / `.pqw`.
    Pqw(pqw::PqwError),
}

impl fmt::Display for PqcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PqcError::EmptyState => write!(f, "empty state: at least one qubit required"),
            PqcError::TooManyQubits { requested, max } => {
                write!(f, "too many qubits: {requested} > {max} (2^n amplitudes)")
            }
            PqcError::BadQubit { q, n_qubits } => {
                write!(f, "qubit index {q} out of range [0, {n_qubits})")
            }
            PqcError::SameQubit { control, target } => {
                write!(f, "control and target coincide: {control} == {target}")
            }
            PqcError::BadPhase(p) => write!(f, "phase out of [-1, 1] or NaN: {p}"),
            PqcError::NotNormalized { norm } => {
                write!(
                    f,
                    "state is not normalized: |psi| = {norm}, Born rule requires 1"
                )
            }
            PqcError::BadArc { index, d_pol } => {
                write!(f, "arc index {index} out of range [0, {d_pol})")
            }
            PqcError::UnsortedArcs => write!(f, "arcs must be strictly sorted by index"),
            PqcError::Pqw(e) => write!(f, "pqw container: {e}"),
        }
    }
}

impl std::error::Error for PqcError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PqcError::Pqw(e) => Some(e),
            _ => None,
        }
    }
}

impl From<pqw::PqwError> for PqcError {
    fn from(e: pqw::PqwError) -> PqcError {
        PqcError::Pqw(e)
    }
}
