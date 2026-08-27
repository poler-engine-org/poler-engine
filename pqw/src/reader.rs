//! Десериализатор: zero-copy разбор контейнера поверх заимствованного среза
//! (обычный буфер или mmap-отображение).
//!
//! `from_bytes` выполняет полную структурную и семантическую валидацию
//! (заголовок, смещения, длины, сортированность индексов, триты).
//! Digest payload проверяется отдельно и лениво — [`PqwReader::verify_payload`]:
//! «мгновенный холодный старт» не должен упираться в хеширование гигабайтов.

use std::borrow::Cow;

use crate::error::{PqwError, Result};
use crate::header::{Header, HEADER_SIZE};
use crate::mcweeny;
use crate::phase::PhaseByte;
use crate::sha256::sha256_trunc24;
use crate::topology;

/// Одна хранимая дуга: индекс в `[0, d_pol)` + упакованная фаза.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Arc {
    /// Индекс дуги в состоянии.
    pub index: u32,
    /// Упакованная фаза: трит + кривизна.
    pub phase: PhaseByte,
}

impl Arc {
    /// Деквантованное значение p̂.
    #[inline]
    pub fn p(self) -> f64 {
        self.phase.p()
    }

    /// Фазовый угол θ̂ = arccos(p̂).
    #[inline]
    pub fn theta(self) -> f64 {
        self.phase.theta()
    }
}

/// Zero-copy читатель `.poler` / `.pqw`.
pub struct PqwReader<'a> {
    header: Header,
    data: &'a [u8],
}

impl<'a> PqwReader<'a> {
    /// Полный разбор и валидация поверх среза (буфер или mmap).
    pub fn from_bytes(data: &'a [u8]) -> Result<PqwReader<'a>> {
        let header = Header::from_bytes(data)?;

        let topo_off = header.topology_offset as usize;
        let topo_len = header.topology_len as usize;
        let phase_off = header.phase_offset as usize;
        let phase_len = header.phase_len as usize;
        let width = header.flags.index_width();

        if topo_off != HEADER_SIZE {
            return Err(PqwError::Layout("topology must start at 0x80"));
        }
        if header.nnz > u64::from(header.d_pol) {
            return Err(PqwError::Layout("nnz exceeds d_pol"));
        }
        if header.topology_len != header.nnz * width as u64 {
            return Err(PqwError::InconsistentTopology {
                field: "topology_len",
                expected: header.nnz * width as u64,
                actual: header.topology_len,
            });
        }
        if header.phase_len != header.nnz {
            return Err(PqwError::InconsistentTopology {
                field: "phase_len",
                expected: header.nnz,
                actual: header.phase_len,
            });
        }
        if phase_off != topo_off + topo_len {
            return Err(PqwError::Layout("phase blocks must follow the topology"));
        }
        let expected_len = phase_off + phase_len;
        if data.len() < expected_len {
            return Err(PqwError::Truncated {
                need: expected_len,
                have: data.len(),
            });
        }
        if data.len() > expected_len {
            return Err(PqwError::Layout("trailing bytes after the phase blocks"));
        }

        // Семантическая валидация payload: индексы и триты.
        let indices = topology::decode_indices(&data[topo_off..phase_off], header.flags.index16())?;
        topology::validate_indices(&indices, header.d_pol)?;
        for &raw in &data[phase_off..phase_off + phase_len] {
            PhaseByte::from_raw(raw)?;
        }

        Ok(PqwReader { header, data })
    }

    /// Разобранный заголовок.
    pub fn header(&self) -> &Header {
        &self.header
    }

    /// Размерность состояния.
    pub fn d_pol(&self) -> u32 {
        self.header.d_pol
    }

    /// Число хранимых дуг.
    pub fn nnz(&self) -> u64 {
        self.header.nnz
    }

    /// Гиперпараметры из заголовка.
    pub fn hyperparams(&self) -> crate::header::HyperParams {
        self.header.hyper
    }

    /// McWeeny-инвариант на момент записи: max |λ² − λ|.
    pub fn mcweeny_residual(&self) -> f64 {
        self.header.mcweeny_residual
    }

    fn topology_bytes(&self) -> &'a [u8] {
        let a = self.header.topology_offset as usize;
        let b = self.header.phase_offset as usize;
        &self.data[a..b]
    }

    /// Сырые фазовые блоки (nnz байт) — zero-copy заимствование.
    pub fn phase_bytes(&self) -> &'a [u8] {
        let a = self.header.phase_offset as usize;
        let b = a + self.header.phase_len as usize;
        &self.data[a..b]
    }

    /// Индексы ненулевых дуг (возрастающие).
    /// В u32-режиме на little-endian — заимствование без копирования.
    ///
    /// Ошибка невозможна после `from_bytes` (длина топологии провалидирована).
    pub fn indices(&self) -> Cow<'a, [u32]> {
        match topology::decode_indices(self.topology_bytes(), self.header.flags.index16()) {
            Ok(cow) => cow,
            Err(_) => Cow::Owned(Vec::new()), // unreachable после from_bytes
        }
    }

    /// Итератор дуг — zero-copy, без аллокаций.
    pub fn arcs(&self) -> impl Iterator<Item = Arc> + 'a {
        let width = self.header.flags.index_width();
        let topo = self.topology_bytes();
        let phases = self.phase_bytes();
        let indices = topo.chunks(width).map(move |c| {
            // u16-LE укладываем в младшие байты u32: [lo, hi, 0, 0].
            let mut b = [0u8; 4];
            b[..width].copy_from_slice(c);
            u32::from_le_bytes(b)
        });
        indices.zip(phases.iter().copied()).map(|(index, raw)| Arc {
            index,
            phase: PhaseByte::from_validated(raw),
        })
    }

    /// Деквантованные дуги: `(index, p̂)`.
    pub fn decoded(&self) -> impl Iterator<Item = (u32, f64)> + 'a {
        self.arcs().map(|arc| (arc.index, arc.phase.p()))
    }

    /// Ленивая проверка целостности payload: SHA-256 (24 байта) поверх
    /// топологии + фазовых блоков.
    pub fn verify_payload(&self) -> Result<()> {
        if sha256_trunc24(&self.data[HEADER_SIZE..]) == self.header.payload_digest {
            Ok(())
        } else {
            Err(PqwError::CorruptPayload)
        }
    }

    /// McWeeny-очистка хранимых дуг: `steps` итераций `p ← purify_p(p)`.
    pub fn purify_steps(&self, steps: usize) -> Vec<(u32, f64)> {
        self.decoded()
            .map(|(index, p)| {
                let mut p = p;
                for _ in 0..steps {
                    p = mcweeny::purify_p(p);
                }
                (index, p)
            })
            .collect()
    }

    /// Два шага McWeeny — типовой режим восстановления P² = P (1–2 такта).
    pub fn purified(&self) -> Vec<(u32, f64)> {
        self.purify_steps(2)
    }
}
