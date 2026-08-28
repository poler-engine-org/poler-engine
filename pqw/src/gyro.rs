//! Гироскопная топология формата v3 (`POLER_Q3`): кососимметричный
//! резонансный оператор `J = A − Aᵀ` в топологической секции контейнера.
//!
//! ## Физика секции
//!
//! Направленный поток смысла между координатами `i → j` накапливается
//! знаковыми вкладами наблюдений `A[i][j] += s_i·s_j`; антисимметричная
//! часть `J = A − Aᵀ` отделяет **циркуляцию** (направленный перенос
//! смысла) от симметричной ассоциации. Кососимметричная `J` порождает
//! унитарную прецессию `ψ ← e^(−iηJ)ψ`: норма памяти сохраняется —
//! гироскоп не забывает, а вращает фазы (Im(P) — фазовые векторы смысла).
//!
//! Хранится только верхний треугольник `i < j`: `J[j][i] = −J[i][j]`
//! восстанавливается по кососимметрии. LENS-философия сохранена: пары с
//! |J| ниже квантовой решётки (|q| < 1) не хранятся вовсе.
//!
//! ## Раскладка секции (little-endian)
//!
//! ```text
//! 0x00  4   magic "GYRO"
//! 0x04  4   версия секции (u32, = 1)
//! 0x08  4   окно W — дальность направленного контекста (u32)
//! 0x0C  4   число пар N (u32)
//! 0x10  8   счётчик тактов t — всего наблюдений (u64)
//! 0x18  4   масштаб квантования scale (f32): max|J| ↔ 127
//! 0x1C  4   reserved (u32, = 0)
//! 0x20  ·   N записей по 5 B (u16-индексы) или 9 B (u32):
//!           arc_i (LE), arc_j (LE), weight q (i8, ±1..±127)
//! ```
//!
//! Вес восстанавливается как `J = q · scale / 127`. Пары строго
//! возрастают по `(i, j)` — детерминизм сериализации. Счётчик тактов
//! дублируется в reserved-слове заголовка v3 (0x70) и сверяется при
//! чтении.

use crate::error::{PqwError, Result};

/// Магические байты гироскопной секции.
pub const GYRO_MAGIC: [u8; 4] = *b"GYRO";
/// Версия раскладки секции, поддерживаемая этой сборкой.
pub const GYRO_SECTION_VERSION: u32 = 1;
/// Размер служебного заголовка секции.
pub const GYRO_HEADER_SIZE: usize = 0x20;
/// Целевая амплитуда квантования: max |J| ↔ 127.
pub const GYRO_QUANT_MAX: f64 = 127.0;

/// Одна хранимая пара гироскопа: `i < j`, деквантованный вес `J[i][j]`.
///
/// Симметричный элемент `J[j][i] = −weight`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GyroPair {
    pub i: u32,
    pub j: u32,
    /// Деквантованный вес `J[i][j] = q · scale / 127`.
    pub weight: f64,
}

/// Полные данные гироскопа для записи в контейнер v3.
///
/// Пары обязаны быть отсортированы по `(i, j)` и уникальны —
/// [`GyroData::new`] сортирует и отвергает дубликаты.
#[derive(Clone, Debug, PartialEq)]
pub struct GyroData {
    window: u32,
    ticks: u64,
    pairs: Vec<(u32, u32, f64)>,
}

impl GyroData {
    /// Конструкция из сырых пар: сортировка по `(i, j)`, валидация
    /// диапазонов (`i < j < d_pol`), конечности и ненулевых весов.
    ///
    /// Нулевые веса и пары легче полкванта решётки (`|w| < scale/254`,
    /// где scale = max|w|) выбрасываются: их квант q = 0 не кодируется
    /// декодером (LENS: циркуляция ниже квантовой решётки не хранится).
    pub fn new(
        window: u32,
        ticks: u64,
        mut pairs: Vec<(u32, u32, f64)>,
        d_pol: u32,
    ) -> Result<GyroData> {
        pairs.retain(|&(_, _, w)| w.is_finite() && w != 0.0);
        pairs.sort_unstable_by(|a, b| (a.0, a.1).cmp(&(b.0, b.1)));
        for w in pairs.windows(2) {
            if (w[0].0, w[0].1) == (w[1].0, w[1].1) {
                return Err(PqwError::DuplicateIndex(w[1].0));
            }
        }
        for &(i, j, _) in &pairs {
            if i >= j {
                return Err(PqwError::BadIndex { index: i, d_pol });
            }
            if j >= d_pol {
                return Err(PqwError::BadIndex { index: j, d_pol });
            }
        }
        // Пары ниже полкванта дали бы q = 0 — отбрасываем до проверки пустоты.
        let max = pairs
            .iter()
            .map(|&(_, _, w)| w.abs())
            .fold(0.0_f64, f64::max);
        if max > 0.0 {
            let half_quant = max / (2.0 * GYRO_QUANT_MAX);
            pairs.retain(|&(_, _, w)| w.abs() >= half_quant);
        }
        if pairs.is_empty() {
            return Err(PqwError::Layout(
                "gyro section requires at least one pair (use v2 container for empty gyro)",
            ));
        }
        Ok(GyroData {
            window,
            ticks,
            pairs,
        })
    }

    /// Окно направленного контекста W.
    pub fn window(&self) -> u32 {
        self.window
    }

    /// Счётчик тактов (всего наблюдений потока).
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Пары верхнего треугольника `(i, j, J[i][j])`, отсортированы.
    pub fn pairs(&self) -> &[(u32, u32, f64)] {
        &self.pairs
    }

    /// Масштаб квантования: max |J| по хранимым парам.
    pub fn scale(&self) -> f32 {
        let max = self
            .pairs
            .iter()
            .map(|&(_, _, w)| w.abs())
            .fold(0.0_f64, f64::max);
        if max > 0.0 {
            max as f32
        } else {
            1.0
        }
    }

    /// Кодирование секции: 32 B служебных + N × (2w+1) B записей,
    /// где w = 2 при `index16` (u16-индексы), иначе 4.
    pub(crate) fn encode(&self, index16: bool) -> Result<Vec<u8>> {
        let scale = self.scale();
        if !scale.is_finite() || scale <= 0.0 {
            return Err(PqwError::BadValue(scale));
        }
        let record = if index16 { 5usize } else { 9 };
        let mut out = Vec::with_capacity(GYRO_HEADER_SIZE + self.pairs.len() * record);
        out.extend_from_slice(&GYRO_MAGIC);
        out.extend_from_slice(&GYRO_SECTION_VERSION.to_le_bytes());
        out.extend_from_slice(&self.window.to_le_bytes());
        out.extend_from_slice(&(self.pairs.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.ticks.to_le_bytes());
        out.extend_from_slice(&scale.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        for &(i, j, w) in &self.pairs {
            if index16 {
                debug_assert!(i <= u16::MAX as u32 && j <= u16::MAX as u32);
                out.extend_from_slice(&(i as u16).to_le_bytes());
                out.extend_from_slice(&(j as u16).to_le_bytes());
            } else {
                out.extend_from_slice(&i.to_le_bytes());
                out.extend_from_slice(&j.to_le_bytes());
            }
            // Квантование: q = round(w / scale · 127), зажатый в ±127.
            // |q| < 1 → 0 → пара не попадает в контейнер (выше отфильтрованы).
            let q = (w / scale as f64 * GYRO_QUANT_MAX).round();
            let q = q.clamp(-GYRO_QUANT_MAX, GYRO_QUANT_MAX) as i8;
            out.push(q as u8);
        }
        Ok(out)
    }
}

/// Разобранная гироскопная секция контейнера v3 (zero-copy не критична:
/// пары деквантуются в плотный Vec — их сотни, не миллионы).
#[derive(Clone, Debug, PartialEq)]
pub struct GyroSection {
    window: u32,
    ticks: u64,
    scale: f32,
    index16: bool,
    pairs: Vec<GyroPair>,
}

impl GyroSection {
    /// Полный разбор и валидация секции поверх среза.
    ///
    /// Проверяются: magic, версия, точная длина, конечный масштаб,
    /// диапазон `i < j < d_pol`, строгое возрастание `(i, j)`,
    /// `|q| ∈ [1, 127]`.
    pub fn decode(bytes: &[u8], d_pol: u32, index16: bool) -> Result<GyroSection> {
        if bytes.len() < GYRO_HEADER_SIZE {
            return Err(PqwError::Truncated {
                need: GYRO_HEADER_SIZE,
                have: bytes.len(),
            });
        }
        if bytes[..4] != GYRO_MAGIC {
            return Err(PqwError::Layout("gyro section: bad magic"));
        }
        let version = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        if version != GYRO_SECTION_VERSION {
            return Err(PqwError::UnsupportedVersion(version));
        }
        let window = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        let count = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
        let ticks = u64::from_le_bytes(bytes[16..24].try_into().unwrap());
        let scale = f32::from_le_bytes(bytes[24..28].try_into().unwrap());
        let reserved = u32::from_le_bytes(bytes[28..32].try_into().unwrap());
        if reserved != 0 {
            return Err(PqwError::ReservedBits {
                value: reserved as u64,
            });
        }
        if !scale.is_finite() || scale <= 0.0 {
            return Err(PqwError::BadValue(scale));
        }
        let record = if index16 { 5usize } else { 9 };
        let expected = GYRO_HEADER_SIZE + count * record;
        if bytes.len() != expected {
            return Err(PqwError::Layout(
                "gyro section: length does not match pair count",
            ));
        }

        let mut pairs: Vec<GyroPair> = Vec::with_capacity(count);
        let mut body = &bytes[GYRO_HEADER_SIZE..];
        for _ in 0..count {
            let (i, j, q, rest) = if index16 {
                let i = u16::from_le_bytes([body[0], body[1]]) as u32;
                let j = u16::from_le_bytes([body[2], body[3]]) as u32;
                let q = body[4] as i8;
                (i, j, q, &body[5..])
            } else {
                let i = u32::from_le_bytes(body[0..4].try_into().unwrap());
                let j = u32::from_le_bytes(body[4..8].try_into().unwrap());
                let q = body[8] as i8;
                (i, j, q, &body[9..])
            };
            body = rest;
            if i >= j || j >= d_pol {
                return Err(PqwError::BadIndex { index: j, d_pol });
            }
            if q == 0 {
                return Err(PqwError::Layout("gyro section: zero weight quant"));
            }
            if let Some(prev) = pairs.last() {
                if (prev.i, prev.j) >= (i, j) {
                    return Err(PqwError::UnsortedTopology);
                }
            }
            pairs.push(GyroPair {
                i,
                j,
                weight: q as f64 * scale as f64 / GYRO_QUANT_MAX,
            });
        }
        Ok(GyroSection {
            window,
            ticks,
            scale,
            index16,
            pairs,
        })
    }

    /// Окно направленного контекста W.
    pub fn window(&self) -> u32 {
        self.window
    }

    /// Счётчик тактов.
    pub fn ticks(&self) -> u64 {
        self.ticks
    }

    /// Масштаб квантования весов.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Ширина индексов пар (true → u16).
    pub fn index16(&self) -> bool {
        self.index16
    }

    /// Деквантованные пары верхнего треугольника.
    pub fn pairs(&self) -> &[GyroPair] {
        &self.pairs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_data() -> GyroData {
        // Несортированный вход — конструктор обязан отсортировать.
        GyroData::new(
            256,
            10_000,
            vec![(40, 50, 3.0), (1, 9, -5.0), (1, 9, 0.0), (2, 7, 1.0)],
            4096,
        )
        .unwrap()
    }

    #[test]
    fn data_sorts_and_drops_zero() {
        let d = sample_data();
        // Нулевой вес (1,9,0.0) отброшен; пары отсортированы.
        assert_eq!(
            d.pairs(),
            &[(1u32, 9u32, -5.0), (2, 7, 1.0), (40, 50, 3.0)]
        );
        assert_eq!(d.window(), 256);
        assert_eq!(d.ticks(), 10_000);
        assert!((d.scale() - 5.0).abs() < 1e-6);
    }

    #[test]
    fn data_rejects_bad_pairs() {
        // i >= j
        assert!(GyroData::new(1, 1, vec![(9, 1, 1.0)], 16).is_err());
        // j >= d_pol
        assert!(GyroData::new(1, 1, vec![(1, 16, 1.0)], 16).is_err());
        // дубликат пары
        assert!(GyroData::new(1, 1, vec![(1, 2, 1.0), (1, 2, 2.0)], 16).is_err());
        // пустой гироскоп → v3 невозможен
        assert!(GyroData::new(1, 1, vec![(1, 2, 0.0)], 16).is_err());
        // NaN
        assert!(GyroData::new(1, 1, vec![(1, 2, f64::NAN)], 16).is_err());
    }

    #[test]
    fn data_drops_subquant_pairs() {
        // Пара легче полкванта (5.0/254 ≈ 0.0197) выбрасывается:
        // её q = 0 не легален в секции. (Одиночная пара не отбрасывается
        // никогда: scale = её собственный вес → q = 127.)
        let d = GyroData::new(1, 1, vec![(1, 2, 5.0), (3, 4, 0.001)], 16).unwrap();
        assert_eq!(d.pairs(), &[(1u32, 2u32, 5.0)]);
    }

    #[test]
    fn section_roundtrip_u16() {
        let d = sample_data();
        let bytes = d.encode(true).unwrap();
        assert_eq!(bytes.len(), GYRO_HEADER_SIZE + 3 * 5);
        let s = GyroSection::decode(&bytes, 4096, true).unwrap();
        assert_eq!(s.window(), 256);
        assert_eq!(s.ticks(), 10_000);
        assert_eq!(s.pairs().len(), 3);
        // Деквантованные веса: q = round(w/scale·127) → граница ошибки
        // scale/(2·127) (полкванта решётки).
        let tol = 5.0 / GYRO_QUANT_MAX / 2.0 + 1e-12;
        for (p, &(i, j, w)) in s.pairs().iter().zip(d.pairs()) {
            assert_eq!((p.i, p.j), (i, j));
            let err = (p.weight - w).abs();
            assert!(err <= tol, "pair ({i},{j}): err {err} > {tol}");
        }
        // Знаки сохранены.
        assert!(s.pairs()[0].weight < 0.0);
        assert!(s.pairs()[1].weight > 0.0);
    }

    #[test]
    fn section_roundtrip_u32() {
        let d = GyroData::new(
            64,
            1,
            vec![(70_000, 80_000, 2.5), (65_536, 70_001, -1.0)],
            1_000_000,
        )
        .unwrap();
        let bytes = d.encode(false).unwrap();
        assert_eq!(bytes.len(), GYRO_HEADER_SIZE + 2 * 9);
        let s = GyroSection::decode(&bytes, 1_000_000, false).unwrap();
        assert_eq!(s.pairs().len(), 2);
        assert_eq!(s.pairs()[0].i, 65_536);
        assert_eq!(s.pairs()[1].j, 80_000);
        assert!((s.pairs()[1].weight - 2.5).abs() <= 2.5 / GYRO_QUANT_MAX / 2.0 + 1e-12);
    }

    #[test]
    fn decode_rejects_corruption() {
        let bytes = sample_data().encode(true).unwrap();
        // Короткий буфер.
        assert!(GyroSection::decode(&bytes[..31], 4096, true).is_err());
        // Плохой magic.
        let mut b = bytes.clone();
        b[0] = b'X';
        assert!(GyroSection::decode(&b, 4096, true).is_err());
        // Плохая версия секции.
        let mut b = bytes.clone();
        b[4] = 2;
        assert!(GyroSection::decode(&b, 4096, true).is_err());
        // Ломаем счётчик пар.
        let mut b = bytes.clone();
        b[12] = 99;
        assert!(GyroSection::decode(&b, 4096, true).is_err());
        // Нулевой масштаб.
        let mut b = bytes.clone();
        b[24..28].copy_from_slice(&0f32.to_le_bytes());
        assert!(GyroSection::decode(&b, 4096, true).is_err());
        // Ненулевое reserved.
        let mut b = bytes.clone();
        b[28] = 1;
        assert!(GyroSection::decode(&b, 4096, true).is_err());
        // Плохой индекс: j >= d_pol.
        let mut b = bytes.clone();
        b[GYRO_HEADER_SIZE + 2..GYRO_HEADER_SIZE + 4].copy_from_slice(&9999u16.to_le_bytes());
        assert!(GyroSection::decode(&b, 4096, true).is_err());
        // Нулевой квант веса.
        let mut b = bytes.clone();
        b[GYRO_HEADER_SIZE + 4] = 0;
        assert!(GyroSection::decode(&b, 4096, true).is_err());
        // Несортированные пары: первая (i,j) больше второй.
        let mut b = bytes.clone();
        b[GYRO_HEADER_SIZE] = 30; // первая пара i=30 > второй (2,7)
        b[GYRO_HEADER_SIZE + 1] = 0;
        assert!(GyroSection::decode(&b, 4096, true).is_err());
    }
}
