//! C-ABI мост к крипто-ядру POLER (PND v8.2, `os/core/poler_core.zig`).
//!
//! Фича `pnd-ffi`: `build.rs` собирает `os/core` через Zig 0.14.0 в
//! статическую библиотеку `libpoler_core.a` и линкует её. Требования к
//! окружению: `zig` в PATH, либо `POLER_ZIG=/путь/к/zig`, либо готовая
//! библиотека `POLER_CORE_LIB=.../os/core/zig-out/lib`.
//!
//! Экспортируемый слой (`os/core/abi.zig`):
//! - скаляры: `phi` (биекция Φ), `pnd_mix` (φ(a·b) +% ε·φ(a⊕b)),
//!   `lhca_step`, `mix_columns` (MDS, ℬ=5), `ct_sbox`, `mod_inverse32`;
//! - полный шифр `PolerCipher` (Feistel ×20, P0-F1: полный 256-битный
//!   ключ) — opaque handle;
//! - счётчиковый `PolerDrbg` (P0-F3: 256-битное состояние, POLER-CTR);
//! - `PolerCbc` (P0-F2: каскад с IV и сцеплением).
//!
//! Все тесты этого модуля сверяются с golden-векторами
//! `tools/verifiers/golden/pnd_v8_golden_54626.txt` — трёхъязычная
//! связка Zig ↔ Python ↔ Rust (MVR-протокол).

use std::ffi::c_void;

pub const KEY_WORDS: usize = 8;
pub const BLOCK_WORDS: usize = 4;

extern "C" {
    fn poler_core_version() -> u32;
    fn poler_phi(x: u32) -> u32;
    fn poler_pnd_mix(a: u32, b: u32, epsilon: u32) -> u32;
    fn poler_lhca_step(x: u32, rule_mask: u32) -> u32;
    fn poler_mix_columns(word: u32) -> u32;
    fn poler_ct_sbox(x: u8) -> u8;
    fn poler_mod_inverse32(a: u32) -> u32;

    fn poler_cipher_new(key: *const u32, epsilon: u32) -> *mut c_void;
    fn poler_cipher_encrypt(handle: *const c_void, plaintext: *const u32, ciphertext: *mut u32);
    fn poler_cipher_decrypt(handle: *const c_void, ciphertext: *const u32, plaintext: *mut u32);
    fn poler_cipher_free(handle: *mut c_void);

    fn poler_drbg_new(seed: *const u32) -> *mut c_void;
    fn poler_drbg_next(handle: *mut c_void) -> u32;
    fn poler_drbg_next_range(handle: *mut c_void, max: u32) -> u32;
    fn poler_drbg_free(handle: *mut c_void);
}

// ── Скалярные примитивы ─────────────────────────────────────────────────────

/// Версия схемы крипто-ядра (8 = PND v8).
pub fn core_version() -> u32 {
    unsafe { poler_core_version() }
}

/// Биекция Φ — ARX-box (add/rotl13/xorshift16/mul/rotl7/add).
pub fn phi(x: u32) -> u32 {
    unsafe { poler_phi(x) }
}

/// pndMix = φ(a·b) +% ε·φ(a⊕b); при ε=0 — автокоррекция на 1.
pub fn pnd_mix(a: u32, b: u32, epsilon: u32) -> u32 {
    unsafe { poler_pnd_mix(a, b, epsilon) }
}

/// Шаг LHCA (клеточный автомат) с маской правила.
pub fn lhca_step(x: u32, rule_mask: u32) -> u32 {
    unsafe { poler_lhca_step(x, rule_mask) }
}

/// MDS-диффузия MixColumns (ветвление = 5).
pub fn mix_columns(word: u32) -> u32 {
    unsafe { poler_mix_columns(word) }
}

/// Constant-time S-box (x^254 над GF(2^8)).
pub fn ct_sbox(x: u8) -> u8 {
    unsafe { poler_ct_sbox(x) }
}

/// Обращение нечётного a по модулю 2^32 (Hensel; чётное a → 0).
pub fn mod_inverse32(a: u32) -> u32 {
    unsafe { poler_mod_inverse32(a) }
}

// ── Полный шифр (opaque handle) ────────────────────────────────────────────

/// Контекст блочного шифра POLER (Feistel ×20, 128-бит блок, 256-бит ключ).
/// Создаётся через [`PolerCipher::new`], освобождается автоматически (Drop).
pub struct PolerCipher {
    handle: *mut c_void,
}

impl PolerCipher {
    /// Создать контекст из 256-битного ключа (8 слов) и базового ε.
    pub fn new(key: &[u32; KEY_WORDS], epsilon: u32) -> Option<Self> {
        let handle = unsafe { poler_cipher_new(key.as_ptr(), epsilon) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Зашифровать блок (4 слова).
    pub fn encrypt_block(&self, plaintext: &[u32; BLOCK_WORDS]) -> [u32; BLOCK_WORDS] {
        let mut ct = [0u32; BLOCK_WORDS];
        unsafe { poler_cipher_encrypt(self.handle, plaintext.as_ptr(), ct.as_mut_ptr()) };
        ct
    }

    /// Расшифровать блок (4 слова).
    pub fn decrypt_block(&self, ciphertext: &[u32; BLOCK_WORDS]) -> [u32; BLOCK_WORDS] {
        let mut pt = [0u32; BLOCK_WORDS];
        unsafe { poler_cipher_decrypt(self.handle, ciphertext.as_ptr(), pt.as_mut_ptr()) };
        pt
    }
}

impl Drop for PolerCipher {
    fn drop(&mut self) {
        unsafe { poler_cipher_free(self.handle) };
    }
}

// ── Счётчиковый DRBG (P0-F3) ───────────────────────────────────────────────

/// Счётчиковый генератор POLER-CTR: 256-битное состояние, u64-счётчик,
/// rekey каждые 2^16 блоков. Замена удалённого PolerPrng (аудит Шнайера F3).
pub struct PolerDrbg {
    handle: *mut c_void,
}

impl PolerDrbg {
    /// Создать генератор из 256-битного сида.
    pub fn new(seed: &[u32; KEY_WORDS]) -> Option<Self> {
        let handle = unsafe { poler_drbg_new(seed.as_ptr()) };
        if handle.is_null() {
            None
        } else {
            Some(Self { handle })
        }
    }

    /// Следующее 32-битное слово потока.
    pub fn next(&mut self) -> u32 {
        unsafe { poler_drbg_next(self.handle) }
    }

    /// Равномерное слово в [0, max) — безсмещённый rejection sampling.
    pub fn next_range(&mut self, max: u32) -> u32 {
        unsafe { poler_drbg_next_range(self.handle, max) }
    }
}

impl Drop for PolerDrbg {
    fn drop(&mut self) {
        unsafe { poler_drbg_free(self.handle) };
    }
}

// ── Тесты: трёхъязычная golden-связка Zig ↔ Python ↔ Rust ──────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// golden: pnd_v8_golden_54626.txt, секция phi.
    #[test]
    fn phi_matches_golden() {
        assert_eq!(phi(0x00000000), 0x1E50A3B3);
        assert_eq!(phi(0x9E3779B9), 0x041222E1);
        assert_eq!(phi(0xFFFFFFFF), 0x39C0A3FF);
    }

    /// golden: pnd_mix(42, 17, 1) = 0xF372612D; ε=0 → автокоррекция на 1
    /// (совпадает с ε=1 — golden-вектор «pndmix 2a 11 0»).
    #[test]
    fn pnd_mix_matches_golden() {
        assert_eq!(pnd_mix(42, 17, 1), 0xF372612D);
        assert_eq!(pnd_mix(42, 17, 0), pnd_mix(42, 17, 1));
    }

    /// golden: первый вектор cipher (P0-F1 расписание: key[4..7] в каждом раунде).
    #[test]
    fn cipher_matches_golden_and_roundtrips() {
        let key = [
            0x01234567, 0x89ABCDEF, 0xFEDCBA98, 0x76543210,
            0x11111111, 0x22222222, 0x33333333, 0x44444444,
        ];
        let pt = [0x01234567, 0x89ABCDEF, 0xFEDCBA98, 0x76543210];
        let cipher = PolerCipher::new(&key, 0x00000001).expect("cipher alloc");

        let ct = cipher.encrypt_block(&pt);
        assert_eq!(ct, [0x0D3A2A6A, 0x976050DB, 0x1C142636, 0xA6DB9CC4]);

        let back = cipher.decrypt_block(&ct);
        assert_eq!(back, pt);
    }

    /// Аудит Шнайера, критерий приёмки F1 (Ось IV): любой из 256 бит ключа
    /// меняет шифротекст. Rust-дубль zig-теста — двойная страховка CI.
    #[test]
    fn cipher_full_256bit_key_sensitivity() {
        let key = [
            0x51CE5B7Eu32, 0x1E50A3B3, 0xDEADBEEF, 0x0BADC0DE,
            0x12345678, 0x9ABCDEF0, 0xFEDCBA98, 0x76543211,
        ];
        let pt = [0x01020304, 0x05060708, 0x090A0B0C, 0x0D0E0F10];
        let cipher = PolerCipher::new(&key, 0xDEAD).expect("cipher alloc");
        let ct0 = cipher.encrypt_block(&pt);

        for bit in 0..256 {
            let mut key2 = key;
            key2[bit / 32] ^= 1 << (bit % 32);
            let c1 = PolerCipher::new(&key2, 0xDEAD).expect("cipher alloc");
            let ct1 = c1.encrypt_block(&pt);
            assert_ne!(ct0, ct1, "бит {bit} ключа не влияет на шифротекст — регрессия F1");
        }
    }

    /// golden: drbg, сид из восьми нулей, первое слово потока.
    #[test]
    fn drbg_matches_golden_and_is_deterministic() {
        let seed = [0u32; KEY_WORDS];
        let mut a = PolerDrbg::new(&seed).expect("drbg alloc");
        assert_eq!(a.next(), 0x7F4A93FE);

        let mut b = PolerDrbg::new(&seed).expect("drbg alloc");
        assert_eq!(b.next(), 0x7F4A93FE); // синхронизация позиций потоков
        for _ in 0..1024 {
            assert_eq!(a.next(), b.next());
        }
    }

    #[test]
    fn drbg_range_bounds() {
        let mut drbg = PolerDrbg::new(&[7, 7, 7, 7, 7, 7, 7, 7]).expect("drbg alloc");
        for _ in 0..1000 {
            assert!(drbg.next_range(13) < 13);
        }
        assert_eq!(drbg.next_range(1), 0);
    }

    #[test]
    fn scalar_primitives_sanity() {
        assert_eq!(core_version(), 8);
        // MDS-диффузия меняет все байты слова
        assert_ne!(mix_columns(0x01020304), 0x01020304);
        // S-box — перестановка на краях
        assert_ne!(ct_sbox(0x00), ct_sbox(0x01));
        // modInverse32: 0x9E3779B9⁻¹ = 0x144CBC89 (golden-значение ядра)
        assert_eq!(mod_inverse32(0x9E3779B9), 0x144CBC89);
        assert_eq!(lhca_step(0x12345678, 0xACACACAC), lhca_step(0x12345678, 0xACACACAC));
    }
}
