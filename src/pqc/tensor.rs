//! Тензорное ядро `pqc` — нативные SIMD-кернелы инференса (Part E/F).
//!
//! Здесь живёт ВСЯ математика прямого прохода: скалярные произведения
//! (fp32, int8, int4), нормы (LayerNorm для энкодера BERT-класса,
//! RMSNorm для GLM-декодера), активации (GELU-erf, SiLU/Swish),
//! softmax, RoPE-таблицы. Ни одной внешней ML-библиотеки: только
//! `core::arch` из std и runtime-детект CPU (тот же канон, что у
//! Teddy в retrieval-слое).
//!
//! ## Канон детерминизма
//!
//! Рантайм выбирает РОВНО ОДИН путь исполнения на процесс (детект
//! кэшируется `OnceLock`), порядок аккумуляции внутри пути фиксирован —
//! инференс побитово воспроизводим на данной машине (инвариант кирпича
//! RaBitQ/HNSW распространён на нейрослой).
//!
//! ## Квантование весов (симметричное, на строку)
//!
//! * **int8**: `scale = max|w_row| / 127`, код ∈ [-127, 127];
//!   матвектор: `out[r] = scale_r · Σ q[r,i]·x[i]` — int8-строка
//!   конвертируется в f32 регистрами AVX2 (`cvtepi8_epi32 → cvtepi32_ps
//!   → fmadd`), активации остаются fp32 (weight-only quantization).
//! * **int4**: `scale = max|w_row| / 7`, код ∈ [-7, 7], упаковка два
//!   nibble на байт (`lo = байт & 0xF`, `hi = байт >> 4`, значение =
//!   `nibble − 8`). Распаковка в горячем цикле.

use std::sync::OnceLock;

// ---------------------------------------------------------------------------
// Детект CPU
// ---------------------------------------------------------------------------

/// Доступен ли AVX2+FMA на этой машине (кэш на процесс).
pub fn avx2() -> bool {
    static CELL: OnceLock<bool> = OnceLock::new();
    *CELL.get_or_init(|| {
        #[cfg(target_arch = "x86_64")]
        {
            std::arch::is_x86_feature_detected!("avx2") && std::arch::is_x86_feature_detected!("fma")
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    })
}

// ---------------------------------------------------------------------------
// Скалярные произведения
// ---------------------------------------------------------------------------

/// Скалярное произведение fp32 (AVX2+FMA при наличии, иначе скаляр).
pub fn dot_f32(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len(), "dot_f32: длины не совпадают");
    #[cfg(target_arch = "x86_64")]
    {
        if avx2() {
            return unsafe { dot_f32_avx2(a, b) };
        }
    }
    dot_f32_scalar(a, b)
}

/// Скалярное произведение int8-строки весов с fp32-вектором активаций.
///
/// Кернел weight-only int8: веса живут байтом, активации — float;
/// конвертация int8→f32 делается SIMD-инструкциями прямо в регистрах.
pub fn dot_i8_f32(q: &[i8], x: &[f32]) -> f32 {
    debug_assert_eq!(q.len(), x.len(), "dot_i8_f32: длины не совпадают");
    #[cfg(target_arch = "x86_64")]
    {
        if avx2() {
            return unsafe { dot_i8_f32_avx2(q, x) };
        }
    }
    dot_i8_f32_scalar(q, x)
}

/// Скалярное произведение int4-строки (упакованные nibble) с fp32-вектором.
///
/// Распаковка в горячем цикле: `v = nibble − 8 ∈ [-8, 7]`. Портативный
/// путь (AVX2-ниббл-трюк — оптимизация следующих кирпичей: int4 нужен
/// GLM-декодеру, энкодер ездит на int8).
pub fn dot_i4_f32(packed: &[u8], x: &[f32], n: usize) -> f32 {
    debug_assert_eq!(packed.len(), (n + 1) / 2, "dot_i4_f32: размер упаковки");
    let mut s = 0f32;
    for (i, &b) in packed.iter().enumerate() {
        let lo = (b & 0xF) as i32 - 8;
        let hi = (b >> 4) as i32 - 8;
        let x0 = if i * 2 < n { x[i * 2] } else { 0.0 };
        let x1 = if i * 2 + 1 < n { x[i * 2 + 1] } else { 0.0 };
        s += lo as f32 * x0 + hi as f32 * x1;
    }
    s
}

#[inline]
fn dot_f32_scalar(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

#[inline]
fn dot_i8_f32_scalar(q: &[i8], x: &[f32]) -> f32 {
    let mut s = 0f32;
    for (w, v) in q.iter().zip(x) {
        s += *w as f32 * *v;
    }
    s
}

// ---------------------------------------------------------------------------
// AVX2-кернелы
// ---------------------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn dot_f32_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let mut acc = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= n {
        let va = _mm256_loadu_ps(a.as_ptr().add(i));
        let vb = _mm256_loadu_ps(b.as_ptr().add(i));
        acc = _mm256_fmadd_ps(va, vb, acc);
        i += 8;
    }
    let mut sum = hsum256(acc);
    while i < n {
        sum += a[i] * b[i];
        i += 1;
    }
    sum
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2", enable = "fma")]
unsafe fn dot_i8_f32_avx2(q: &[i8], x: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = q.len();
    let mut acc = _mm256_setzero_ps();
    let mut i = 0;
    while i + 8 <= n {
        // 8 int8-весов → 8 int32 → 8 float, FMA с активациями.
        let w8 = _mm_loadl_epi64(q.as_ptr().add(i) as *const __m128i);
        let w32 = _mm256_cvtepi8_epi32(w8);
        let wf = _mm256_cvtepi32_ps(w32);
        let xf = _mm256_loadu_ps(x.as_ptr().add(i));
        acc = _mm256_fmadd_ps(wf, xf, acc);
        i += 8;
    }
    let mut sum = hsum256(acc);
    while i < n {
        sum += q[i] as f32 * x[i];
        i += 1;
    }
    sum
}

/// Горизонтальная сумма `__m256` с фиксированным порядком (детерминизм).
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hsum256(v: std::arch::x86_64::__m256) -> f32 {
    use std::arch::x86_64::*;
    let lo = _mm256_castps256_ps128(v);
    let hi = _mm256_extractf128_ps(v, 1);
    let s = _mm_add_ps(lo, hi);
    let mut arr = [0f32; 4];
    _mm_storeu_ps(arr.as_mut_ptr(), s);
    ((arr[0] + arr[1]) + (arr[2] + arr[3])) as f32
}

// ---------------------------------------------------------------------------
// Нормы
// ---------------------------------------------------------------------------

/// LayerNorm (BERT-класс, post-norm): `(x − μ)/σ · γ + β`, in-place.
///
/// σ — популяционная (ddof = 0), как в исходном BERT/XLM-R.
pub fn layer_norm(x: &mut [f32], gamma: &[f32], beta: &[f32], eps: f32) {
    debug_assert_eq!(x.len(), gamma.len());
    debug_assert_eq!(x.len(), beta.len());
    let n = x.len();
    if n == 0 {
        return;
    }
    let mean = x.iter().sum::<f32>() / n as f32;
    let mut var = 0f32;
    for v in x.iter() {
        let d = v - mean;
        var += d * d;
    }
    var /= n as f32;
    let inv = 1.0 / (var + eps).sqrt();
    for i in 0..n {
        x[i] = (x[i] - mean) * inv * gamma[i] + beta[i];
    }
}

/// RMSNorm (GLM-класс, pre-norm): `x / √(mean(x²)+ε) · γ`.
///
/// Работает в отдельный выход (residual-ветка декодера требует исходный x).
pub fn rms_norm(x: &[f32], gamma: &[f32], eps: f32, out: &mut [f32]) {
    debug_assert_eq!(x.len(), gamma.len());
    debug_assert_eq!(x.len(), out.len());
    let n = x.len();
    if n == 0 {
        return;
    }
    let mut ms = 0f32;
    for v in x.iter() {
        ms += v * v;
    }
    let inv = 1.0 / (ms / n as f32 + eps).sqrt();
    for i in 0..n {
        out[i] = x[i] * inv * gamma[i];
    }
}

/// L2-нормализация in-place (CLS-пулинг BGE-M3: косинус == IP).
pub fn l2_normalize(x: &mut [f32]) {
    let mut s = 0f32;
    for v in x.iter() {
        s += v * v;
    }
    let n = s.sqrt();
    if n > 0.0 {
        for v in x.iter_mut() {
            *v /= n;
        }
    }
}

// ---------------------------------------------------------------------------
// Активации
// ---------------------------------------------------------------------------

/// erf(x) — аппроксимация Абрамовица–Стегуна 7.1.26 (|err| < 1.5e-7),
/// достаточно точно против fp32-округления GELU-эталона.
pub fn erf(x: f32) -> f32 {
    let sign = if x < 0.0 { -1.0f32 } else { 1.0 };
    let ax = x.abs();
    let t = 1.0 / (1.0 + 0.327_591_1 * ax);
    let y = 1.0
        - (((((1.061_405_429 * t - 1.453_152_027) * t) + 1.421_413_741) * t
            - 0.284_496_736)
            * t
            + 0.254_829_592)
            * t
            * (-ax * ax).exp();
    sign * y
}

/// Точный GELU (erf-форма, как в BERT/XLM-R/BGE-M3): `x·Φ(x)`.
pub fn gelu(v: f32) -> f32 {
    0.5 * v * (1.0 + erf(v * std::f32::consts::FRAC_1_SQRT_2))
}

/// GELU по всему срезу in-place.
pub fn gelu_inplace(x: &mut [f32]) {
    for v in x.iter_mut() {
        *v = gelu(*v);
    }
}

/// SiLU/Swish: `x·sigmoid(x)` (ветка gate в SwiGLU-FFN декодера).
pub fn silu(v: f32) -> f32 {
    v / (1.0 + (-v).exp())
}

/// SiLU по всему срезу in-place.
pub fn silu_inplace(x: &mut [f32]) {
    for v in x.iter_mut() {
        *v = silu(*v);
    }
}

// ---------------------------------------------------------------------------
// Softmax / выбор
// ---------------------------------------------------------------------------

/// Численно стабильный softmax in-place (сдвиг максимума).
pub fn softmax_inplace(x: &mut [f32]) {
    if x.is_empty() {
        return;
    }
    let m = x.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let mut z = 0f32;
    for v in x.iter_mut() {
        *v = (*v - m).exp();
        z += *v;
    }
    if z > 0.0 {
        for v in x.iter_mut() {
            *v /= z;
        }
    }
}

/// Индекс максимума (ties → младший индекс, детерминизм).
pub fn argmax(x: &[f32]) -> usize {
    debug_assert!(!x.is_empty(), "argmax: пустой срез");
    let mut best = 0usize;
    let mut bv = x[0];
    for (i, &v) in x.iter().enumerate().skip(1) {
        if v > bv {
            bv = v;
            best = i;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// RoPE (Rotary Position Embedding)
// ---------------------------------------------------------------------------

/// Таблицы RoPE: предвычисленные cos/sin на каждую позицию и пару
/// координат. Никаких тяжёлых тригонометрических вызовов в рантайме —
/// только табличные lookups (быстрее CORDIC, требование Part F).
pub struct RopeTable {
    head_dim: usize,
    max_pos: usize,
    cos: Vec<f32>,
    sin: Vec<f32>,
}

impl RopeTable {
    /// Частоты `inv_freq[i] = 10000^(−2i/d)`, угол = pos · inv_freq[i].
    pub fn new(head_dim: usize, max_pos: usize) -> Self {
        debug_assert!(head_dim % 2 == 0, "RoPE: head_dim должен быть чётным");
        let pairs = head_dim / 2;
        let mut cos = vec![0f32; max_pos * pairs];
        let mut sin = vec![0f32; max_pos * pairs];
        for pos in 0..max_pos {
            for i in 0..pairs {
                let freq = (10000f64).powf(-(2.0 * i as f64) / head_dim as f64);
                let angle = pos as f64 * freq;
                cos[pos * pairs + i] = angle.cos() as f32;
                sin[pos * pairs + i] = angle.sin() as f32;
            }
        }
        Self {
            head_dim,
            max_pos,
            cos,
            sin,
        }
    }

    pub fn head_dim(&self) -> usize {
        self.head_dim
    }

    pub fn max_pos(&self) -> usize {
        self.max_pos
    }

    /// Поворот головы на +pos in-place: пары (2i, 2i+1) вращаются как
    /// комплексные числа `x + iy → (x+iy)·e^{iθ}`.
    pub fn rotate(&self, x: &mut [f32], pos: usize) {
        self.rotate_signed(x, pos, 1.0)
    }

    /// Обратный поворот (−pos): conjugate `e^{−iθ}`. Инвариант
    /// roundtrip: rotate(rotate(x, p), p, −1) == x.
    pub fn rotate_inv(&self, x: &mut [f32], pos: usize) {
        self.rotate_signed(x, pos, -1.0)
    }

    fn rotate_signed(&self, x: &mut [f32], pos: usize, sign: f32) {
        debug_assert_eq!(x.len(), self.head_dim, "RoPE: длина не равна head_dim");
        let pairs = self.head_dim / 2;
        for i in 0..pairs {
            let c = self.cos[pos * pairs + i];
            let s = sign * self.sin[pos * pairs + i];
            let a = x[2 * i];
            let b = x[2 * i + 1];
            x[2 * i] = a * c - b * s;
            x[2 * i + 1] = a * s + b * c;
        }
    }
}

// ---------------------------------------------------------------------------
// Квантование (для .pqw-билдера и конвертера)
// ---------------------------------------------------------------------------

/// Симметричное int8-квантование по строкам: `(коды, масштабы)`.
pub fn quant_i8_per_row(w: &[f32], rows: usize, cols: usize) -> (Vec<i8>, Vec<f32>) {
    debug_assert_eq!(w.len(), rows * cols);
    let mut q = vec![0i8; rows * cols];
    let mut scales = vec![1f32; rows];
    for r in 0..rows {
        let row = &w[r * cols..(r + 1) * cols];
        let m = row.iter().fold(0f32, |a, v| a.max(v.abs()));
        let sc = if m > 0.0 { m / 127.0 } else { 1.0 };
        scales[r] = sc;
        for (i, &v) in row.iter().enumerate() {
            q[r * cols + i] = (v / sc).round().clamp(-127.0, 127.0) as i8;
        }
    }
    (q, scales)
}

/// Симметричное int4-квантование по строкам: `(упакованные байты, масштабы)`.
///
/// Код ∈ [-7, 7], nibble = код + 8 ∈ [1, 15]; чётный элемент — младший
/// nibble, нечётный — старший.
pub fn quant_i4_per_row(w: &[f32], rows: usize, cols: usize) -> (Vec<u8>, Vec<f32>) {
    debug_assert_eq!(w.len(), rows * cols);
    let packed_cols = (cols + 1) / 2;
    let mut q = vec![0u8; rows * packed_cols];
    let mut scales = vec![1f32; rows];
    for r in 0..rows {
        let row = &w[r * cols..(r + 1) * cols];
        let m = row.iter().fold(0f32, |a, v| a.max(v.abs()));
        let sc = if m > 0.0 { m / 7.0 } else { 1.0 };
        scales[r] = sc;
        for (i, &v) in row.iter().enumerate() {
            let code = (v / sc).round().clamp(-7.0, 7.0) as i32 + 8;
            let byte = &mut q[r * packed_cols + i / 2];
            if i % 2 == 0 {
                *byte = (*byte & 0xF0) | (code as u8 & 0x0F);
            } else {
                *byte = (*byte & 0x0F) | ((code as u8) << 4);
            }
        }
    }
    (q, scales)
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn rel_close(a: f32, b: f32, eps: f32) -> bool {
        (a - b).abs() <= eps * b.abs().max(1.0)
    }

    #[test]
    fn dot_f32_matches_scalar() {
        let a: Vec<f32> = (0..257u32).map(|i| (i % 13) as f32 * 0.25 - 1.5).collect();
        let b: Vec<f32> = (0..257u32).map(|i| (i % 7) as f32 * 0.5 - 1.0).collect();
        let scalar = dot_f32_scalar(&a, &b);
        let dispatched = dot_f32(&a, &b);
        assert!(
            rel_close(dispatched, scalar, 1e-5),
            "dispatched={dispatched} scalar={scalar}"
        );
    }

    #[test]
    fn dot_i8_matches_scalar() {
        let q: Vec<i8> = (0..301i32).map(|i| (i % 51 - 25) as i8).collect();
        let x: Vec<f32> = (0..301u32).map(|i| (i % 11) as f32 * 0.3 - 1.2).collect();
        let scalar = dot_i8_f32_scalar(&q, &x);
        let dispatched = dot_i8_f32(&q, &x);
        assert!(
            rel_close(dispatched, scalar, 1e-5),
            "dispatched={dispatched} scalar={scalar}"
        );
    }

    #[test]
    fn dot_i4_roundtrip_and_value() {
        // Квантование → распаковка: ошибка ≤ scale/2 на элемент.
        let w: Vec<f32> = (0..64u32).map(|i| ((i * 37 % 19) as f32) / 19.0 - 0.5).collect();
        let (packed, scales) = quant_i4_per_row(&w, 1, 64);
        assert_eq!(packed.len(), 32);
        // Восстановим строку через dot с базисными векторами e_i.
        for i in 0..64 {
            let mut e = vec![0f32; 64];
            e[i] = 1.0;
            let restored = scales[0] * dot_i4_f32(&packed, &e, 64);
            assert!(
                (restored - w[i]).abs() <= scales[0] / 2.0 + 1e-6,
                "i={i}: {restored} vs {}",
                w[i]
            );
        }
        // Пустой хвост упаковки (нечётная длина).
        let (p2, _) = quant_i4_per_row(&[0.5, -0.5, 0.25], 1, 3);
        assert_eq!(p2.len(), 2);
    }

    #[test]
    fn quant_i8_roundtrip_error() {
        let w: Vec<f32> = (0..1000u32).map(|i| ((i * 73 % 41) as f32 / 41.0) - 0.5).collect();
        let (q, s) = quant_i8_per_row(&w, 10, 100);
        for r in 0..10 {
            for i in 0..100 {
                let restored = s[r] * q[r * 100 + i] as f32;
                assert!(
                    (restored - w[r * 100 + i]).abs() <= s[r] / 2.0 + 1e-6,
                    "r={r} i={i}"
                );
            }
        }
    }

    #[test]
    fn layer_norm_zero_mean_unit_var() {
        let gamma = vec![1.0f32; 16];
        let beta = vec![0.0f32; 16];
        let mut x: Vec<f32> = (0..16u32).map(|i| (i as f32) * 3.0 - 20.0).collect();
        layer_norm(&mut x, &gamma, &beta, 1e-5);
        let mean = x.iter().sum::<f32>() / 16.0;
        let var = x.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / 16.0;
        assert!(mean.abs() < 1e-4, "mean={mean}");
        assert!((var - 1.0).abs() < 1e-3, "var={var}");
    }

    #[test]
    fn rms_norm_matches_reference() {
        let x: Vec<f32> = (0..32u32).map(|i| (i as f32) * 0.1 - 1.5).collect();
        let gamma: Vec<f32> = (0..32u32).map(|i| 1.0 + (i % 3) as f32 * 0.1).collect();
        let mut out = vec![0f32; 32];
        rms_norm(&x, &gamma, 1e-6, &mut out);
        let ms = x.iter().map(|v| v * v).sum::<f32>() / 32.0;
        let inv = 1.0 / (ms + 1e-6).sqrt();
        for i in 0..32 {
            let expect = x[i] * inv * gamma[i];
            assert!((out[i] - expect).abs() < 1e-5, "i={i}");
        }
    }

    #[test]
    fn gelu_known_values() {
        assert!(gelu(0.0).abs() < 1e-6);
        assert!((gelu(1.0) - 0.841_344_7).abs() < 1e-4, "gelu(1)={}", gelu(1.0));
        assert!((gelu(-1.0) + 0.158_655_3).abs() < 1e-4);
        // GELU ≈ ReLU сверху, мягкий хвост снизу.
        assert!(gelu(5.0) > 4.99);
        assert!(gelu(-5.0) > -1e-5 && gelu(-5.0) < 0.0);
    }

    #[test]
    fn softmax_properties() {
        let mut x = vec![1.0f32, 2.0, 3.0, -1.0, 0.5];
        softmax_inplace(&mut x);
        let s = x.iter().sum::<f32>();
        assert!((s - 1.0).abs() < 1e-5);
        assert!(x[2] > x[1] && x[1] > x[0] && x[0] > x[4] && x[4] > x[3]);
        // Сдвиго-инвариантность.
        let mut y = vec![101.0f32, 102.0, 103.0, 99.0, 100.5];
        softmax_inplace(&mut y);
        for i in 0..5 {
            assert!((x[i] - y[i]).abs() < 1e-5);
        }
    }

    #[test]
    fn argmax_tie_breaks_to_lowest() {
        assert_eq!(argmax(&[1.0, 3.0, 3.0, 2.0]), 1);
        assert_eq!(argmax(&[5.0, 5.0]), 0);
    }

    #[test]
    fn rope_identity_and_roundtrip() {
        let rope = RopeTable::new(16, 64);
        let x: Vec<f32> = (0..16u32).map(|i| (i as f32) * 0.25 - 2.0).collect();
        // Позиция 0 — тождество (cos=1, sin=0).
        let mut y = x.clone();
        rope.rotate(&mut y, 0);
        assert_eq!(y, x);
        // Прямой + обратный поворот = тождество (комплексное сопряжение).
        for pos in [1usize, 7, 31, 63] {
            let mut z = x.clone();
            rope.rotate(&mut z, pos);
            rope.rotate_inv(&mut z, pos);
            for i in 0..16 {
                assert!((z[i] - x[i]).abs() < 1e-5, "pos={pos} i={i}: {} vs {}", z[i], x[i]);
            }
        }
    }

    #[test]
    fn rope_preserves_norm() {
        // Вращение — унитарная операция: норма вектора не меняется.
        let rope = RopeTable::new(8, 32);
        let x: Vec<f32> = vec![0.3, -1.2, 0.7, 2.0, -0.5, 0.1, 1.4, -0.9];
        let norm = x.iter().map(|v| v * v).sum::<f32>().sqrt();
        let mut y = x.clone();
        rope.rotate(&mut y, 17);
        let norm2 = y.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((norm - norm2).abs() < 1e-5);
    }

    #[test]
    fn rope_relative_property() {
        // Ключевое свойство RoPE: ⟨q_rot(p), k_rot(m)⟩ зависит только от p−m.
        let rope = RopeTable::new(8, 128);
        let q = vec![0.5f32, -0.3, 1.0, 0.7, -0.2, 0.9, 0.1, -0.6];
        let k = vec![0.4f32, 0.2, -0.8, 0.5, 0.3, -1.0, 0.6, 0.2];
        let mut qp = q.clone();
        rope.rotate(&mut qp, 10);
        let mut km = k.clone();
        rope.rotate(&mut km, 25);
        let a = dot_f32(&qp, &km);
        let mut qp2 = q.clone();
        rope.rotate(&mut qp2, 50);
        let mut km2 = k.clone();
        rope.rotate(&mut km2, 65);
        let b = dot_f32(&qp2, &km2);
        assert!((a - b).abs() < 1e-4, "RoPE относительность нарушена: {a} vs {b}");
    }

    #[test]
    fn l2_normalize_unit() {
        let mut x = vec![3.0f32, 4.0];
        l2_normalize(&mut x);
        let n = x.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-6);
        let mut z = vec![0.0f32, 0.0];
        l2_normalize(&mut z); // не паникует на нуле
        assert_eq!(z, vec![0.0, 0.0]);
    }

    #[test]
    fn silu_known_values() {
        assert!(silu(0.0).abs() < 1e-7);
        assert!((silu(1.0) - 0.731_058_6).abs() < 1e-5);
        assert!((silu(-1.0) + 0.268_941_4).abs() < 1e-5);
        assert!(silu(10.0) > 9.999);
    }
}
