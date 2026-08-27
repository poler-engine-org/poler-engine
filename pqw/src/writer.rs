//! Сериализатор: сборка контейнера `.poler` / `.pqw` из разреженного
//! фазового состояния.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;

use crate::error::{PqwError, Result};
use crate::header::{Flags, Header, HyperParams, FORMAT_VERSION, HEADER_SIZE};
use crate::mcweeny;
use crate::phase::{quantize, PhaseByte};
use crate::sha256::sha256_trunc24;
use crate::topology;

/// Писатель `.poler` / `.pqw`.
///
/// ```
/// use pqw::PqwWriter;
///
/// let bytes = PqwWriter::new(8)?
///     .hyperparams(0.5, 0.25, 0.75, 0.125)
///     .add_phase(1, -0.9)?
///     .to_bytes()?;
/// assert_eq!(bytes.len(), 0x80 + 2 + 1); // заголовок + u16-индекс + фазовый байт
/// # Ok::<(), pqw::PqwError>(())
/// ```
pub struct PqwWriter {
    d_pol: u32,
    hyper: HyperParams,
    /// BTreeMap: строгая сортированность индексов + детерминизм сериализации.
    entries: BTreeMap<u32, f32>,
    force_index32: bool,
}

impl PqwWriter {
    /// Новый писатель для состояния размерности `d_pol` (≥ 1).
    pub fn new(d_pol: u32) -> Result<PqwWriter> {
        if d_pol == 0 {
            return Err(PqwError::BadDimension(d_pol));
        }
        Ok(PqwWriter {
            d_pol,
            hyper: HyperParams::default(),
            entries: BTreeMap::new(),
            force_index32: false,
        })
    }

    /// Гиперпараметры потока (builder).
    pub fn hyperparams(
        mut self,
        eta: f32,
        gamma: f32,
        rho: f32,
        epsilon_threshold: f32,
    ) -> PqwWriter {
        self.hyper = HyperParams {
            eta,
            gamma,
            rho,
            epsilon_threshold,
        };
        self
    }

    /// Запретить u16-топологию: индексы всегда u32.
    pub fn force_index32(mut self) -> PqwWriter {
        self.force_index32 = true;
        self
    }

    /// Явная дуга: индекс + значение `p ∈ [−1, 1]`.
    ///
    /// Не фильтруется LENS: экспертный режим для точечного контроля топологии.
    pub fn add_phase(&mut self, index: u32, p: f32) -> Result<&mut PqwWriter> {
        if index >= self.d_pol {
            return Err(PqwError::BadIndex {
                index,
                d_pol: self.d_pol,
            });
        }
        if !p.is_finite() || p < -1.0 || p > 1.0 {
            return Err(PqwError::BadValue(p));
        }
        if self.entries.contains_key(&index) {
            return Err(PqwError::DuplicateIndex(index));
        }
        self.entries.insert(index, p);
        Ok(self)
    }

    /// Плотное состояние длины `d_pol`: LENS оставляет только `|p| ≥ epsilon_threshold`.
    pub fn add_state(&mut self, state: &[f32]) -> Result<&mut PqwWriter> {
        if state.len() != self.d_pol as usize {
            return Err(PqwError::StateLen {
                expected: self.d_pol as usize,
                actual: state.len(),
            });
        }
        for (i, &p) in state.iter().enumerate() {
            if !p.is_finite() || p < -1.0 || p > 1.0 {
                return Err(PqwError::BadValue(p));
            }
            if p.abs() >= self.hyper.epsilon_threshold {
                self.entries.insert(i as u32, p);
            }
        }
        Ok(self)
    }

    /// Число хранимых дуг.
    pub fn nnz(&self) -> usize {
        self.entries.len()
    }

    /// Размерность состояния.
    pub fn d_pol(&self) -> u32 {
        self.d_pol
    }

    /// Текущие гиперпараметры.
    pub fn hyper(&self) -> HyperParams {
        self.hyper
    }

    /// Будет ли использована u16-топология.
    pub fn uses_index16(&self) -> bool {
        !self.force_index32 && self.d_pol <= 65536
    }

    fn build(&self) -> Result<(Header, Vec<u8>)> {
        if !self.hyper.is_finite() {
            return Err(PqwError::BadValue(self.hyper.eta));
        }
        let index16 = self.uses_index16();
        let indices: Vec<u32> = self.entries.keys().copied().collect();
        let topology_bytes = topology::encode_indices(&indices, index16);
        let phases: Vec<u8> = self.entries.values().map(|&p| quantize(p).raw()).collect();

        // McWeeny-инвариант: max |λ²−λ| по деквантованным значениям дуг.
        let residual = mcweeny::idempotency_residual(
            phases.iter().map(|&raw| PhaseByte::from_validated(raw).p()),
        );

        let mut payload = Vec::with_capacity(topology_bytes.len() + phases.len());
        payload.extend_from_slice(&topology_bytes);
        payload.extend_from_slice(&phases);

        let header = Header {
            format_version: FORMAT_VERSION,
            d_pol: self.d_pol,
            hyper: self.hyper,
            mcweeny_residual: residual,
            payload_digest: sha256_trunc24(&payload),
            topology_offset: HEADER_SIZE as u64,
            topology_len: topology_bytes.len() as u64,
            phase_offset: (HEADER_SIZE + topology_bytes.len()) as u64,
            phase_len: phases.len() as u64,
            nnz: indices.len() as u64,
            flags: Flags::new(index16),
        };
        Ok((header, payload))
    }

    /// Сериализация в память. Детерминизм: одинаковый набор дуг → идентичные байты.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let (header, payload) = self.build()?;
        let mut out = Vec::with_capacity(HEADER_SIZE + payload.len());
        out.extend_from_slice(&header.to_bytes());
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Запись в файл.
    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<()> {
        let bytes = self.to_bytes()?;
        let mut file = std::fs::File::create(path.as_ref())?;
        file.write_all(&bytes)?;
        Ok(())
    }
}
