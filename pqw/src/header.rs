//! Фиксированный заголовок (0x00..0x80, 128 байт) — контракт формата v1.
//!
//! ```text
//! 0x00  8   magic "POLER_QW"            0x40  8   topology_offset
//! 0x08  4   format_version (u32)        0x48  8   topology_len
//! 0x0C  4   d_pol (u32)                 0x50  8   phase_offset
//! 0x10  4   eta (f32)                   0x58  8   phase_len
//! 0x14  4   gamma (f32)                 0x60  8   nnz
//! 0x18  4   rho (f32)                   0x68  8   flags
//! 0x1C  4   epsilon_threshold (f32)     0x70  8   reserved (0)
//! 0x20  8   mcweeny_residual (f64)      0x78  8   header checksum (FNV-1a64)
//! 0x28  24  payload digest (SHA-256→24)
//! ```
//!
//! Все числа — little-endian.

use crate::checksum::fnv1a64;
use crate::error::{PqwError, Result};

/// Магические байты контейнера.
pub const MAGIC: [u8; 8] = *b"POLER_QW";
/// Версия формата, поддерживаемая этой сборкой.
pub const FORMAT_VERSION: u32 = 1;
/// Размер фиксированного заголовка.
pub const HEADER_SIZE: usize = 0x80;

// Точные смещения полей.
pub const OFF_VERSION: usize = 0x08;
pub const OFF_D_POL: usize = 0x0C;
pub const OFF_ETA: usize = 0x10;
pub const OFF_GAMMA: usize = 0x14;
pub const OFF_RHO: usize = 0x18;
pub const OFF_EPS: usize = 0x1C;
pub const OFF_MCWEENY_RESIDUAL: usize = 0x20;
pub const OFF_DIGEST: usize = 0x28;
pub const DIGEST_LEN: usize = 24;
pub const OFF_TOPOLOGY_OFFSET: usize = 0x40;
pub const OFF_TOPOLOGY_LEN: usize = 0x48;
pub const OFF_PHASE_OFFSET: usize = 0x50;
pub const OFF_PHASE_LEN: usize = 0x58;
pub const OFF_NNZ: usize = 0x60;
pub const OFF_FLAGS: usize = 0x68;
pub const OFF_RESERVED: usize = 0x70;
pub const OFF_CHECKSUM: usize = 0x78;

/// Гиперпараметры фазового потока `dp/dt = −η·Π_Λ(∇F − γ∇ε)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HyperParams {
    /// Шаг потока (learning rate).
    pub eta: f32,
    /// Трение потока.
    pub gamma: f32,
    /// Затухание IIR-резонатора `R_t = ε_t + ρ·R_{t−1}`.
    pub rho: f32,
    /// Порог ε-значимости: LENS отсекает дуги с |p| < ε.
    pub epsilon_threshold: f32,
}

impl Default for HyperParams {
    fn default() -> Self {
        HyperParams {
            eta: 0.01,
            gamma: 0.1,
            rho: 0.99,
            epsilon_threshold: 0.05,
        }
    }
}

impl HyperParams {
    /// Все четыре значения конечны.
    pub fn is_finite(&self) -> bool {
        self.eta.is_finite()
            && self.gamma.is_finite()
            && self.rho.is_finite()
            && self.epsilon_threshold.is_finite()
    }
}

/// Флаги формата: v1 определяет два младших бита, остальные обязаны быть нулями.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Flags(u64);

impl Flags {
    /// Топология хранит u16-индексы (допустимо при `d_pol ≤ 65536`).
    pub const INDEX16: u64 = 1 << 0;
    /// Фазовые блоки содержат 6-битную кривизну. v1: бит обязан быть установлен.
    pub const CURVATURE: u64 = 1 << 1;

    /// Флаги для записи: CURVATURE всегда; INDEX16 по выбору писателя.
    pub fn new(index16: bool) -> Flags {
        Flags(Self::CURVATURE | if index16 { Self::INDEX16 } else { 0 })
    }

    /// u16-топология?
    pub fn index16(self) -> bool {
        self.0 & Self::INDEX16 != 0
    }

    /// Кривизна присутствует?
    pub fn curvature(self) -> bool {
        self.0 & Self::CURVATURE != 0
    }

    /// Ширина индекса топологии в байтах (2 или 4).
    pub fn index_width(self) -> usize {
        if self.index16() {
            2
        } else {
            4
        }
    }

    /// Проверка зарезервированных битов и обязательного бита CURVATURE.
    pub fn validate(v: u64) -> Result<Flags> {
        if v & !(Self::INDEX16 | Self::CURVATURE) != 0 {
            return Err(PqwError::ReservedBits { value: v });
        }
        if v & Self::CURVATURE == 0 {
            return Err(PqwError::UnsupportedFlags(v));
        }
        Ok(Flags(v))
    }

    /// Сырые биты.
    pub fn bits(self) -> u64 {
        self.0
    }
}

/// Полностью разобранный фиксированный заголовок.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub format_version: u32,
    pub d_pol: u32,
    pub hyper: HyperParams,
    /// max `|λ² − λ|` по хранимым дугам — отклонение от P² = P на момент записи.
    pub mcweeny_residual: f64,
    /// Первые 24 байта SHA-256 от payload (топология + фазовые блоки).
    pub payload_digest: [u8; DIGEST_LEN],
    pub topology_offset: u64,
    pub topology_len: u64,
    pub phase_offset: u64,
    pub phase_len: u64,
    pub nnz: u64,
    pub flags: Flags,
}

#[inline]
fn put_u32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn get_u32(buf: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(buf[off..off + 4].try_into().unwrap())
}

#[inline]
fn put_u64(buf: &mut [u8], off: usize, v: u64) {
    buf[off..off + 8].copy_from_slice(&v.to_le_bytes());
}

#[inline]
fn get_u64(buf: &[u8], off: usize) -> u64 {
    u64::from_le_bytes(buf[off..off + 8].try_into().unwrap())
}

#[inline]
fn put_f32(buf: &mut [u8], off: usize, v: f32) {
    put_u32(buf, off, v.to_bits());
}

#[inline]
fn get_f32(buf: &[u8], off: usize) -> f32 {
    f32::from_bits(get_u32(buf, off))
}

#[inline]
fn put_f64(buf: &mut [u8], off: usize, v: f64) {
    put_u64(buf, off, v.to_bits());
}

#[inline]
fn get_f64(buf: &[u8], off: usize) -> f64 {
    f64::from_bits(get_u64(buf, off))
}

impl Header {
    /// Сериализация в 128 байт; checksum вычисляется последним.
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut b = [0u8; HEADER_SIZE];
        b[..8].copy_from_slice(&MAGIC);
        put_u32(&mut b, OFF_VERSION, self.format_version);
        put_u32(&mut b, OFF_D_POL, self.d_pol);
        put_f32(&mut b, OFF_ETA, self.hyper.eta);
        put_f32(&mut b, OFF_GAMMA, self.hyper.gamma);
        put_f32(&mut b, OFF_RHO, self.hyper.rho);
        put_f32(&mut b, OFF_EPS, self.hyper.epsilon_threshold);
        put_f64(&mut b, OFF_MCWEENY_RESIDUAL, self.mcweeny_residual);
        b[OFF_DIGEST..OFF_DIGEST + DIGEST_LEN].copy_from_slice(&self.payload_digest);
        put_u64(&mut b, OFF_TOPOLOGY_OFFSET, self.topology_offset);
        put_u64(&mut b, OFF_TOPOLOGY_LEN, self.topology_len);
        put_u64(&mut b, OFF_PHASE_OFFSET, self.phase_offset);
        put_u64(&mut b, OFF_PHASE_LEN, self.phase_len);
        put_u64(&mut b, OFF_NNZ, self.nnz);
        put_u64(&mut b, OFF_FLAGS, self.flags.bits());
        // OFF_RESERVED остаётся нулевым
        let checksum = fnv1a64(&b[..OFF_CHECKSUM]);
        put_u64(&mut b, OFF_CHECKSUM, checksum);
        b
    }

    /// Разбор и проверка: magic → version → checksum → flags/reserved → d_pol.
    pub fn from_bytes(data: &[u8]) -> Result<Header> {
        if data.len() < HEADER_SIZE {
            return Err(PqwError::Truncated {
                need: HEADER_SIZE,
                have: data.len(),
            });
        }
        let mut magic = [0u8; 8];
        magic.copy_from_slice(&data[..8]);
        if magic != MAGIC {
            return Err(PqwError::BadMagic(magic));
        }
        let version = get_u32(data, OFF_VERSION);
        if version != FORMAT_VERSION {
            return Err(PqwError::UnsupportedVersion(version));
        }
        let expected = get_u64(data, OFF_CHECKSUM);
        let actual = fnv1a64(&data[..OFF_CHECKSUM]);
        if expected != actual {
            return Err(PqwError::CorruptHeader { expected, actual });
        }
        let flags = Flags::validate(get_u64(data, OFF_FLAGS))?;
        let reserved = get_u64(data, OFF_RESERVED);
        if reserved != 0 {
            return Err(PqwError::ReservedBits { value: reserved });
        }
        let d_pol = get_u32(data, OFF_D_POL);
        if d_pol == 0 {
            return Err(PqwError::BadDimension(d_pol));
        }
        let mut payload_digest = [0u8; DIGEST_LEN];
        payload_digest.copy_from_slice(&data[OFF_DIGEST..OFF_DIGEST + DIGEST_LEN]);
        Ok(Header {
            format_version: version,
            d_pol,
            hyper: HyperParams {
                eta: get_f32(data, OFF_ETA),
                gamma: get_f32(data, OFF_GAMMA),
                rho: get_f32(data, OFF_RHO),
                epsilon_threshold: get_f32(data, OFF_EPS),
            },
            mcweeny_residual: get_f64(data, OFF_MCWEENY_RESIDUAL),
            payload_digest,
            topology_offset: get_u64(data, OFF_TOPOLOGY_OFFSET),
            topology_len: get_u64(data, OFF_TOPOLOGY_LEN),
            phase_offset: get_u64(data, OFF_PHASE_OFFSET),
            phase_len: get_u64(data, OFF_PHASE_LEN),
            nnz: get_u64(data, OFF_NNZ),
            flags,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Header {
        Header {
            format_version: FORMAT_VERSION,
            d_pol: 8,
            hyper: HyperParams {
                eta: 0.5,
                gamma: 0.25,
                rho: 0.75,
                epsilon_threshold: 0.125,
            },
            mcweeny_residual: 0.25,
            payload_digest: [7u8; 24],
            topology_offset: 0x80,
            topology_len: 6,
            phase_offset: 0x86,
            phase_len: 3,
            nnz: 3,
            flags: Flags::new(true),
        }
    }

    #[test]
    fn header_roundtrip() {
        let h = sample();
        let b = h.to_bytes();
        assert_eq!(b.len(), HEADER_SIZE);
        let h2 = Header::from_bytes(&b).unwrap();
        assert_eq!(h2, h);
    }

    #[test]
    fn checksum_detects_mutation() {
        let mut b = sample().to_bytes();
        b[0x0C] ^= 0x01; // d_pol без пересчёта checksum
        assert!(matches!(
            Header::from_bytes(&b),
            Err(PqwError::CorruptHeader { .. })
        ));
    }

    #[test]
    fn bad_magic_rejected() {
        let mut b = sample().to_bytes();
        b[0] = b'X';
        assert!(matches!(Header::from_bytes(&b), Err(PqwError::BadMagic(_))));
    }

    #[test]
    fn short_buffer_rejected() {
        let b = sample().to_bytes();
        assert!(matches!(
            Header::from_bytes(&b[..127]),
            Err(PqwError::Truncated { .. })
        ));
    }
}
