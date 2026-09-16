//! Суверенная криптография POLER.
//!
//! `pnd` — C-ABI мост к Zig-ядру PND v8.2 (`os/core/poler_core.zig`),
//! собранному в `libpoler_core.a` (фича `pnd-ffi`, см. `build.rs`).
//!
//! Архитектурное место — уровень L0 единого инструмента
//! (`docs/UNIFIED_ARCHITECTURE.md`, M4): крипто-ядро под движком,
//! зависимость направлена строго вниз.

#[cfg(feature = "pnd-ffi")]
pub mod pnd;
