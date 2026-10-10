//! VAULT-ASM — сокровищница DeepSeek Vault в машинном коде x86_64.
//!
//! v0.87.0: пять микроядер на чистом ассемблере (Intel-синтаксис,
//! `global_asm!`), портированных из `docs/vault_drafts/`:
//!
//! | Ядро | Источник в сокровищнице | Базис |
//! |------|--------------------------|-------|
//! | [`fep_asm`] | `poler-core/src/fep_loss.rs` (Фристон) | AVX2+FMA |
//! | [`lens_asm`] | `poler-lens/src/lens_index.rs` (No-Hits) | SSE2 (базовый!) |
//! | [`synapse_asm`] | блоки 0986/0984 (синапс 1.7Б) | AVX2+FMA+F16C |
//! | [`cordic_asm`] | P3/CORDIC S¹ | SSE2 + целочисленный |
//! | [`stdp_asm`] | LanguageCoreV2 (0601, STDP+WTA) | AVX2+FMA |
//!
//! Каждое ядро имеет скалярный эталон (fallback для хостов без AVX2 и
//! референс для тестов). CLI: `--asm-info`, `--asm-bench`.

#[cfg(target_arch = "x86_64")]
pub mod cordic_asm;
#[cfg(target_arch = "x86_64")]
pub mod fep_asm;
#[cfg(target_arch = "x86_64")]
pub mod lens_asm;
#[cfg(target_arch = "x86_64")]
pub mod stdp_asm;
#[cfg(target_arch = "x86_64")]
pub mod synapse_asm;

use std::sync::OnceLock;

/// CPUID-возможности хоста (детекция один раз).
#[derive(Debug, Clone, Copy)]
pub struct AsmCaps {
    pub avx2: bool,
    pub fma: bool,
    pub f16c: bool,
    pub popcnt: bool,
    pub avx512f: bool,
    pub sse2: bool,
}

impl AsmCaps {
    fn detect() -> Self {
        Self {
            avx2: is_x86_feature_detected!("avx2"),
            fma: is_x86_feature_detected!("fma"),
            f16c: is_x86_feature_detected!("f16c"),
            popcnt: is_x86_feature_detected!("popcnt"),
            avx512f: is_x86_feature_detected!("avx512f"),
            sse2: is_x86_feature_detected!("sse2"),
        }
    }

    /// Золотой набор VAULT-ASM (все ядра, кроме SSE2-базовых).
    pub fn golden(&self) -> bool {
        self.avx2 && self.fma && self.f16c
    }
}

static CAPS: OnceLock<AsmCaps> = OnceLock::new();

/// Возможности CPU (кэш через `OnceLock`).
pub fn caps() -> AsmCaps {
    *CAPS.get_or_init(AsmCaps::detect)
}

/// Текстовый отчёт о микроядрах для CLI (`--asm-info`).
pub fn info_report() -> String {
    let c = caps();
    let on = |b: bool| if b { "ON" } else { "—" };
    format!(
        "VAULT-ASM x86_64 microkernels (v0.87.0)\n\
         \n\
         CPU basis:\n\
           AVX2      : {}\n\
           FMA       : {}\n\
           F16C      : {}\n\
           POPCNT    : {}\n\
           AVX-512F  : {}\n\
           SSE2      : {} (baseline — LENS/CORDIC/WTA всегда)\n\
         \n\
         Kernels (source → engine):\n\
           fep_asm      poler-core/fep_loss.rs  → literary/engine.rs (F, ∇F)  [AVX2+FMA]\n\
           lens_asm     poler-lens/lens_index.rs → engine.rs Causal Nexus    [SSE2]\n\
           synapse_asm  vault 0986/0984 (1.7Б)  → SSN inference layer        [AVX2+FMA+F16C]\n\
           cordic_asm   P3 CORDIC S¹             → детерминированный ротор    [SSE2+int]\n\
           stdp_asm     LanguageCoreV2 0601      → трёхфакторный STDP + WTA   [AVX2+FMA]\n\
         \n\
         Golden set: {}",
        on(c.avx2),
        on(c.fma),
        on(c.f16c),
        on(c.popcnt),
        on(c.avx512f),
        on(c.sse2),
        on(c.golden()),
    )
}

/// Печать отчёта (CLI `--asm-info`).
pub fn print_info() {
    println!("{}", info_report());
}

fn mps(n: f64, secs: f64) -> f64 {
    n / secs / 1e6
}

fn gbs(bytes: f64, secs: f64) -> f64 {
    bytes / secs / 1e9
}

/// Бенчмарк микроядер (CLI `--asm-bench`): пропускная способность +
/// сверка со скалярными эталонами. Возвращает код выхода.
pub fn run_bench(synapses: usize, cycles: usize) -> std::process::ExitCode {
    use std::time::Instant;
    let c = caps();
    println!("VAULT-ASM microkernel benchmark (v0.87.0)");
    println!(
        "caps: avx2={} fma={} f16c={} popcnt={} avx512={}\n",
        c.avx2, c.fma, c.f16c, c.popcnt, c.avx512f
    );
    let mut all_pass = true;

    // ---------------- FEP: полный шаг Фристона ----------------
    {
        let n = 1 << 20;
        let mut p: Vec<f32> = (0..n).map(|i| ((i % 97) as f32) / 97.0).collect();
        let o: Vec<f32> = (0..n).map(|i| ((i % 61) as f32) / 61.0).collect();
        let gw: Vec<f32> = vec![1.0; n];
        let mut grad = vec![0.0f32; n];
        // прогрев
        fep_asm::step(&mut p, &o, &gw, &mut grad, 1e-3, 0.05);
        let t = Instant::now();
        let steps = 100;
        let mut f = 0.0;
        for _ in 0..steps {
            f = fep_asm::step(&mut p, &o, &gw, &mut grad, 1e-3, 0.05);
        }
        let dt = t.elapsed().as_secs_f64();
        let ok = f.is_finite();
        all_pass &= ok;
        println!(
            "FEP  step        n={:>9} x{:>3}: {:8.1} M MAC8/s   F={:.4}   {}",
            n,
            steps,
            mps((n * steps) as f64, dt),
            f,
            pass(ok)
        );
        // сверка
        let mut p2 = p.clone();
        let mut g2 = vec![0.0f32; 64];
        let mut p3: Vec<f32> = p[..64].to_vec();
        let o64: Vec<f32> = o[..64].to_vec();
        let gw64: Vec<f32> = gw[..64].to_vec();
        p2[..64].copy_from_slice(&p3);
        fep_asm::step(&mut p2[..64], &o64, &gw64, &mut g2, 1e-3, 0.05);
        fep_asm::step_scalar(&mut p3, &o64, &gw64, &mut vec![0.0f32; 64], 1e-3, 0.05);
        let ok2 = p2[..64]
            .iter()
            .zip(&p3)
            .all(|(a, b)| (a - b).abs() < 1e-4);
        all_pass &= ok2;
        println!("     parity vs scalar: {}", pass(ok2));
    }

    // ---------------- FEP: только энергия ----------------
    {
        let n = 10_000_000;
        let p: Vec<f32> = (0..n).map(|i| ((i % 89) as f32) / 89.0).collect();
        let o: Vec<f32> = (0..n).map(|i| ((i % 71) as f32) / 71.0).collect();
        // identity-таблица .rodata ограничена 64 float — для 10M метрика
        // материализуется явно (один аллок на бенч)
        let gw: Vec<f32> = vec![1.0; n];
        let t = Instant::now();
        let f = fep_asm::energy(&p, &o, Some(&gw), 0.5);
        let dt = t.elapsed().as_secs_f64();
        println!(
            "FEP  energy      n={:>9}     : {:8.1} M elem/s  ({:.2} GB/s)  F={:.2}",
            n,
            mps(n as f64, dt),
            gbs((n * 8) as f64, dt),
            f
        );
    }

    // ---------------- LENS: No-Hits барьер ----------------
    {
        let n = 10_000_000;
        let mut s = 12345u64;
        let mut next = move || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s
        };
        let w: Vec<f32> = (0..n).map(|_| (next() % 100) as f32 / 100.0).collect();
        let flags: Vec<u64> = (0..n).map(|_| next() & 0xF).collect();
        let mut out = vec![0u32; n];
        let t = Instant::now();
        let kept = lens_asm::filter(&w, &flags, &mut out, lens_asm::LENS_MIN_W, 0x3, 0x8);
        let dt = t.elapsed().as_secs_f64();
        let t2 = Instant::now();
        let bits = lens_asm::popcount(&flags, 0x3);
        let dt2 = t2.elapsed().as_secs_f64();
        println!(
            "LENS filter      n={:>9}     : {:8.1} M cand/s   keep {:.1}%   [барьер 0.05]",
            n,
            mps(n as f64, dt),
            100.0 * kept as f64 / n as f64
        );
        println!(
            "LENS popcount    n={:>9}     : {:8.1} M flags/s  Σbits={}",
            n,
            mps(n as f64, dt2),
            bits
        );
        // сверка семантики на подмножестве
        let mut out_r = vec![0u32; 4096];
        let kr = lens_asm::filter_scalar(&w[..4096], &flags[..4096], &mut out_r, lens_asm::LENS_MIN_W, 0x3, 0x8);
        let ka = lens_asm::filter(&w[..4096], &flags[..4096], &mut out[..4096], lens_asm::LENS_MIN_W, 0x3, 0x8);
        let ok = kr == ka && out[..ka] == out_r[..kr];
        all_pass &= ok;
        println!("     parity vs scalar: {}", pass(ok));
    }

    // ---------------- SSN: синапс-атомарный слой ----------------
    {
        let n_pre = (synapses / 8).max(1024);
        let fanout = 8;
        let field = synapse_asm::SynapseField::synthetic(n_pre, fanout, 4242);
        let mut pre: Vec<f32> = (0..field.pad_pre_len())
            .map(|i| ((i % 53) as f32) / 53.0)
            .collect();
        let mut post = vec![0.0f32; field.n_post];
        // прогрев
        field.step_f16(&pre, &mut post, 0.9, 0.5);
        post.iter_mut().for_each(|x| *x = 0.0);
        let t = Instant::now();
        let mut e = 0.0;
        for _ in 0..cycles.max(1) {
            e = field.step_f16(&pre, &mut post, 0.9, 0.5);
        }
        let dt = t.elapsed().as_secs_f64();
        let total = (n_pre * fanout) as f64 * cycles.max(1) as f64;
        let mb_f16 = (n_pre * fanout * 2) as f64 / 1e6;
        println!(
            "SSN  f16         {:>9} syn x{:>3}: {:8.1} M syn/s   payload {:.1} МБ (2.0 Б/син)  E={:.2}",
            n_pre * fanout,
            cycles.max(1),
            mps(total, dt),
            mb_f16,
            e
        );
        // i8-режим: 1 Б/синапс
        let mut post8 = vec![0.0f32; field.n_post];
        field.step_i8(&pre, &mut post8, 0.9, 0.5);
        post8.iter_mut().for_each(|x| *x = 0.0);
        let t = Instant::now();
        let mut e8 = 0.0;
        for _ in 0..cycles.max(1) {
            e8 = field.step_i8(&pre, &mut post8, 0.9, 0.5);
        }
        let dt = t.elapsed().as_secs_f64();
        let mb_i8 = (n_pre * fanout) as f64 / 1e6;
        println!(
            "SSN  i8          {:>9} syn x{:>3}: {:8.1} M syn/s   payload {:.1} МБ (1.0 Б/син)  E={:.2}",
            n_pre * fanout,
            cycles.max(1),
            mps(total, dt),
            mb_i8,
            e8
        );
        // сверка всех 16 примитивов
        let mut f16 = synapse_asm::SynapseField::synthetic(256, 8, 777);
        for (i, fu) in f16.func.iter_mut().enumerate() {
            *fu = (i % synapse_asm::FUNC_COUNT) as u8;
        }
        let mut pa = vec![0.0f32; f16.n_post];
        let mut pr = vec![0.0f32; f16.n_post];
        let ea = f16.step_f16(&pre[..f16.pad_pre_len()], &mut pa, 0.9, 0.5);
        let er = f16.step_scalar(
            synapse_asm::Density::F16,
            &pre[..f16.pad_pre_len()],
            &mut pr,
            0.9,
            0.5,
        );
        let ok = (ea - er).abs() / er.abs().max(1e-6) < 2e-3;
        all_pass &= ok;
        println!(
            "     parity 16 примитивов vs scalar: {} ({:.4} vs {:.4})",
            pass(ok),
            ea,
            er
        );
    }

    // ---------------- CORDIC: ротор S¹ ----------------
    {
        let n = 2_000_000;
        let mut re: Vec<f32> = (0..n).map(|i| ((i * 37 % 199) as f32 / 100.0) - 1.0).collect();
        let mut im: Vec<f32> = (0..n).map(|i| ((i * 53 % 199) as f32 / 100.0) - 1.0).collect();
        let t = Instant::now();
        cordic_asm::renorm(&mut re, &mut im);
        let dt = t.elapsed().as_secs_f64();
        // ошибка нормы
        let mut worst = 0.0f32;
        for i in 0..n {
            let m = (re[i] * re[i] + im[i] * im[i]).sqrt();
            worst = worst.max((m - 1.0).abs());
        }
        let ok = worst < 5e-3;
        all_pass &= ok;
        println!(
            "CORDIC renorm    n={:>9}     : {:8.3} M elem/s  max‖·‖err {:.2e}  {}",
            n,
            mps(n as f64, dt),
            worst,
            pass(ok)
        );
    }

    // ---------------- STDP + WTA ----------------
    {
        let n = 10_000_000.min(synapses.max(1_000_000));
        let mut w: Vec<f32> = vec![0.1; n];
        let mut tr: Vec<f32> = vec![0.0; n];
        let pre: Vec<f32> = (0..n).map(|i| ((i % 31) as f32 / 31.0)).collect();
        stdp_asm::step(&mut w, &mut tr, &pre, 0.01, 0.9, 0.95, 2.0);
        let t = Instant::now();
        let steps = cycles.max(1);
        let mut dw = 0.0;
        for _ in 0..steps {
            dw = stdp_asm::step(&mut w, &mut tr, &pre, 0.01, 0.9, 0.95, 2.0);
        }
        let dt = t.elapsed().as_secs_f64();
        let total = n as f64 * steps as f64;
        println!(
            "STDP step        {:>9} syn x{:>3}: {:8.1} M syn/s   Σ|Δw|={:.1}",
            n,
            steps,
            mps(total, dt),
            dw
        );
        let rates: Vec<f32> = (0..1_000_000)
            .map(|i| ((i * 2654435761u64 % 1000) as f32 / 1000.0))
            .collect();
        let t = Instant::now();
        let idx = stdp_asm::wta_argmax(&rates);
        let dt = t.elapsed().as_secs_f64();
        println!(
            "WTA  argmax      n={:>9}     : {:8.1} M rate/s  idx={}  {}",
            rates.len(),
            mps(rates.len() as f64, dt),
            idx,
            pass(idx == wta_expected(&rates))
        );
    }

    println!("\nИтог: {}", if all_pass { "ВСЕ ПРОВЕРКИ ПРОЙДЕНЫ" } else { "ЕСТЬ РАССОГЛАСОВАНИЯ" });
    if all_pass {
        std::process::ExitCode::SUCCESS
    } else {
        std::process::ExitCode::from(3)
    }
}

fn wta_expected(rates: &[f32]) -> u32 {
    let mut best = 0usize;
    for (i, &r) in rates.iter().enumerate() {
        if r > rates[best] {
            best = i;
        }
    }
    best as u32
}

fn pass(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}
