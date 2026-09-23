//! # Water — спектральная гидродинамика (цикл V1, v0.58.0)
//!
//! Разбор владельца «OpenAI решила задачу» (2026-09-24):
//!
//! ```text
//!   OpenAI (brute force)              POLER WATER (спектр)
//!   ─────────────────                 ────────────────────
//!   10 000 агентов, 88 часов,         состояние = K мод поля
//!   тераватты под столом              (байты, не гигабайты)
//!   UE/Unity: плотные сетки f32,      эволюция = целочисленные
//!   Re^(9/4) ≈ 10^14 точек,           трит-операции на кремнии
//!   дрейф округлений, «резина»        (счётчик фаз — фикс-точка)
//! ```
//!
//! Принцип владельца: «Компьютеру не нужно перебирать бесконечность
//! или плодить миллионы float-матриц. Плавающая точка — это кремний.
//! Нужно дать условия и инварианты, услышать уравнение через
//! вязкость и энергию».
//!
//! Физика (всё честно, без магии):
//!
//! ```text
//!   1. Состояние — спектр гравитационно-капиллярных волн:
//!      h(x,t) = Σ_k A_k · cos(k·x + φ_k(t)),
//!      дисперсия глубоководная: ω(k) = √(g·k + (σ/ρ)·k³).
//!   2. Равновесие — каскад Колмогорова: A(k) ∝ k^(−5/6)
//!      (E(k) ∝ k^(−5/3)), окно ветра вокруг k_p = g/v²
//!      (пик Пирсона–Московица), H_s ≈ 0.21·v²/g.
//!   3. Вязкость (Навье–Стокс, Ламб): A(t) = A₀·e^(−2νk²t) —
//!      в лог-уровнях амплитуды это линейный спуск.
//!   4. Ветер: релаксация к равновесию за τ_wind —
//!      стационарный баланс «накачка ↔ диссипация».
//! ```
//!
//! Кремний (главное): фаза живёт в **целочисленном счётчике**
//! микро-секторов 3^(p+m) с 32 дробными битами фикс-точки.
//! Шаг эволюции — одно целочисленное вычитание и один rem_euclid:
//! ошибка ≤ 2⁻³³ сектора на шаг и практически не накапливается
//! (дрейф f32-аккумулятора на тех же шагах — порядка радиан;
//! см. тест `f32_drifts_trit_counter_does_not`). Амплитуда —
//! тернарная лестница 3^t лог-уровней с фикс-точечным накопителем;
//! релаксация τ_wind сама демпфирует отклонения — устойчиво
//! по построению.
//!
//! Контейнер — тот же VRTX, что у кодека V0: спектр есть спектр.
//! `to_vortex()` раскладывает каждую волну в эрмитову пару ±k,
//! `vortex::synthesize` даёт поле высот; `from_vortex` собирает
//! пары обратно (физика мод восстанавливается из параметров,
//! равновесие принимается равным текущему уровню — контейнер
//! хранит видимый кадр спектра). Течения для геймплея —
//! аналитические орбитальные скорости линейной теории (точные
//! производные потенциала, без конечных разностей): потенциальное
//! течение, завихрённость нуль по построению (тест меряет curl
//! численно). Вихри в линейной воде живут в фазе поля (волновые
//! дислокации) — нелинейное растяжение вихрей и обрушение
//! гребней это будущие циклы; верность квантования честно меряется
//! PSNR против f64-эталона с теми же уравнениями ([`WaterF64`]).

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};
use super::vortex::{self, Rng};

/// Фикс-точка состояний: 32 дробных бита.
const FP: f64 = 4294967296.0; // 2^32
const FP_I: i64 = 1i64 << 32;

// ---------------------------------------------------------------------------
// Параметры
// ---------------------------------------------------------------------------

/// Параметры водной поверхности. Физические величины — в СИ.
#[derive(Debug, Clone)]
pub struct WaterParams {
    /// Сетка синтеза n×n (степень двойки).
    pub n: usize,
    /// Число активных мод (волн).
    pub modes: usize,
    /// Сид детерминизма.
    pub seed: u64,
    /// Скорость ветра, м/с — задаёт пик спектра k_p = g/v² и H_s.
    pub wind: f64,
    /// Время релаксации к равновесию, с (игровой масштаб развития моря).
    pub tau_wind: f64,
    /// Кинематическая вязкость, м²/с (пресная вода ≈ 1e-6).
    pub viscosity: f64,
    /// Ускорение свободного падения, м/с².
    pub gravity: f64,
    /// Капиллярность σ/ρ, м³/с² (вода ≈ 7.4e-5).
    pub capillary: f64,
    /// Физический размер домена, м.
    pub domain: f64,
    /// Направление ветра, рад (0 = +x).
    pub wind_dir: f64,
    /// Угловой разброс волн вокруг ветра, рад.
    pub spread: f64,
    /// Триты амплитуды: 3^t лог-уровней лестницы.
    pub amp_trits: u32,
    /// Триты фазы (сектора): 3^p на оборот.
    pub phase_trits: u32,
    /// Суб-секторные триты: счётчик живёт в 3^(p+m) микро-секторах.
    pub micro_trits: u32,
}

impl Default for WaterParams {
    fn default() -> Self {
        WaterParams {
            n: 256,
            modes: 192,
            seed: 42,
            wind: 8.0,
            tau_wind: 30.0,
            viscosity: 1e-6,
            gravity: 9.81,
            capillary: 7.4e-5,
            domain: 100.0,
            wind_dir: 0.0,
            spread: PI / 5.0,
            amp_trits: 6,
            phase_trits: 6,
            micro_trits: 3,
        }
    }
}

impl WaterParams {
    /// Валидация параметров (общая для либы и shell).
    pub fn validate(&self) -> Result<(), String> {
        if !(16..=1024).contains(&self.n) || !self.n.is_power_of_two() {
            return Err("water: n — степень двойки 16..=1024".into());
        }
        if !(1..=16384).contains(&self.modes) {
            return Err("water: modes 1..=16384".into());
        }
        if !(0.5..=60.0).contains(&self.wind) {
            return Err("water: wind 0.5..=60 м/с".into());
        }
        if !(0.5..=3600.0).contains(&self.tau_wind) {
            return Err("water: tau-wind 0.5..=3600 с".into());
        }
        if !(1e-8..=1e-1).contains(&self.viscosity) {
            return Err("water: viscosity 1e-8..=1e-1 м²/с".into());
        }
        if !(0.1..=1000.0).contains(&self.domain) {
            return Err("water: domain 0.1..=1000 м".into());
        }
        if !(2..=8).contains(&self.amp_trits) {
            return Err("water: amp-trits 2..=8".into());
        }
        if !(2..=10).contains(&self.phase_trits) {
            return Err("water: phase-trits 2..=10".into());
        }
        if self.micro_trits != 3 {
            return Err("water: micro-trits = 3 (фикс-точка счётчика)".into());
        }
        if !(self.gravity > 0.0) || !(self.capillary >= 0.0) || !(self.spread > 0.0) {
            return Err("water: физические константы должны быть положительными".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Состояние: целочисленный спектр
// ---------------------------------------------------------------------------

/// Одна волна спектра. Состояние — только целые числа;
/// физика моды прекомпьютедна (f64-константы, не состояние).
#[derive(Debug, Clone)]
pub struct WaterMode {
    /// Бин Фурье по x, 1..=n/2 (канонический представитель пары ±k).
    pub kx: i32,
    /// Бин Фурье по y, −n/2..=n/2.
    pub ky: i32,
    /// Фикс-точечный накопитель уровня амплитуды (единица = 2⁻³² лестницы).
    pub acc: i64,
    /// Фикс-точечный счётчик фазы: микро-сектора·2³²; убывает — волна
    /// бежит по +k (по ветру).
    pub phase_fp: i64,
    /// Физическое волновое число |k|, рад/м.
    pub k: f64,
    /// Физическая проекция k на ось x, рад/м.
    pub kx_phys: f64,
    /// Физическая проекция k на ось y, рад/м.
    pub ky_phys: f64,
    /// Частота по дисперсионному соотношению, рад/с.
    pub omega: f64,
    /// Равновесный уровень лестницы (окно ветра × K41).
    pub u_eq: f64,
    /// Вязкий спуск по лестнице, уровней/с (константа ≥ 0).
    pub decay_rate: f64,
}

/// Водная поверхность как спектр волн: состояние в целых числах.
#[derive(Debug, Clone)]
pub struct WaterSpectrum {
    pub n: usize,
    pub amp_trits: u32,
    pub phase_trits: u32,
    /// Микро-секторов на оборот: 3^(p+m).
    pub micro_total: i64,
    /// Границы лог-окна амплитуд в Place-единицах (бины Фурье: A·n²/2).
    pub a_min: f64,
    pub a_max: f64,
    /// Уровней в лестнице: 3^t − 1.
    pub level_max: i64,
    /// Натуральных логарифмов на уровень лестницы.
    pub ln_per_level: f64,
    /// Время релаксации к равновесию, с.
    pub tau_wind: f64,
    /// Физический размер домена, м.
    pub domain: f64,
    pub modes: Vec<WaterMode>,
    /// Целочисленный счётчик шагов.
    pub steps: u64,
    /// Целевая дисперсия высот, м² (нормировка спектра по ветру).
    pub variance_target: f64,
}

impl WaterSpectrum {
    /// Видимый уровень лестницы амплитуды.
    pub fn level(&self, m: &WaterMode) -> i64 {
        (m.acc >> 32).clamp(0, self.level_max)
    }

    /// Амплитуда волны в Place-единицах (значение бина Фурье): A·n²/2.
    fn amp_place(&self, m: &WaterMode) -> f64 {
        let u = self.level(m) as f64;
        self.a_min * (self.a_max / self.a_min).powf(u / self.level_max as f64)
    }

    /// Физическая амплитуда волны, м.
    pub fn amplitude(&self, m: &WaterMode) -> f64 {
        2.0 * self.amp_place(m) / (self.n * self.n) as f64
    }

    /// Фаза волны, рад — в точности та, что видит контейнер и синтез
    /// (микро-сектор фикс-точки, дробные биты — только для эволюции).
    pub fn phase(&self, m: &WaterMode) -> f64 {
        let s = (m.phase_fp >> 32).rem_euclid(self.micro_total);
        TAU * s as f64 / self.micro_total as f64
    }

    /// Дисперсия высот поверхности, м².
    pub fn variance(&self) -> f64 {
        self.modes
            .iter()
            .map(|m| {
                let a = self.amplitude(m);
                a * a / 2.0
            })
            .sum()
    }

    /// Значимая высота волн H_s = 4σ, м.
    pub fn significant_height(&self) -> f64 {
        4.0 * self.variance().sqrt()
    }

    /// Один шаг эволюции. Фаза — целочисленно (без дрейфа), амплитуда —
    /// фикс-точечная лестница с релаксацией к равновесию и вязким спуском.
    pub fn step(&mut self, dt: f64) {
        assert!(dt.is_finite() && dt > 0.0, "water: dt должен быть конечным > 0");
        let wrap = self.micro_total * FP_I;
        let top = self.level_max * FP_I;
        for m in &mut self.modes {
            // Фаза: вычитаем ω·dt в микро-секторах фикс-точки.
            // round() константы — единственный источник ошибки (≤ 2⁻³³
            // сектора на шаг); суммирование — точные целочисленные операции.
            let adv = (m.omega * dt * self.micro_total as f64 / TAU * FP).round() as i64;
            m.phase_fp = (m.phase_fp - adv).rem_euclid(wrap);
            // Амплитуда: du/dt = (u_eq − u)/τ_wind − 2νk²/L.
            let u = m.acc as f64 / FP;
            let du = (m.u_eq - u) / self.tau_wind - m.decay_rate;
            m.acc = (m.acc + (du * dt * FP).round() as i64).clamp(0, top);
        }
        self.steps += 1;
    }

    /// Высота в точке сетки (x, y), м. O(K) — запросы геймплея.
    pub fn height_at(&self, x: usize, y: usize) -> f64 {
        let nf = self.n as f64;
        let mut h = 0.0;
        for m in &self.modes {
            let a = self.amplitude(m);
            let arg = TAU * (m.kx as f64 * x as f64 + m.ky as f64 * y as f64) / nf
                + self.phase(m);
            h += a * arg.cos();
        }
        h
    }

    /// Поле высот n×n — через вихревой синтез (тот же FFT-путь, что у кодека).
    pub fn height_field(&self) -> Vec<f64> {
        vortex::synthesize(&self.to_vortex())
    }

    /// Поверхность в физической точке (x, y — метры от угла домена):
    /// (h, ∂h/∂x, ∂h/∂y) — высота и точные градиенты, м и безразмерные.
    /// Один проход по модам — для шейдинга и нормалей.
    pub fn surface_at(&self, x: f64, y: f64) -> (f64, f64, f64) {
        let mut h = 0.0;
        let mut gx = 0.0;
        let mut gy = 0.0;
        for m in &self.modes {
            let a = self.amplitude(m);
            let theta = m.kx_phys * x + m.ky_phys * y + self.phase(m);
            let (s, c) = theta.sin_cos();
            h += a * c;
            gx -= a * m.kx_phys * s;
            gy -= a * m.ky_phys * s;
        }
        (h, gx, gy)
    }

    /// Орбитальное течение в физической точке (x, y — метры):
    /// (u_x, u_y, w) — горизонтальный снос и вертикальная скорость, м/с.
    /// Линейная теория глубоководных волн: u = ∇Φ, z = 0.
    pub fn flow_at(&self, x: f64, y: f64) -> (f64, f64, f64) {
        let mut ux = 0.0;
        let mut uy = 0.0;
        let mut w = 0.0;
        for m in &self.modes {
            let a = self.amplitude(m);
            let theta = m.kx_phys * x + m.ky_phys * y + self.phase(m);
            let (s, c) = theta.sin_cos();
            let aw = a * m.omega;
            ux += aw * (m.kx_phys / m.k) * c;
            uy += aw * (m.ky_phys / m.k) * c;
            w += aw * s;
        }
        (ux, uy, w)
    }
}

// ---------------------------------------------------------------------------
// Генерация спектра: K41 × окно ветра × нормировка Пирсона–Московица
// ---------------------------------------------------------------------------

/// Сгенерировать развитое море: моды лог-равномерно по k с направленным
/// разбросом вокруг ветра, амплитуды — равновесие K41 в окне ветра,
/// нормировка на H_s ≈ 0.21·v²/g. Всё детерминированно от сида.
pub fn generate(p: &WaterParams) -> WaterSpectrum {
    p.validate().expect("water: валидные параметры");
    let mut rng = Rng::new(p.seed);
    let n = p.n;
    let half = (n / 2) as i64;
    let k_min = TAU / p.domain; // бин 1
    let k_peak = (p.gravity / (p.wind * p.wind)).max(k_min);
    let k_nyq = PI * n as f64 / p.domain;
    let k_max = (8.0 * k_peak).min(k_nyq).max(2.0 * k_min);
    let sigma_w = 1.2; // ширина окна ветра (лог-нормальное)

    let mut seen = HashSet::new();
    let mut waves: Vec<(i32, i32, f64)> = Vec::with_capacity(p.modes);
    let mut attempts = 0usize;
    while waves.len() < p.modes && attempts < p.modes * 64 + 256 {
        attempts += 1;
        // Лог-равномерный радиус: каскад от инерционных к мелким масштабам.
        let kk = k_min * (k_max / k_min).powf(rng.next_f64());
        let theta = (p.wind_dir + rng.next_gaussian() * p.spread)
            .clamp(p.wind_dir - PI * 0.45, p.wind_dir + PI * 0.45);
        let kx = (kk * theta.cos() * p.domain / TAU).round() as i64;
        let ky = (kk * theta.sin() * p.domain / TAU).round() as i64;
        // Частоты Найквиста (|бин| = n/2) — самозеркальные: запрещаем,
        // чтобы эрмитова пара ±k всегда была двумя разными бинами.
        if !(1..half).contains(&kx) || ky.abs() >= half {
            continue;
        }
        if !seen.insert((kx, ky)) {
            continue;
        }
        let kbin = ((kx * kx + ky * ky) as f64).sqrt();
        waves.push((kx as i32, ky as i32, TAU * kbin / p.domain));
    }
    assert!(!waves.is_empty(), "water: пустой спектр — увеличь n или domain");

    // Равновесные амплитуды: K41 A ∝ k^(−5/6) в окне ветра,
    // нормировка Σ A²/2 = σ_h², H_s = 4σ_h ≈ 0.21·v²/g (Пирсон–Московиц).
    let hs = 0.21 * p.wind * p.wind / p.gravity;
    let var_target = (hs / 4.0) * (hs / 4.0);
    let raw: Vec<f64> = waves
        .iter()
        .map(|&(_, _, k)| {
            let x = (k / k_peak).ln() / sigma_w;
            (k / k_peak).powf(-5.0 / 6.0) * (-0.5 * x * x).exp()
        })
        .collect();
    let raw_sq: f64 = raw.iter().map(|v| v * v).sum();
    let c = if raw_sq > 0.0 { (2.0 * var_target / raw_sq).sqrt() } else { 0.0 };

    // Place-единицы: бин Фурье несёт A·n²/2 (см. тождество синтеза).
    let np2 = (n * n) as f64 / 2.0;
    let a_place_eq: Vec<f64> = raw.iter().map(|&r| c * r * np2).collect();
    let mut a_lo = f64::INFINITY;
    let mut a_hi = 0.0f64;
    for &a in &a_place_eq {
        a_lo = a_lo.min(a);
        a_hi = a_hi.max(a);
    }
    if !(a_hi > 0.0) {
        a_lo = 1.0;
        a_hi = 1.0;
    }
    let a_min = a_lo / 1.6;
    let a_max = a_hi * 1.6;
    let level_max = 3i64.pow(p.amp_trits) - 1;
    let ln_per_level = (a_max / a_min).ln() / level_max as f64;
    let micro_total = 3i64.pow(p.phase_trits + p.micro_trits);

    let modes: Vec<WaterMode> = waves
        .iter()
        .zip(a_place_eq.iter())
        .map(|(&(kx, ky, k), &a_eq)| {
            let omega = (p.gravity * k + p.capillary * k * k * k).sqrt();
            let u_eq = ((a_eq / a_min).ln() / ln_per_level).clamp(0.0, level_max as f64);
            let decay_rate =
                if ln_per_level > 0.0 { 2.0 * p.viscosity * k * k / ln_per_level } else { 0.0 };
            let phase_fp = (rng.next_f64() * micro_total as f64 * FP).round() as i64;
            WaterMode {
                kx,
                ky,
                acc: (u_eq * FP).round() as i64,
                phase_fp: phase_fp.clamp(0, micro_total * FP_I - 1),
                k,
                kx_phys: TAU * kx as f64 / p.domain,
                ky_phys: TAU * ky as f64 / p.domain,
                omega,
                u_eq,
                decay_rate,
            }
        })
        .collect();

    WaterSpectrum {
        n,
        amp_trits: p.amp_trits,
        phase_trits: p.phase_trits,
        micro_total,
        a_min,
        a_max,
        level_max,
        ln_per_level,
        tau_wind: p.tau_wind,
        domain: p.domain,
        modes,
        steps: 0,
        variance_target: var_target,
    }
}

// ---------------------------------------------------------------------------
// Контейнер VRTX: вода ↔ кодек (спектр есть спектр)
// ---------------------------------------------------------------------------

impl WaterSpectrum {
    /// Спектр воды как спектр кодека: каждая волна раскладывается в
    /// эрмитову пару ±k (вещественное поле). Фаза контейнера — микро-сектор
    /// 3^(p+m): тот же `synthesize`, что у кодека, даёт поле высот.
    pub fn to_vortex(&self) -> vortex::VortexSpectrum {
        let n = self.n as u32;
        let mut harmonics = Vec::with_capacity(self.modes.len() * 2);
        for m in &self.modes {
            let a = self.amp_place(m);
            let q = vortex::amp_quant(a, self.a_min, self.a_max, self.amp_trits);
            let s = (m.phase_fp >> 32).rem_euclid(self.micro_total) as u64;
            let s_neg = (self.micro_total - s as i64).rem_euclid(self.micro_total) as u64;
            let kx = m.kx as u32;
            let ky = m.ky.rem_euclid(n as i32) as u32;
            let kxn = (n - kx) % n;
            let kyn = (n - ky) % n;
            harmonics.push(vortex::Harmonic { kx, ky, q, s });
            harmonics.push(vortex::Harmonic { kx: kxn, ky: kyn, q, s: s_neg });
        }
        vortex::VortexSpectrum {
            n: self.n as u32,
            amp_trits: self.amp_trits,
            phase_trits: self.phase_trits + 3, // p + m микро-секторов
            a_min: self.a_min,
            a_max: self.a_max,
            harmonics,
            energy_total: 0.0,
            energy_kept: 0.0,
        }
    }

    /// Хеш детерминизма: FNV-1a по байтам VRTX-контейнера.
    pub fn water_hash(&self) -> u64 {
        vortex::fnv1a_u8(&vortex::encode(&self.to_vortex()))
    }

    /// Восстановление из контейнера. Физика мод пересчитывается из
    /// параметров (домен, дисперсия, вязкость); равновесие принимается
    /// равным текущему уровню — контейнер хранит видимый кадр спектра.
    pub fn from_vortex(
        spec: &vortex::VortexSpectrum,
        p: &WaterParams,
    ) -> Result<WaterSpectrum, String> {
        let n = spec.n as usize;
        if !n.is_power_of_two() || n < 16 {
            return Err("water: n контейнера — степень двойки ≥ 16".into());
        }
        if spec.phase_trits <= 3 {
            return Err("water: контейнер без микро-секторов фазы (нужны p+3 трита)".into());
        }
        let micro_total = 3i64.pow(spec.phase_trits);
        let half = (n / 2) as i64;
        let level_max = 3i64.pow(spec.amp_trits) - 1;
        let ln_per_level = (spec.a_max / spec.a_min).ln() / level_max as f64;
        if !(ln_per_level > 0.0) || !(spec.a_min > 0.0) {
            return Err("water: битое лог-окно амплитуд".into());
        }

        let mut bins: HashMap<(u32, u32), (u64, u64)> = HashMap::new();
        for h in &spec.harmonics {
            if bins.insert((h.kx, h.ky), (h.q, h.s)).is_some() {
                return Err("water: дубликат бина в контейнере".into());
            }
        }
        let mut modes = Vec::with_capacity(spec.harmonics.len() / 2);
        let mut used = HashSet::new();
        for h in &spec.harmonics {
            let wrap = |v: u32| -> i64 {
                let s = v as i64;
                if s > n as i64 / 2 {
                    s - n as i64
                } else {
                    s
                }
            };
            let kx_s = wrap(h.kx);
            let ky_s = wrap(h.ky);
            if !(1..half).contains(&kx_s) || ky_s.abs() >= half {
                // Неканонический представитель (−k) или частота Найквиста:
                // волну обработает пара с kx ∈ [1, n/2); сироты и самозеркала
                // поймает финальная проверка счёта бинов.
                continue;
            }
            if used.contains(&(h.kx, h.ky)) {
                continue;
            }
            let pkx = ((n as u32) - h.kx) % n as u32;
            let pky = ((n as u32) - h.ky) % n as u32;
            let partner = bins
                .get(&(pkx, pky))
                .ok_or_else(|| "water: нет эрмитовой пары ±k".to_string())?;
            if partner.0 != h.q {
                return Err("water: амплитуда эрмитовой пары не совпала".into());
            }
            let s_neg = (micro_total - h.s as i64).rem_euclid(micro_total);
            if partner.1 as i64 != s_neg {
                return Err("water: фаза эрмитовой пары не совпала".into());
            }
            used.insert((h.kx, h.ky));
            used.insert((pkx, pky));

            let kx = kx_s as i32;
            let ky = ky_s as i32;
            let k = TAU * ((kx_s * kx_s + ky_s * ky_s) as f64).sqrt() / p.domain;
            let omega = (p.gravity * k + p.capillary * k * k * k).sqrt();
            let u = (h.q as i64).clamp(0, level_max) as f64;
            let decay_rate =
                if ln_per_level > 0.0 { 2.0 * p.viscosity * k * k / ln_per_level } else { 0.0 };
            modes.push(WaterMode {
                kx,
                ky,
                acc: (h.q as i64).clamp(0, level_max) << 32,
                phase_fp: (h.s as i64 % micro_total) << 32,
                k,
                kx_phys: TAU * kx as f64 / p.domain,
                ky_phys: TAU * ky as f64 / p.domain,
                omega,
                u_eq: u, // равновесие = текущий уровень (кадр)
                decay_rate,
            });
        }
        if used.len() != spec.harmonics.len() {
            return Err("water: непарные бины в контейнере (сироты ±k)".into());
        }
        let var_target = {
            let hs = 0.21 * p.wind * p.wind / p.gravity;
            (hs / 4.0) * (hs / 4.0)
        };
        Ok(WaterSpectrum {
            n,
            amp_trits: spec.amp_trits,
            phase_trits: spec.phase_trits - 3,
            micro_total,
            a_min: spec.a_min,
            a_max: spec.a_max,
            level_max,
            ln_per_level,
            tau_wind: p.tau_wind,
            domain: p.domain,
            modes,
            steps: 0,
            variance_target: var_target,
        })
    }
}

// ---------------------------------------------------------------------------
// f64-эталон: те же уравнения, ноль квантования
// ---------------------------------------------------------------------------

/// Мода эталона: непрерывные амплитуда и фаза (f64).
#[derive(Debug, Clone)]
struct WaterModeF64 {
    kx: i32,
    ky: i32,
    /// Логарифм амплитуды в Place-единицах (непрерывный).
    ln_a_place: f64,
    /// Фаза, рад (f64, с завёрткой).
    phi: f64,
    omega: f64,
    /// Равновесный логарифм (окно ветра × K41).
    ln_eq: f64,
    decay_ln: f64,
}

/// Эталонная вода: та же физика без трит-квантования. Меряется PSNR
/// трит-воды против неё — честная цена квантования на кремнии.
#[derive(Debug, Clone)]
pub struct WaterF64 {
    n: usize,
    tau_wind: f64,
    modes: Vec<WaterModeF64>,
    steps: u64,
}

impl WaterF64 {
    /// Снять непрерывную копию с трит-спектра: начальные амплитуда и фаза
    /// совпадают с внутренним непрерывным состоянием трит-воды, так что
    /// расхождение дальше — только цена лестницы и секторов.
    pub fn from(s: &WaterSpectrum) -> Self {
        let modes = s
            .modes
            .iter()
            .map(|m| {
                let u_cont = (m.acc as f64 / FP).clamp(0.0, s.level_max as f64);
                WaterModeF64 {
                    kx: m.kx,
                    ky: m.ky,
                    ln_a_place: s.a_min.ln() + u_cont * s.ln_per_level,
                    phi: s.phase(m),
                    omega: m.omega,
                    ln_eq: s.a_min.ln() + m.u_eq * s.ln_per_level,
                    decay_ln: m.decay_rate * s.ln_per_level,
                }
            })
            .collect();
        WaterF64 { n: s.n, tau_wind: s.tau_wind, modes, steps: s.steps }
    }

    /// Шаг эталона: f32 НЕ используется даже здесь — честный f64.
    pub fn step(&mut self, dt: f64) {
        for m in &mut self.modes {
            m.phi = (m.phi - m.omega * dt).rem_euclid(TAU);
            let dln = (m.ln_eq - m.ln_a_place) / self.tau_wind - m.decay_ln;
            m.ln_a_place += dln * dt;
        }
        self.steps += 1;
    }

    fn amp_full(&self, m: &WaterModeF64) -> f64 {
        2.0 * m.ln_a_place.exp() / (self.n * self.n) as f64
    }

    /// Поле высот эталона (прямая сумма — без FFT, независимый путь).
    pub fn height_field(&self) -> Vec<f64> {
        let n = self.n;
        let nf = n as f64;
        let mut f = vec![0.0f64; n * n];
        for m in &self.modes {
            let a = self.amp_full(m);
            for y in 0..n {
                let base = TAU * m.ky as f64 * y as f64 / nf + m.phi;
                let row = y * n;
                for x in 0..n {
                    let arg = TAU * m.kx as f64 * x as f64 / nf + base;
                    f[row + x] += a * arg.cos();
                }
            }
        }
        f
    }

    /// Дисперсия высот эталона, м².
    pub fn variance(&self) -> f64 {
        self.modes
            .iter()
            .map(|m| {
                let a = self.amp_full(m);
                a * a / 2.0
            })
            .sum()
    }
}

// ---------------------------------------------------------------------------
// Метрики честности
// ---------------------------------------------------------------------------

/// Поле высот (метры) → u8: окно ±2.5σ целевой дисперсии.
/// Одна шкала для трит-воды и эталона — PSNR видит только разницу физики.
pub fn field_to_u8(field: &[f64], sigma: f64) -> Vec<u8> {
    let scale = if sigma > 1e-300 { 2.5 * sigma } else { 1.0 };
    field
        .iter()
        .map(|&h| ((h / scale + 0.5) * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect()
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn small(seed: u64, modes: usize) -> WaterParams {
        WaterParams { n: 64, modes, seed, ..Default::default() }
    }

    #[test]
    fn generation_modes_unique_directed() {
        let p = WaterParams { n: 256, modes: 192, seed: 42, ..Default::default() };
        let w = generate(&p);
        assert_eq!(w.modes.len(), p.modes, "все моды разместились");
        let mut uniq = HashSet::new();
        for m in &w.modes {
            assert!((1..128).contains(&m.kx), "kx={} вне канона", m.kx);
            assert!(m.ky.abs() < 128, "ky={} вне канона", m.ky);
            assert!(uniq.insert((m.kx, m.ky)), "дубликат бина");
            assert!(m.omega > 0.0 && m.k > 0.0 && m.u_eq >= 0.0 && m.decay_rate >= 0.0);
        }
        // Спектр при генерации уже нормирован на цель (квантование лестницей).
        let var = w.variance() / w.variance_target;
        assert!((0.8..=1.25).contains(&var), "дисперсия/цель = {var}");
        // Значимая высота согласуется с оценкой Пирсона–Московица.
        let hs_pm = 0.21 * p.wind * p.wind / p.gravity;
        let hs = w.significant_height();
        assert!((0.6..=1.4).contains(&(hs / hs_pm)), "H_s={hs} против PM {hs_pm}");
    }

    #[test]
    fn phase_counter_dispersion_exact() {
        // Одномодовое море: счётчик фазы обязан отщелкать ровно ω·dt
        // за шаг — с точностью округления константы (без накопления).
        let p = WaterParams { n: 64, modes: 1, seed: 3, wind: 6.0, ..Default::default() };
        let mut w = generate(&p);
        let m0 = w.modes[0].clone();
        let dt = 1.0 / 60.0;
        let steps = 1000i64;
        let fp0 = m0.phase_fp;
        for _ in 0..steps as usize {
            w.step(dt);
        }
        let adv = (m0.omega * dt * w.micro_total as f64 / TAU * FP).round() as i64;
        // Целочисленная аддитивность: ровно steps·adv, без дрейфа.
        let wrap = w.micro_total * FP_I;
        let expect = (fp0 - steps * adv).rem_euclid(wrap);
        assert_eq!(w.modes[0].phase_fp, expect, "счётчик дрейфанул");
        // Физика: видимая частота совпадает с дисперсионной
        // (adv живёт в фикс-точке микро-секторов — делим на 2³²).
        let omega_apparent = adv as f64 / FP * TAU / (w.micro_total as f64 * dt);
        let rel = ((omega_apparent - m0.omega) / m0.omega).abs();
        assert!(rel < 1e-8, "относительная ошибка частоты {rel}");
    }

    #[test]
    fn crest_moves_at_phase_speed() {
        // Гребень бегит по ветру. Вдоль y=0 косая волна движется со
        // скоростью ω/kx (фазовая скорость по оси x) — её и меряем.
        // У чистой косинусоиды гребни равновысоки: argmax ищем окном
        // ± полпериода от прошлого положения, без глобального шума.
        let p =
            WaterParams { n: 256, modes: 1, seed: 11, wind: 5.0, domain: 100.0, ..Default::default() };
        let mut w = generate(&p);
        let m = w.modes[0].clone();
        let c = m.omega / m.kx_phys; // скорость гребня вдоль оси x, м/с
        let dt = 0.1;
        let cell = p.domain / p.n as f64;
        let period_cells = p.n as f64 / m.kx as f64;
        let win = (period_cells / 2.0).ceil() as i64;

        let crest_global = |w: &WaterSpectrum| -> f64 {
            let n = w.n as i64;
            let mut best = 0i64;
            let mut bh = f64::NEG_INFINITY;
            for x in 0..n {
                let h = w.height_at(x as usize, 0);
                if h > bh {
                    bh = h;
                    best = x;
                }
            }
            best as f64
        };
        let crest_near = |w: &WaterSpectrum, prev: f64| -> f64 {
            let n = w.n as i64;
            let center = prev.round() as i64;
            let mut best = center;
            let mut bh = f64::NEG_INFINITY;
            for di in -win..=win {
                let x = (center + di).rem_euclid(n) as usize;
                let h = w.height_at(x, 0);
                if h > bh {
                    bh = h;
                    best = (center + di).rem_euclid(n);
                }
            }
            // Параболическая интерполяция — субпиксельная точность.
            let at = |i: i64| w.height_at(i.rem_euclid(n) as usize, 0);
            let bh2 = at(best);
            let hm = at(best - 1);
            let hp = at(best + 1);
            let denom = hm - 2.0 * bh2 + hp;
            let shift = if denom.abs() > 1e-300 { 0.5 * (hm - hp) / denom } else { 0.0 };
            best as f64 + shift
        };

        let mut pos = crest_global(&w);
        let mut dist = 0.0f64;
        let n_steps = 30;
        for _ in 0..n_steps {
            w.step(dt);
            let np = crest_near(&w, pos);
            let mut d = (np - pos) * cell;
            if d < -p.domain / 2.0 {
                d += p.domain;
            }
            if d > p.domain / 2.0 {
                d -= p.domain;
            }
            dist += d;
            pos = np;
        }
        let c_est = dist / (n_steps as f64 * dt);
        assert!(c_est > 0.0, "волна побежала против ветра: {c_est}");
        assert!((c_est - c).abs() / c < 0.15, "c_est={c_est} против c={c}");
    }

    #[test]
    fn viscous_decay_matches_lamb() {
        // Затухание Ламба A ∝ e^(−2νk²t): в лог-уровнях — линейный спуск.
        let p = WaterParams {
            n: 64,
            modes: 1,
            seed: 5,
            wind: 4.0,
            viscosity: 0.05,
            tau_wind: 3600.0, // ветер почти выключен
            ..Default::default()
        };
        let mut w = generate(&p);
        let m = w.modes[0].clone();
        assert!(m.decay_rate > 0.0, "для этого k вязкость должна действовать");
        let target_drop = 4.0f64; // уровней лестницы
        let t_total = target_drop / m.decay_rate;
        let n_steps = 200;
        let dt = t_total / n_steps as f64;
        let u0 = w.modes[0].acc >> 32;
        for _ in 0..n_steps {
            w.step(dt);
        }
        let drop = (u0 - (w.modes[0].acc >> 32)) as f64;
        // Лестница + округления: допуск полтора уровня.
        assert!((drop - target_drop).abs() <= 1.5, "спуск {drop} против {target_drop}");
    }

    #[test]
    fn wind_builds_equilibrium_band() {
        // Из штиля ветер накачивает море; после 6τ — стационарная полоса.
        let p =
            WaterParams { n: 64, modes: 96, seed: 7, wind: 8.0, tau_wind: 5.0, ..Default::default() };
        let mut w = generate(&p);
        for m in &mut w.modes {
            m.acc = (m.acc / 20).max(1); // штиль: ~5% уровней
        }
        assert!(w.variance() / w.variance_target < 0.1, "это ещё штиль");
        let dt = 1.0 / 60.0;
        for _ in 0..(6.0 * p.tau_wind / dt) as usize {
            w.step(dt);
        }
        let var = w.variance() / w.variance_target;
        assert!((0.4..=1.6).contains(&var), "море не развилось: {var}");
        // Ещё 6τ — полоса держится (нет ни разгона, ни затухания).
        for _ in 0..(6.0 * p.tau_wind / dt) as usize {
            w.step(dt);
        }
        let var2 = w.variance() / w.variance_target;
        assert!((0.4..=1.6).contains(&var2), "полоса не держится: {var2}");
    }

    #[test]
    fn f32_drifts_trit_counter_does_not() {
        // Главная демонрация кремния: одна и та же динамика фазы —
        // f32-аккумулятор (как в сеточных движках) против целочисленного
        // счётчика фикс-точки. 3 млн шагов.
        let omega = 2.0f64;
        let dt = 1.0f64 / 60.0;
        let micro = 19683i64; // 3^9
        let steps = 3_000_000i64;
        let sector = TAU / micro as f64;

        // f32: погрешность округления растёт вместе с величиной фазы.
        let step32 = (omega * dt) as f32;
        let mut ph32 = 0.0f32;
        for _ in 0..steps {
            ph32 -= step32;
        }
        let exact = omega * dt * steps as f64;
        let err32 = (ph32 as f64 + exact).abs();

        // Трит-счётчик: целочисленные вычитания точны по построению;
        // вся «цена» — округление константы шага (≤ 2⁻³³ сектора).
        let adv = (omega * dt * micro as f64 / TAU * FP).round() as i64;
        let wrap = micro * FP_I;
        let mut fp = 0i64;
        for _ in 0..steps {
            fp = (fp - adv).rem_euclid(wrap);
        }
        let expect = (-(steps) * adv).rem_euclid(wrap);
        assert_eq!(fp, expect, "целочисленная аддитивность нарушена?!");
        // Ошибка квантования константы, набежавшая за 3 млн шагов:
        let trit_err_micro = (steps as f64 * (adv as f64 / FP
            - omega * dt * micro as f64 / TAU)).abs();
        let trit_err_rad = trit_err_micro * TAU / micro as f64;

        assert!(trit_err_rad < 1e-4, "дрейф трита {trit_err_rad} рад");
        assert!(err32 > 100.0 * sector, "f32 дрейф {err32} рад мал сверх ожидания");
        assert!(err32 > 1e4 * trit_err_rad, "разрыв не убедителен: {err32} против {trit_err_rad}");
    }

    #[test]
    fn determinism_bit_to_bit() {
        let run = |seed: u64| {
            let p = WaterParams { n: 64, modes: 48, seed, ..Default::default() };
            let mut w = generate(&p);
            for _ in 0..100 {
                w.step(1.0 / 60.0);
            }
            let f = w.height_field();
            (w.water_hash(), vortex::fnv1a_u8(&field_to_u8(&f, w.variance_target.sqrt())))
        };
        let a = run(21);
        let b = run(21);
        let c = run(22);
        assert_eq!(a, b, "бит-в-бит детерминизм");
        assert_ne!(a, c, "разные сиды — разные моря");
    }

    #[test]
    fn vrtx_roundtrip_exact() {
        let p = small(9, 24);
        let mut w = generate(&p);
        for _ in 0..50 {
            w.step(1.0 / 60.0);
        }
        let bytes = vortex::encode(&w.to_vortex());
        let spec = vortex::decode(&bytes).expect("контейнер воды декодируется");
        let w2 = WaterSpectrum::from_vortex(&spec, &p).expect("пары ±k собираются");
        assert_eq!(w2.modes.len(), w.modes.len());
        let f1 = w.height_field();
        let f2 = w2.height_field();
        for (a, b) in f1.iter().zip(f2.iter()) {
            assert_eq!(a.to_bits(), b.to_bits(), "восстановление не бит-в-бит");
        }
    }

    #[test]
    fn height_at_matches_height_field() {
        let w = generate(&small(13, 24));
        let f = w.height_field();
        let n = 64;
        for y in (0..n).step_by(7) {
            for x in (0..n).step_by(5) {
                let d = (w.height_at(x, y) - f[y * n + x]).abs();
                assert!(d < 1e-9, "({x},{y}): расхождение {d}");
            }
        }
    }

    #[test]
    fn flow_field_is_irrotational() {
        // Линейная глубоководная вода — потенциальное течение:
        // ротор горизонтального поля обязан быть нулём.
        let w = generate(&small(17, 24));
        let h = 0.05; // шаг центральной разности, м
        let mut max_curl = 0.0f64;
        let mut max_u = 0.0f64;
        for i in 0..10 {
            for j in 0..10 {
                let x = 5.0 + i as f64 * 9.0;
                let y = 5.0 + j as f64 * 9.0;
                let (_, uy1, _) = w.flow_at(x - h, y);
                let (_, uy2, _) = w.flow_at(x + h, y);
                let (ux1, _, _) = w.flow_at(x, y - h);
                let (ux2, _, _) = w.flow_at(x, y + h);
                let curl = (uy2 - uy1) / (2.0 * h) - (ux2 - ux1) / (2.0 * h);
                max_curl = max_curl.max(curl.abs());
                let (ux, uy, w_) = w.flow_at(x, y);
                max_u = max_u.max(ux.abs().max(uy.abs()).max(w_.abs()));
            }
        }
        assert!(max_u > 0.0, "течение нулевое — тест слепой");
        assert!(max_curl < 0.05 * max_u, "curl {max_curl} при скорости {max_u}");
    }

    #[test]
    fn psnr_against_f64_reference() {
        // Честная цена квантования: трит-вода против f64-эталона
        // с теми же уравнениями. 4 секунды модельного времени.
        let p = WaterParams { n: 64, modes: 48, seed: 42, ..Default::default() };
        let mut w = generate(&p);
        let mut r = WaterF64::from(&w);
        let dt = 1.0 / 60.0;
        for _ in 0..240 {
            w.step(dt);
            r.step(dt);
        }
        let sigma = w.variance_target.sqrt();
        let fa = field_to_u8(&w.height_field(), sigma);
        let fb = field_to_u8(&r.height_field(), sigma);
        let psnr = vortex::psnr_u8(&fa, &fb);
        assert!(psnr > 35.0, "PSNR трит-воды против эталона: {psnr}");
        // Дисперсии согласуются — энергия не потерялась и не приплыла.
        let rel = ((w.variance() - r.variance()) / r.variance()).abs();
        assert!(rel < 0.05, "дисперсии разошлись: {rel}");
    }

    #[test]
    fn state_memory_small_vs_grid() {
        // Состояние в байтах против f32-сетки той же детализации.
        let w = generate(&WaterParams { n: 256, modes: 192, ..Default::default() });
        let bytes = vortex::encode(&w.to_vortex()).len();
        let grid = 256 * 256 * 4;
        assert!(bytes * 100 < grid, "состояние {bytes} Б против сетки {grid} Б");
    }
}
