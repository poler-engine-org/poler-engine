//! Реестр функций калькулятора (цикл M, v0.48.0).
//!
//! РЕГРЕССИИ прошлой сессии, закрытые здесь:
//! - erf: Хорнер с правильными знаками И внешним множителем t
//!   (формула A&S 7.1.26, точность 1.5e-7);
//! - zeta: Эйлер–Маклорен вместо расходящегося ряда (ζ(2) = π²/6 и
//!   ζ(−1) = −1/12 сходятся);
//! - гамма: Ланцрош g=7, n=9.

use super::astro;
use super::geodesy;
use super::matrix::Matrix;
use super::numbers;
use super::solve::Complex;
use super::trits::Trits;
use super::units;
use super::units::Unit;
use super::viz;
use super::Value;

// ---------------------------------------------------------------------
// Скалярные помощники
// ---------------------------------------------------------------------

/// Скалярный аргумент: числа и безразмерные величины.
fn scalar_arg(args: &[Value], i: usize) -> Result<f64, String> {
    match args.get(i) {
        Some(Value::Scalar(v)) => Ok(*v),
        Some(Value::Quantity(q, u)) if u.is_dimensionless() => Ok(*q),
        Some(other) => Err(format!("аргумент {} должен быть числом, получено {other}", i + 1)),
        None => Err(format!("не хватает аргументов (нужен аргумент {})", i + 1)),
    }
}

/// Угловой аргумент: число = радианы; величина угловой размерности — конверсия.
fn angle_arg(args: &[Value], i: usize) -> Result<f64, String> {
    match args.get(i) {
        Some(Value::Scalar(v)) => Ok(*v),
        Some(Value::Quantity(q, u)) if u.dim[units::DIM_ANGLE] == 1
            && u.dim.iter().enumerate().all(|(k, &d)| k == units::DIM_ANGLE || d == 0) =>
        {
            let rad = units::by_name("rad").unwrap();
            Ok(units::convert(*q, u, &rad)?)
        }
        Some(other) => Err(format!(
            "аргумент {} должен быть углом (радианы или deg/rad/turn): {other}",
            i + 1
        )),
        None => Err(format!("не хватает аргументов (нужен аргумент {})", i + 1)),
    }
}

fn need(args: &[Value], n: usize, name: &str) -> Result<(), String> {
    if args.len() != n {
        Err(format!(
            "{name}: ожидается {} аргумент(ов), получено {}",
            n,
            args.len()
        ))
    } else {
        Ok(())
    }
}

fn one_arg(args: &[Value], name: &str) -> Result<f64, String> {
    need(args, 1, name)?;
    scalar_arg(args, 0)
}

fn matrix_arg(args: &[Value], i: usize) -> Result<Matrix, String> {
    match args.get(i) {
        Some(Value::Matrix(m)) => Ok(m.clone()),
        Some(other) => Err(format!("аргумент {} должен быть матрицей, получено {other}", i + 1)),
        None => Err("не хватает аргументов".into()),
    }
}

/// Кольцо/точка: матрица N×2 (N=1 — точка, N=2 — отрезок, ≥3 — кольцо,
/// замыкается автоматически). Возвращает f64-координаты.
fn ring_arg(args: &[Value], i: usize, name: &str) -> Result<Vec<(f64, f64)>, String> {
    let m = match args.get(i) {
        Some(Value::Matrix(m)) => m.clone(),
        Some(other) => {
            return Err(format!(
                "{name}: аргумент {} — матрица Nx2 (кольцо/точка), получено {other}",
                i + 1
            ))
        }
        None => return Err(format!("{name}: не хватает аргументов")),
    };
    if m.cols != 2 || m.rows < 1 {
        return Err(format!(
            "{name}: матрица {}×{} — нужно Nx2 (x, y в столбцах)",
            m.rows, m.cols
        ));
    }
    let mut pts = Vec::with_capacity(m.rows);
    for r in 0..m.rows {
        pts.push((m.get(r, 0).re, m.get(r, 1).re));
    }
    Ok(pts)
}

/// f64-кольцо → тритные точки решётки 3⁻ᵏ.
fn quantize_ring(ring: &[(f64, f64)], k: u8) -> Result<Vec<crate::geo::trit_coord::TritPoint>, String> {
    ring.iter()
        .map(|&(x, y)| {
            crate::geo::trit_coord::TritPoint::quantize(x, y, k)
                .map_err(|e| format!("квантование ({x}, {y}): {e}"))
        })
        .collect()
}

/// Complex → Value: вещественный результат остаётся числом (нулевая
/// регрессия отображения), комплексный — Complex (цикл O).
fn complex_to_value(c: Complex) -> Value {
    if c.im == 0.0 {
        Value::Scalar(c.re)
    } else {
        Value::Complex(c)
    }
}

/// Гибкое число: Scalar | Quantity (в базовых СИ) | Complex (вещественная
/// часть с диагностикуой) — для t и ħ в schrodinger (цикл O).
fn flex_num(args: &[Value], i: usize, name: &str) -> Result<f64, String> {
    match args.get(i) {
        Some(Value::Scalar(v)) => Ok(*v),
        Some(Value::Quantity(q, _)) => Ok(*q), // значение в базовых СИ (hbar → 1.05e-34)
        Some(other) => Err(format!(
            "{name}: аргумент {} должен быть числом, получено {other}",
            i + 1
        )),
        None => Err(format!("{name}: не хватает аргументов")),
    }
}

/// Строковый аргумент (имя планеты, тритная запись).
fn str_arg(args: &[Value], i: usize) -> Result<String, String> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s.clone()),
        Some(other) => Err(format!("аргумент {} должен быть строкой \"…\": {other}", i + 1)),
        None => Err("не хватает аргументов".into()),
    }
}

/// Список чисел целиком (viz_prob/viz_bars сессии-8):
/// скаляры и безразмерные величины; вектор-матрица [a, b, c]
/// (грабля 16: литерал в скобках — матрица-строка).
fn float_list_arg(args: &[Value], i: usize, name: &str) -> Result<Vec<f64>, String> {
    match args.get(i) {
        Some(Value::List(items)) => items
            .iter()
            .map(|x| match x {
                Value::Scalar(v) => Ok(*v),
                Value::Quantity(q, u) if u.is_dimensionless() => Ok(*q),
                other => Err(format!(
                    "{name}: элементы списка должны быть числами, получено {other}"
                )),
            })
            .collect(),
        Some(Value::Matrix(m)) => viz::matrix_as_vector(m).ok_or_else(|| {
            format!(
                "{name}: матрица {}×{} — не вектор; подайте строку [a, b, c] или столбец",
                m.rows, m.cols
            )
        }),
        Some(other) => Err(format!(
            "{name}: аргумент {} должен быть списком [ … ], получено {other}",
            i + 1
        )),
        None => Err("не хватает аргументов".into()),
    }
}

/// Дата (y, m, d[, час UTC]) из аргументов, начиная с индекса i.
fn date_args(args: &[Value], i: usize) -> Result<(i32, u32, u32, f64), String> {
    let y = scalar_arg(args, i)?;
    let m = scalar_arg(args, i + 1)?;
    let d = scalar_arg(args, i + 2)?;
    let h = if args.len() > i + 3 { scalar_arg(args, i + 3)? } else { 12.0 };
    let (yi, mi, di) = (y as i32, m as u32, d as u32);
    if !(1..=12).contains(&mi) || !(1..=31).contains(&di) {
        return Err(format!("некорректная дата {yi}-{mi}-{di}"));
    }
    Ok((yi, mi, di, h))
}

// ---------------------------------------------------------------------
// Специальные функции (тяжёлая математика)
// ---------------------------------------------------------------------

/// Гамма-функция, аппроксимация Ланцроша (g=7, n=9).
/// Точность ~1e-13 в комплексной плоскости |z|<последнего полюса.
pub fn gamma_impl(x: f64) -> Result<f64, String> {
    if x.is_nan() {
        return Err("NaN".into());
    }
    if x == 0.0 || (x < 0.0 && x.fract() == 0.0) {
        // цикл N: полюс — расходимость на расширенной прямой.
        // Вычет в x = −n равен (−1)ⁿ/n! (для анализа — solve вокруг полюса)
        return Ok(f64::INFINITY);
    }
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.5203681218851,
        -1259.1392167224028,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507343278686905,
        -0.13857109526572012,
        9.984_369_578_019_572e-6,
        1.5056327351493116e-7,
    ];
    if x < 0.5 {
        // отражение: Γ(z)Γ(1−z) = π/sin(πz)
        Ok(std::f64::consts::PI / ((std::f64::consts::PI * x).sin() * gamma_impl(1.0 - x)?))
    } else {
        let z = x - 1.0;
        let mut acc = C[0];
        for (i, &c) in C.iter().enumerate().skip(1) {
            acc += c / (z + i as f64);
        }
        let t = z + G + 0.5;
        Ok((2.0 * std::f64::consts::PI).sqrt() * t.powf(z + 0.5) * (-t).exp() * acc)
    }
}

/// ln|Γ(x)| — для больших x без переполнения.
pub fn lgamma_impl(x: f64) -> Result<f64, String> {
    if x <= 0.0 && x.fract() == 0.0 {
        return Err("Γ имеет полюса в неположительных целых".into());
    }
    Ok(gamma_impl(x)?.abs().ln())
}

/// Функция ошибок, A&S 7.1.26 (|ε| ≤ 1.5e-7).
/// РЕГРЕССИЯ: Хорнер с правильными знаками и внешним множителем t:
///   erf(x) = 1 − t·(a1 + a2·t + a3·t² + a4·t³ + a5·t⁴)·e^{−x²},
///   t = 1/(1+px).
pub fn erf_impl(x: f64) -> f64 {
    if x == 0.0 {
        return 0.0; // нечётная функция — точно (полином A&S даёт ~1e-9 в нуле)
    }
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let p = 0.3275911;
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let t = 1.0 / (1.0 + p * x);
    // Хорнер: a1 + t·(a2 + t·(a3 + t·(a4 + t·a5))), затем × t
    let poly = a1 + t * (a2 + t * (a3 + t * (a4 + t * a5)));
    let y = 1.0 - poly * t * (-x * x).exp();
    sign * y
}

/// Дзета-функция Римана: Эйлер–Маклорен для s>1, функциональное
/// уравнение для s<1, тривиальные нули/ζ(0) — точно.
pub fn zeta_impl(s: f64) -> Result<f64, String> {
    if !s.is_finite() {
        return Err("ζ: аргумент должен быть конечен".into());
    }
    if s == 1.0 {
        // гармонический ряд расходится к +∞ — расширенная прямая,
        // в одном стиле с полюсами Γ (gamma(-1) → ∞)
        return Ok(f64::INFINITY);
    }
    // тривиальные нули: s = −2, −4, −6, …
    if s < 0.0 && s.fract() == 0.0 && (s / 2.0).fract() == 0.0 {
        return Ok(0.0);
    }
    if s == 0.0 {
        return Ok(-0.5);
    }
    if s > 1.0 {
        // Эйлер–Маклорен: Σ_{k=1}^{N−1} k^{-s} + N^{1−s}/(s−1) + N^{-s}/2
        //   + s·N^{-s-1}/12 − s(s+1)(s+2)·N^{-s-3}/720
        let nf: f64 = 30.0;
        let mut sum = 0.0;
        for k in 1..30u64 {
            sum += (k as f64).powf(-s);
        }
        sum += nf.powf(1.0 - s) / (s - 1.0);
        sum += 0.5 * nf.powf(-s);
        sum += s * nf.powf(-s - 1.0) / 12.0;
        sum -= s * (s + 1.0) * (s + 2.0) * nf.powf(-s - 3.0) / 720.0;
        Ok(sum)
    } else {
        // s < 1 (кроме 0 и отрицательных чётных): функциональное уравнение
        // ζ(s) = 2^s π^{s−1} sin(πs/2) Γ(1−s) ζ(1−s)
        let g = gamma_impl(1.0 - s)?;
        let z2 = zeta_impl(1.0 - s)?;
        Ok(2.0_f64.powf(s) * std::f64::consts::PI.powf(s - 1.0)
            * (std::f64::consts::PI * s / 2.0).sin()
            * g
            * z2)
    }
}

// ---------------------------------------------------------------------
// Реестр
// ---------------------------------------------------------------------

/// Является ли имя известной функцией (для парсера: вызов vs умножение).
pub fn is_function(name: &str) -> bool {
    matches!(
        name,
        // математика
        "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
        | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh"
        | "sind" | "cosd" | "tand" | "atan2d"
        | "exp" | "ln" | "log" | "log2" | "log10" | "sqrt" | "cbrt" | "hypot" | "pow"
        | "abs" | "floor" | "ceil" | "round" | "sign" | "fract"
        | "deg2rad" | "rad2deg" | "min" | "max" | "mean" | "median" | "std" | "clamp"
        // специальная
        | "gamma" | "lgamma" | "erf" | "erfc" | "zeta" | "beta"
        // теория чисел
        | "gcd" | "lcm" | "mod" | "is_prime" | "next_prime" | "prev_prime"
        | "factorize" | "divisors" | "fib" | "binomial" | "catalan"
        | "factorial" | "fact"
        // физика (цикл N)
        | "lorentz"
        // матрицы/квант
        | "transpose" | "det" | "inv" | "pinv" | "trace" | "expm" | "charpoly" | "eigen"
        | "eigen_sturm"
        | "identity" | "rot2" | "rotx" | "roty" | "rotz" | "so_gen"
        // квант цикла O: Шрёдингер
        | "schrodinger" | "dagger" | "kron" | "tridiag" | "eye"
        | "pauli_x" | "pauli_y" | "pauli_z" | "hadamard"
        // триты
        | "trits" | "trit_val" | "trit_and" | "trit_or" | "trit_not"
        // визуализация (цикл U, сессия-8)
        | "viz" | "viz_bell" | "viz_scale" | "viz_prob" | "viz_bars" | "viz_matrix"
        // суверенный рендер (сессия-9)
        | "viz_graph" | "viz_field" | "viz_surf"
        // GIS-ядро на тритах (сессия-12)
        | "viz_iso3" | "de9im" | "geo_pred"
        // единицы/температура
        | "degC" | "degF"
        // астрономия
        | "sun_lon" | "sun_ra" | "sun_dec"
        | "moon_lon" | "moon_lat" | "moon_dist" | "moon_phase" | "moon_illum"
        | "moon_age" | "moon_ra" | "moon_dec"
        | "planet_lon" | "planet_dist" | "sunrise" | "sunset"
        // геодезия/навигация
        | "dist" | "bearing" | "midpoint" | "dest" | "earth_radius"
    )
}

/// Вызвать функцию по имени.
pub fn call_function(name: &str, args: &[Value]) -> Result<Value, String> {
    // Списки: скалярные функции применяются поэлементно
    if let Some(Value::List(_)) = args.iter().find(|a| matches!(a, Value::List(_))) {
        if !matches!(
            name,
            "min" | "max" | "mean" | "median" | "std" | "gcd" | "lcm" | "atan2" | "pow"
                | "hypot" | "clamp" | "mod" | "binomial" | "trit_and" | "trit_or"
                | "midpoint" | "dist" | "bearing" | "dest" | "sunrise" | "sunset"
                | "planet_lon" | "planet_dist"
                // viz-функции принимают список целиком (сессия-8)
                | "viz" | "viz_prob" | "viz_bars"
                // стек срезов — целиком (сессия-12)
                | "viz_iso3"
        ) {
            return map_over_lists(name, args);
        }
    }

    match name {
        // ---------- тригонометрия ----------
        "sin" => {
            // цикл O: sin(x+iy) = sin x·cosh y + i·cos x·sinh y
            match args {
                [Value::Complex(c)] => Ok(Value::Complex(Complex::new(
                    c.re.sin() * c.im.cosh(),
                    c.re.cos() * c.im.sinh(),
                ))),
                _ => Ok(Value::Scalar(angle_arg(args, 0)?.sin())),
            }
        }
        "cos" => {
            // цикл O: cos(x+iy) = cos x·cosh y − i·sin x·sinh y
            match args {
                [Value::Complex(c)] => Ok(Value::Complex(Complex::new(
                    c.re.cos() * c.im.cosh(),
                    -(c.re.sin() * c.im.sinh()),
                ))),
                _ => Ok(Value::Scalar(angle_arg(args, 0)?.cos())),
            }
        }
        "tan" => Ok(Value::Scalar(angle_arg(args, 0)?.tan())),
        "asin" => {
            // цикл N: |x| > 1 — комплексная ветвь (DLMF 4.23):
            // asin(x) = π/2 − i·acosh(x) при x > 1; нечётность при x < −1
            let x = one_arg(args, name)?;
            if x > 1.0 {
                Ok(Value::Complex(Complex::new(
                    std::f64::consts::FRAC_PI_2,
                    -(x + (x * x - 1.0).sqrt()).ln(),
                )))
            } else if x < -1.0 {
                Ok(Value::Complex(Complex::new(
                    -std::f64::consts::FRAC_PI_2,
                    (-x + (x * x - 1.0).sqrt()).ln(),
                )))
            } else {
                Ok(Value::Scalar(x.asin()))
            }
        }
        "acos" => {
            // цикл N: acos(x) = i·acosh(x) при x > 1; π − i·acosh(−x) при x < −1
            let x = one_arg(args, name)?;
            if x > 1.0 {
                Ok(Value::Complex(Complex::new(
                    0.0,
                    (x + (x * x - 1.0).sqrt()).ln(),
                )))
            } else if x < -1.0 {
                Ok(Value::Complex(Complex::new(
                    std::f64::consts::PI,
                    -(-x + (x * x - 1.0).sqrt()).ln(),
                )))
            } else {
                Ok(Value::Scalar(x.acos()))
            }
        }
        "atan" => Ok(Value::Scalar(one_arg(args, name)?.atan())),
        "atan2" => {
            need(args, 2, name)?;
            Ok(Value::Scalar(scalar_arg(args, 0)?.atan2(scalar_arg(args, 1)?)))
        }
        "sinh" => Ok(Value::Scalar(one_arg(args, name)?.sinh())),
        "cosh" => Ok(Value::Scalar(one_arg(args, name)?.cosh())),
        "tanh" => Ok(Value::Scalar(one_arg(args, name)?.tanh())),
        "asinh" => Ok(Value::Scalar(one_arg(args, name)?.asinh())),
        "acosh" => {
            // цикл N: x < 1 — комплексная ветвь acosh(x) = i·acos(x)
            let x = one_arg(args, name)?;
            if x < 1.0 {
                // acos(x) для x<−1 сам комплексный: acosh = i·acos(x)
                if x < -1.0 {
                    let a = (-x + (x * x - 1.0).sqrt()).ln();
                    // acos(x) = π − i·a → i·acos = i·π + a
                    return Ok(Value::Complex(Complex::new(a, std::f64::consts::PI)));
                }
                return Ok(Value::Complex(Complex::new(0.0, x.acos())));
            }
            Ok(Value::Scalar(x.acosh()))
        }
        "atanh" => {
            // цикл N: |x| ≥ 1 — расширенный результат (DLMF 4.37):
            // x=±1 → ±∞; x>1 → ½ln((x+1)/(x−1)) − i·π/2; x<−1 — нечётно
            let x = one_arg(args, name)?;
            if x == 1.0 {
                return Ok(Value::Scalar(f64::INFINITY));
            }
            if x == -1.0 {
                return Ok(Value::Scalar(f64::NEG_INFINITY));
            }
            if x > 1.0 {
                Ok(Value::Complex(Complex::new(
                    0.5 * ((x + 1.0) / (x - 1.0)).ln(),
                    -std::f64::consts::FRAC_PI_2,
                )))
            } else if x < -1.0 {
                Ok(Value::Complex(Complex::new(
                    0.5 * ((-x - 1.0) / (1.0 - x)).ln(),
                    std::f64::consts::FRAC_PI_2,
                )))
            } else {
                Ok(Value::Scalar(x.atanh()))
            }
        }
        "sind" => Ok(Value::Scalar(one_arg(args, name)?.to_radians().sin())),
        "cosd" => Ok(Value::Scalar(one_arg(args, name)?.to_radians().cos())),
        "tand" => Ok(Value::Scalar(one_arg(args, name)?.to_radians().tan())),
        "atan2d" => {
            need(args, 2, name)?;
            Ok(Value::Scalar(
                scalar_arg(args, 0)?.atan2(scalar_arg(args, 1)?).to_degrees(),
            ))
        }

        // ---------- экспоненты/логарифмы ----------
        "exp" => {
            // цикл O: экспонента комплексного — тождество Эйлера
            // exp(iπ) = −1; e^{x+iy} = e^x·(cos y + i·sin y)
            match args {
                [Value::Complex(c)] => {
                    Ok(Value::Complex(Complex::from_polar(c.re.exp(), c.im)))
                }
                _ => Ok(Value::Scalar(one_arg(args, name)?.exp())),
            }
        }
        "ln" => {
            // цикл N: расширенный логарифм — ln(0) = −∞ (предел),
            // ln(x<0) = ln|x| + iπ (главная ветвь)
            let x = one_arg(args, name)?;
            if x == 0.0 {
                return Ok(Value::Scalar(f64::NEG_INFINITY));
            }
            if x < 0.0 {
                return Ok(Value::Complex(Complex::new(
                    (-x).ln(),
                    std::f64::consts::PI,
                )));
            }
            Ok(Value::Scalar(x.ln()))
        }
        "log" => {
            // log(x) = log10 (калькуляторная конвенция); log(x, b) — по основанию b
            // цикл N: x ≤ 0 — расширенно (0 → −∞; отрицательное → комплексно)
            match args.len() {
                1 => {
                    let x = scalar_arg(args, 0)?;
                    if x == 0.0 {
                        return Ok(Value::Scalar(f64::NEG_INFINITY));
                    }
                    if x < 0.0 {
                        let l = Complex::new((-x).ln(), std::f64::consts::PI);
                        return Ok(Value::Complex(Complex::new(
                            l.re / std::f64::consts::LN_10,
                            l.im / std::f64::consts::LN_10,
                        )));
                    }
                    Ok(Value::Scalar(x.log10()))
                }
                2 => {
                    let (x, b) = (scalar_arg(args, 0)?, scalar_arg(args, 1)?);
                    if x <= 0.0 || b <= 0.0 || b == 1.0 {
                        return Err("log(x, b): x > 0, b > 0, b ≠ 1".into());
                    }
                    Ok(Value::Scalar(x.log(b)))
                }
                n => Err(format!("log: 1 или 2 аргумента, получено {n}")),
            }
        }
        "log2" => {
            let x = one_arg(args, name)?;
            if x == 0.0 {
                return Ok(Value::Scalar(f64::NEG_INFINITY));
            }
            if x < 0.0 {
                let l = Complex::new((-x).ln(), std::f64::consts::PI);
                return Ok(Value::Complex(Complex::new(
                    l.re / std::f64::consts::LN_2,
                    l.im / std::f64::consts::LN_2,
                )));
            }
            Ok(Value::Scalar(x.log2()))
        }
        "log10" => {
            let x = one_arg(args, name)?;
            if x == 0.0 {
                return Ok(Value::Scalar(f64::NEG_INFINITY));
            }
            if x < 0.0 {
                return Ok(Value::Complex(Complex::new(
                    (-x).log10(),
                    std::f64::consts::PI / std::f64::consts::LN_10,
                )));
            }
            Ok(Value::Scalar(x.log10()))
        }
        "sqrt" => {
            // √ размерной величины: размерности обязаны быть чётными
            // (√(s²) = s, √(m²/s²) = m/s) — нужно законам Кеплера и др.
            if let Some(Value::Quantity(q, u)) = args.first() {
                if u.dim.iter().all(|&dd| (dd as i32) % 2 == 0) {
                    let mut dim = [0i8; units::NDIM];
                    for (i, &dd) in u.dim.iter().enumerate() {
                        dim[i] = dd / 2;
                    }
                    let unit = Unit { dim, factor: u.factor.sqrt(), name: "", offset: 0.0 };
                    return Ok(Value::Quantity(q.sqrt(), unit));
                }
                return Err("sqrt: размерности величины должны быть чётными (√(s²)=s ок, √(m) — нет)".into());
            }
            // цикл N: sqrt(x<0) = i·√|x| — тахионный множитель Лоренца
            // и любая мнимая ось без обходного пути через solve
            let x = one_arg(args, name)?;
            if x < 0.0 {
                return Ok(Value::Complex(Complex::new(0.0, (-x).sqrt())));
            }
            Ok(Value::Scalar(x.sqrt()))
        }
        "cbrt" => Ok(Value::Scalar(one_arg(args, name)?.cbrt())),
        "hypot" => {
            need(args, 2, name)?;
            Ok(Value::Scalar(scalar_arg(args, 0)?.hypot(scalar_arg(args, 1)?)))
        }
        "pow" => {
            need(args, 2, name)?;
            Ok(Value::Scalar(scalar_arg(args, 0)?.powf(scalar_arg(args, 1)?)))
        }

        // ---------- округление/знак ----------
        "abs" => {
            // цикл O: модуль комплексного |3+4i| = 5 и амплитуды 1×1
            // (квантовая вероятность: abs(⟨1|ψ⟩)^2)
            // ГРАБЛИ сессии-4 (исправлены): abs был только скалярным — теперь
            // вектор (1×n / n×1) даёт евклидову норму ‖v‖₂, общая матрица —
            // норму Фробениуса ‖A‖_F = sqrt(Σ|a_ij|²): одна формула на оба случая
            match args {
                [Value::Complex(c)] => Ok(Value::Scalar(c.abs())),
                [Value::Matrix(m)] => {
                    if m.rows == 1 && m.cols == 1 {
                        Ok(Value::Scalar(m.get(0, 0).abs()))
                    } else {
                        let s = m.data.iter().map(|c| c.re * c.re + c.im * c.im).sum::<f64>();
                        Ok(Value::Scalar(s.sqrt()))
                    }
                }
                _ => Ok(Value::Scalar(one_arg(args, name)?.abs())),
            }
        }
        "floor" => Ok(Value::Scalar(one_arg(args, name)?.floor())),
        "ceil" => Ok(Value::Scalar(one_arg(args, name)?.ceil())),
        "round" => Ok(Value::Scalar(one_arg(args, name)?.round())),
        // ГРАБЛИ сессии-4 (исправлены): sign(0) → 0 (мат-конвенция знака);
        // f64::signum отдаёт 1.0 для +0.0 (и −1.0 для −0.0) — на нулевых
        // траекториях CGLMP-сканов это ломало симметрию разностей
        "sign" => {
            let x = one_arg(args, name)?;
            Ok(Value::Scalar(if x == 0.0 { 0.0 } else { x.signum() }))
        }
        "fract" => Ok(Value::Scalar(one_arg(args, name)?.fract())),
        "deg2rad" => Ok(Value::Scalar(one_arg(args, name)?.to_radians())),
        "rad2deg" => Ok(Value::Scalar(one_arg(args, name)?.to_degrees())),

        // ---------- агрегаты (вариативные) ----------
        "min" | "max" => {
            if args.is_empty() {
                return Err(format!("{name}: нужен хотя бы один аргумент"));
            }
            let mut best = scalar_arg(args, 0)?;
            for i in 1..args.len() {
                let v = scalar_arg(args, i)?;
                best = if name == "min" { best.min(v) } else { best.max(v) };
            }
            Ok(Value::Scalar(best))
        }
        "mean" | "median" | "std" => {
            if args.is_empty() {
                return Err(format!("{name}: нужен хотя бы один аргумент"));
            }
            let mut xs: Vec<f64> = Vec::with_capacity(args.len());
            for i in 0..args.len() {
                xs.push(scalar_arg(args, i)?);
            }
            match name {
                "mean" => Ok(Value::Scalar(xs.iter().sum::<f64>() / xs.len() as f64)),
                "median" => {
                    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                    let n = xs.len();
                    Ok(Value::Scalar(if n % 2 == 1 {
                        xs[n / 2]
                    } else {
                        (xs[n / 2 - 1] + xs[n / 2]) / 2.0
                    }))
                }
                _ => {
                    let m = xs.iter().sum::<f64>() / xs.len() as f64;
                    let var = xs.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / xs.len() as f64;
                    Ok(Value::Scalar(var.sqrt()))
                }
            }
        }
        "clamp" => {
            need(args, 3, name)?;
            let (x, lo, hi) = (scalar_arg(args, 0)?, scalar_arg(args, 1)?, scalar_arg(args, 2)?);
            Ok(Value::Scalar(x.clamp(lo, hi)))
        }

        // ---------- специальная математика ----------
        "gamma" => Ok(Value::Scalar(gamma_impl(one_arg(args, name)?)?)),
        "lgamma" => Ok(Value::Scalar(lgamma_impl(one_arg(args, name)?)?)),
        "erf" => Ok(Value::Scalar(erf_impl(one_arg(args, name)?))),
        "erfc" => Ok(Value::Scalar(1.0 - erf_impl(one_arg(args, name)?))),
        "zeta" => Ok(Value::Scalar(zeta_impl(one_arg(args, name)?)?)),
        "beta" => {
            need(args, 2, name)?;
            let (a, b) = (scalar_arg(args, 0)?, scalar_arg(args, 1)?);
            Ok(Value::Scalar(
                gamma_impl(a)? * gamma_impl(b)? / gamma_impl(a + b)?,
            ))
        }

        // ---------- теория чисел ----------
        "gcd" => {
            need(args, 2, name)?;
            Ok(Value::Scalar(numbers::gcd(scalar_arg(args, 0)?, scalar_arg(args, 1)?)?))
        }
        "lcm" => {
            need(args, 2, name)?;
            Ok(Value::Scalar(numbers::lcm(scalar_arg(args, 0)?, scalar_arg(args, 1)?)?))
        }
        "mod" => {
            need(args, 2, name)?;
            Ok(Value::Scalar(
                scalar_arg(args, 0)?.rem_euclid(scalar_arg(args, 1)?),
            ))
        }
        "is_prime" => Ok(Value::Scalar(
            if numbers::is_prime(one_arg(args, name)?)? { 1.0 } else { 0.0 },
        )),
        "next_prime" => Ok(Value::Scalar(numbers::next_prime(one_arg(args, name)?)?)),
        "prev_prime" => Ok(Value::Scalar(numbers::prev_prime(one_arg(args, name)?)?)),
        "factorize" => {
            let fs = numbers::factorize(one_arg(args, name)?)?;
            let mut out: Vec<Value> = Vec::new();
            for (p, k) in fs {
                out.push(Value::Scalar(p));
                out.push(Value::Scalar(k));
            }
            Ok(Value::List(out))
        }
        "divisors" => {
            let ds = numbers::divisors(one_arg(args, name)?)?;
            Ok(Value::List(ds.into_iter().map(Value::Scalar).collect()))
        }
        "fib" => {
            // цикл N: точный big-путь за f64-пределом (F(78))
            let n = one_arg(args, name)?;
            if !n.is_finite() || n.fract() != 0.0 {
                return Err("fib: ожидается целое".into());
            }
            if n < 0.0 {
                return Err("fib: ожидается неотрицательное (реализация без негафибоначчи)".into());
            }
            if n <= 78.0 {
                Ok(Value::Scalar(numbers::fib(n)?))
            } else {
                Ok(Value::BigInt(numbers::fib_big(n as u64)?))
            }
        }
        "binomial" => {
            // цикл N: точный big-путь (C(100,50) уже неточен в f64)
            need(args, 2, name)?;
            let (n, k) = (scalar_arg(args, 0)?, scalar_arg(args, 1)?);
            if n.fract() != 0.0 || k.fract() != 0.0 {
                return Err("binomial: ожидается целые".into());
            }
            if n < 0.0 || k < 0.0 {
                return Err("binomial: n, k ≥ 0".into());
            }
            if k > n {
                return Ok(Value::Scalar(0.0));
            }
            if n > 10_000.0 {
                return Err("binomial: n ≤ 10 000 (точный big-путь)".into());
            }
            let s = numbers::binomial_big(n as u64, k as u64)?;
            // малые результаты — Scalar (совместимость), гиганты — точное целое
            if let Ok(iv) = s.parse::<i64>() {
                if iv.abs() <= 9_007_199_254_740_992 {
                    return Ok(Value::Scalar(iv as f64));
                }
            }
            Ok(Value::BigInt(s))
        }
        "catalan" => {
            // цикл N: точный big-путь (C_n = C(2n,n)/(n+1), деление нацело)
            let n = one_arg(args, name)?;
            if n.fract() != 0.0 || n < 0.0 {
                return Err("catalan: ожидается целое n ≥ 0".into());
            }
            if n > 5_000.0 {
                return Err("catalan: n ≤ 5 000 (точный big-путь)".into());
            }
            let b = numbers::binomial_big(2 * n as u64, n as u64)?;
            let s = numbers::big_div_small(&b, n as u64 + 1);
            if let Ok(iv) = s.parse::<i64>() {
                if iv.abs() <= 9_007_199_254_740_992 {
                    return Ok(Value::Scalar(iv as f64));
                }
            }
            Ok(Value::BigInt(s))
        }
        "factorial" | "fact" => {
            // цикл N: точный факториал (18! — потолок f64-точности)
            let n = one_arg(args, name)?;
            if !n.is_finite() || n.fract() != 0.0 || n < 0.0 {
                return Err("factorial: ожидается целое n ≥ 0".into());
            }
            if n <= 18.0 {
                let r: u64 = (2..=n as u64).product();
                return Ok(Value::Scalar(r as f64));
            }
            Ok(Value::BigInt(numbers::factorial_big(n as u64)?))
        }
        "lorentz" => {
            // цикл N: γ(v) = 1/√(1−v²), v в долях c.
            // v < c → вещественная γ; v = c → ∞ (расходимость);
            // v > c → комплексная γ = −i/√(v²−1) — тахионная ветвь
            // (мнимое собственное время).
            let v = one_arg(args, name)?;
            let s = 1.0 - v * v;
            if s > 0.0 {
                Ok(Value::Scalar(1.0 / s.sqrt()))
            } else if s == 0.0 {
                Ok(Value::Scalar(f64::INFINITY))
            } else {
                Ok(Value::Complex(Complex::new(0.0, -1.0 / (-s).sqrt())))
            }
        }

        // ---------- матрицы/квант ----------
        "transpose" => Ok(Value::Matrix(matrix_arg(args, 0)?.transpose())),
        "det" => Ok(complex_to_value(matrix_arg(args, 0)?.det()?)),
        "inv" => Ok(Value::Matrix(matrix_arg(args, 0)?.inv()?)),
        "pinv" => Ok(Value::Matrix(matrix_arg(args, 0)?.pinv()?)),
        "trace" => Ok(complex_to_value(matrix_arg(args, 0)?.trace()?)),
        "expm" => Ok(Value::Matrix(matrix_arg(args, 0)?.expm()?)),
        "charpoly" => {
            let c = matrix_arg(args, 0)?.charpoly()?;
            Ok(Value::List(c.into_iter().map(complex_to_value).collect()))
        }
        "eigen" => {
            let ev = matrix_arg(args, 0)?.eigenvalues()?;
            Ok(Value::List(ev.into_iter().map(complex_to_value).collect()))
        }
        "eigen_sturm" => {
            // Штурм-бисекция: без charpoly — иммунен к взрыву Уилкинсона (n>=28);
            // симметричные вещественные, машинная точность при n=256+
            let ev = matrix_arg(args, 0)?.eigenvalues_sturm()?;
            Ok(Value::List(ev.into_iter().map(complex_to_value).collect()))
        }
        "identity" => {
            let n = one_arg(args, name)?;
            if n.fract() != 0.0 || !(1.0..=12.0).contains(&n) {
                return Err("identity(n): n — целое 1..=12".into());
            }
            Ok(Value::Matrix(Matrix::identity(n as usize)))
        }
        // eye — алиас identity; ГРАБЛИ сессии-4 (исправлены): искусственный
        // лимит 64 душил кутрит-конвейеры (eye(81), eye(729) для 3⁶ состояний,
        // eye(2187) для 3⁷). Потолок 4096: dense n² × 16 байт, eye(4096) ≈ 268 МБ;
        // крупнее — собирайте kron'ом, память всё равно упрётся раньше
        "eye" => {
            let n = one_arg(args, name)?;
            if n.fract() != 0.0 || !(1.0..=4096.0).contains(&n) {
                return Err("eye(n): n — целое 1..=4096 (4096²·16 байт ≈ 268 МБ dense)".into());
            }
            Ok(Value::Matrix(Matrix::identity(n as usize)))
        }
        "rot2" => Ok(Value::Matrix(Matrix::rot2(angle_arg(args, 0)?))),
        "rotx" => Ok(Value::Matrix(Matrix::rot_x(angle_arg(args, 0)?))),
        "roty" => Ok(Value::Matrix(Matrix::rot_y(angle_arg(args, 0)?))),
        "rotz" => Ok(Value::Matrix(Matrix::rot_z(angle_arg(args, 0)?))),
        "so_gen" => {
            need(args, 3, name)?;
            let (n, i, j) = (scalar_arg(args, 0)?, scalar_arg(args, 1)?, scalar_arg(args, 2)?);
            if n.fract() != 0.0 || i.fract() != 0.0 || j.fract() != 0.0 {
                return Err("so_gen(n, i, j): целые аргументы".into());
            }
            Ok(Value::Matrix(Matrix::so_generator(n as usize, i as usize, j as usize)?))
        }

        // ---------- квант цикла O: Шрёдингер ----------
        "schrodinger" => {
            // |Ψ(t)⟩ = expm(−i·H·t/ħ)·|Ψ₀⟩ — решатель уравнения Шрёдингера.
            // ħ = 1 по умолчанию (натуральные единицы); для СИ передайте
            // четвёртым аргументом hbar (или t уже в t/ħ).
            if args.len() != 3 && args.len() != 4 {
                return Err(
                    "schrodinger(H, psi0, t[, hbar]): 3 или 4 аргумента (по умолчанию ħ = 1)"
                        .into(),
                );
            }
            let h = matrix_arg(args, 0)?;
            let psi0 = matrix_arg(args, 1)?;
            let t = flex_num(args, 2, name)?;
            let hbar = if args.len() == 4 { flex_num(args, 3, name)? } else { 1.0 };
            if !hbar.is_finite() || hbar == 0.0 {
                return Err("schrodinger: ħ должен быть конечным и ненулевым".into());
            }
            if !h.is_square() {
                return Err("schrodinger: гамильтониан H — квадратная матрица".into());
            }
            if psi0.cols != 1 || psi0.rows != h.rows {
                return Err(format!(
                    "schrodinger: psi0 — вектор-столбец {}×1, получено {}×{}",
                    h.rows, psi0.rows, psi0.cols
                ));
            }
            let u = h.scale_c(Complex::new(0.0, -t / hbar)).expm()?;
            Ok(Value::Matrix(u.mul(&psi0)?))
        }
        "dagger" => Ok(Value::Matrix(matrix_arg(args, 0)?.dagger())),
        "kron" => {
            need(args, 2, name)?;
            Ok(Value::Matrix(matrix_arg(args, 0)?.kron(&matrix_arg(args, 1)?)))
        }
        "pauli_x" => {
            need(args, 0, name)?;
            Ok(Value::Matrix(Matrix::from_rows(&[vec![0.0, 1.0], vec![1.0, 0.0]])?))
        }
        "pauli_y" => {
            need(args, 0, name)?;
            Ok(Value::Matrix(Matrix::from_complex_rows(&[
                vec![Complex::ZERO, Complex::new(0.0, -1.0)],
                vec![Complex::new(0.0, 1.0), Complex::ZERO],
            ])?))
        }
        "pauli_z" => {
            need(args, 0, name)?;
            Ok(Value::Matrix(Matrix::from_rows(&[vec![1.0, 0.0], vec![0.0, -1.0]])?))
        }
        "hadamard" => {
            need(args, 0, name)?;
            let k = std::f64::consts::FRAC_1_SQRT_2;
            Ok(Value::Matrix(Matrix::from_rows(&[vec![k, k], vec![k, -k]])?))
        }
        "tridiag" => {
            // трёхдиагональная матрица: d на диагонали, off на соседях —
            // дискретизация лапласиана (яма, осциллятор на сетке).
            // v0.62 (AI-API-реформа): лимит поднят 64 -> 512 — безопасно для
            // det (LU, O(n^3)) и eigen_sturm (Штурм, машинная точность при
            // n=256+); eigen/charpoly ломаются на n>=28 ВСЕГДА (Уилкинсон),
            // независимо от лимита билдера.
            need(args, 3, name)?;
            let (d, off, n) = (
                scalar_arg(args, 0)?,
                scalar_arg(args, 1)?,
                scalar_arg(args, 2)?,
            );
            if n.fract() != 0.0 || !(2.0..=512.0).contains(&n) {
                return Err("tridiag(d, off, n): n — целое 2..=512".into());
            }
            let n = n as usize;
            let mut m = Matrix::zeros(n, n);
            for i in 0..n {
                m.set(i, i, Complex::new(d, 0.0));
            }
            for i in 0..n - 1 {
                m.set(i, i + 1, Complex::new(off, 0.0));
                m.set(i + 1, i, Complex::new(off, 0.0));
            }
            Ok(Value::Matrix(m))
        }

        // ---------- визуализация (цикл U, сессия-8) ----------
        "viz" => {
            need(args, 1, name)?;
            Ok(Value::Str(viz::auto_svg(&args[0])?))
        }
        "viz_bell" => {
            let i = one_arg(args, name)?;
            Ok(Value::Str(viz::bell_svg(i)))
        }
        "viz_scale" => {
            // Метры: скаляр по конвенции либо величина длиновой
            // размерности (конверсия через factor к базовой).
            need(args, 1, name)?;
            let x_m = match args.get(0) {
                Some(Value::Scalar(x)) => *x,
                Some(Value::Quantity(q, u))
                    if u.dim[0] == 1 && u.dim[1..].iter().all(|d| *d == 0) =>
                {
                    q * u.factor
                }
                Some(other) => {
                    return Err(format!(
                        "viz_scale: аргумент 1 должен быть числом (метры) или длиной, \
                         получено {other}"
                    ))
                }
                None => return Err("viz_scale: не хватает аргументов".into()),
            };
            Ok(Value::Str(viz::scale_svg(x_m)?))
        }
        "viz_prob" => {
            let ps = float_list_arg(args, 0, name)?;
            Ok(Value::Str(viz::prob_svg(&ps)?))
        }
        "viz_bars" => {
            let vs = float_list_arg(args, 0, name)?;
            Ok(Value::Str(viz::bars_svg(&vs)?))
        }
        "viz_matrix" => {
            let m = matrix_arg(args, 0)?;
            Ok(Value::Str(viz::matrix_svg(&m)?))
        }
        // ---------- суверенный рендер (сессия-9) ----------
        "viz_graph" => {
            // Смежность + опциональные метки ОДНОЙ строкой через запятую
            // (грабля 16: литерала списка строк в языке нет).
            if args.is_empty() || args.len() > 2 {
                return Err(format!(
                    "viz_graph: ожидается 1–2 аргумента, получено {}",
                    args.len()
                ));
            }
            let m = matrix_arg(args, 0)?;
            let labels = match args.get(1) {
                None => None,
                Some(Value::Str(s)) => Some(
                    s.split(',')
                        .map(|t| t.trim().to_string())
                        .collect::<Vec<_>>(),
                ),
                Some(other) => {
                    return Err(format!(
                        "viz_graph: метки — строка через запятую \
                         (\"имя1, имя2, …\"), получено {other}"
                    ))
                }
            };
            Ok(Value::Str(viz::graph_svg(&m, labels.as_deref())?))
        }
        "viz_field" => {
            let m = matrix_arg(args, 0)?;
            Ok(Value::Str(viz::field_svg(&m)?))
        }
        "viz_surf" => {
            let m = matrix_arg(args, 0)?;
            Ok(Value::Str(viz::surf_svg(&m)?))
        }
        // ---------- GIS-ядро на тритах (сессия-12) ----------
        "viz_iso3" => {
            // Стек z-срезов: список матриц (список строится функциями —
            // литерала списка матриц в языке нет; либо одна матрица
            // (rows = nz·ny, блоки строк — срезы), либо список).
            let level = match args.get(1) {
                None => None,
                Some(Value::Scalar(v)) => Some(*v),
                Some(Value::Quantity(q, u)) if u.is_dimensionless() => Some(*q),
                Some(other) => {
                    return Err(format!(
                        "viz_iso3: уровень — число, получено {other}"
                    ))
                }
            };
            let slices: Vec<Matrix> = match args.get(0) {
                Some(Value::List(items)) => {
                    let mut ms = Vec::with_capacity(items.len());
                    for (i, it) in items.iter().enumerate() {
                        match it {
                            Value::Matrix(m) => ms.push(m.clone()),
                            other => {
                                return Err(format!(
                                    "viz_iso3: срез {i} должен быть матрицей, \
                                     получено {other}"
                                ))
                            }
                        }
                    }
                    ms
                }
                Some(Value::Matrix(m)) => {
                    // Одна матрица (nz·ny)×nx: nz блоков по ny строк.
                    if m.rows % m.cols != 0 || m.rows / m.cols < 2 {
                        return Err(format!(
                            "viz_iso3: матрица {}×{} не разбивается на куб \
                             (rows должны быть nz·cols)",
                            m.rows, m.cols
                        ));
                    }
                    let ny = m.cols;
                    let nz = m.rows / ny;
                    let mut ms = Vec::with_capacity(nz);
                    for k in 0..nz {
                        let mut plane = Matrix::zeros(ny, m.cols);
                        for i in 0..ny {
                            for j in 0..m.cols {
                                plane.set(i, j, m.get(k * ny + i, j));
                            }
                        }
                        ms.push(plane);
                    }
                    ms
                }
                Some(other) => {
                    return Err(format!(
                        "viz_iso3: аргумент 1 — список матриц или блочная \
                         матрица, получено {other}"
                    ))
                }
                None => return Err("viz_iso3: не хватает аргументов".into()),
            };
            Ok(Value::Str(viz::iso3_svg(&slices, level)?))
        }
        "de9im" => {
            // relate(a, b [, k]): кольца — матрицы Nx2; точка — 1x2.
            let a = ring_arg(args, 0, "de9im")?;
            let b = ring_arg(args, 1, "de9im")?;
            let k = match args.get(2) {
                None => 6u8,
                Some(Value::Scalar(v)) if *v >= 0.0 && *v <= 12.0 => *v as u8,
                Some(other) => {
                    return Err(format!(
                        "de9im: масштаб k ∈ [0, 12] — число, получено {other}"
                    ))
                }
            };
            let qa = quantize_ring(&a, k)?;
            let qb = quantize_ring(&b, k)?;
            let m = crate::geo::de9im::relate(&qa, &qb)
                .map_err(|e| format!("de9im: {e}"))?;
            let preds = m.predicate_names().join(", ");
            Ok(Value::Str(format!(
                "{} · код {} · предикаты: {}",
                m.bal_string(),
                m.trit_code(),
                if preds.is_empty() { "—" } else { &preds }
            )))
        }
        "geo_pred" => {
            // Список сработавших предикатов relate(a, b [, k]).
            let a = ring_arg(args, 0, "geo_pred")?;
            let b = ring_arg(args, 1, "geo_pred")?;
            let k = match args.get(2) {
                None => 6u8,
                Some(Value::Scalar(v)) if *v >= 0.0 && *v <= 12.0 => *v as u8,
                Some(other) => {
                    return Err(format!(
                        "geo_pred: масштаб k ∈ [0, 12] — число, получено {other}"
                    ))
                }
            };
            let qa = quantize_ring(&a, k)?;
            let qb = quantize_ring(&b, k)?;
            let m = crate::geo::de9im::relate(&qa, &qb)
                .map_err(|e| format!("geo_pred: {e}"))?;
            Ok(Value::List(
                m.predicate_names()
                    .into_iter()
                    .map(|p| Value::Str(p.to_string()))
                    .collect(),
            ))
        }
        // ---------- триты ----------
        "trits" => {
            let v = one_arg(args, name)?;
            let t = Trits::from_i64(v as i64)
                .map_err(|e| format!("trits({v}): {e}"))?;
            Ok(Value::Str(t.to_string_bal()))
        }
        "trit_val" => {
            let s = str_arg(args, 0)?;
            Ok(Value::Scalar(Trits::parse(&s)?.to_i64() as f64))
        }
        "trit_and" | "trit_or" => {
            need(args, 2, name)?;
            let a = Trits::parse(&str_arg(args, 0)?)?;
            let b = Trits::parse(&str_arg(args, 1)?)?;
            let r = if name == "trit_and" { Trits::t_and(&a, &b) } else { Trits::t_or(&a, &b) };
            Ok(Value::Str(r.to_string_bal()))
        }
        "trit_not" => {
            let a = Trits::parse(&str_arg(args, 0)?)?;
            Ok(Value::Str(Trits::t_not(&a).to_string_bal()))
        }

        // ---------- температура ----------
        "degC" => {
            let c = one_arg(args, name)?;
            let u = units::by_name("degC").unwrap();
            Ok(Value::Quantity(c, u))
        }
        "degF" => {
            let f = one_arg(args, name)?;
            let u = units::by_name("degF").unwrap();
            Ok(Value::Quantity(f, u))
        }

        // ---------- астрономия ----------
        "sun_lon" => {
            let (y, m, d, h) = date_args(args, 0)?;
            Ok(Value::Scalar(astro::sun_ecliptic_lon(y, m, d, h)))
        }
        "sun_ra" => {
            let (y, m, d, h) = date_args(args, 0)?;
            let (ra, _) = astro::sun_ra_dec(y, m, d, h);
            Ok(Value::Scalar(ra))
        }
        "sun_dec" => {
            let (y, m, d, h) = date_args(args, 0)?;
            let (_, dec) = astro::sun_ra_dec(y, m, d, h);
            Ok(Value::Scalar(dec))
        }
        "moon_lon" => {
            let (y, m, d, h) = date_args(args, 0)?;
            Ok(Value::Scalar(astro::moon_position(y, m, d, h).0))
        }
        "moon_lat" => {
            let (y, m, d, h) = date_args(args, 0)?;
            Ok(Value::Scalar(astro::moon_position(y, m, d, h).1))
        }
        "moon_dist" => {
            let (y, m, d, h) = date_args(args, 0)?;
            Ok(Value::Scalar(astro::moon_position(y, m, d, h).2))
        }
        "moon_phase" => {
            let (y, m, d, h) = date_args(args, 0)?;
            Ok(Value::Scalar(astro::moon_elongation(y, m, d, h)))
        }
        "moon_illum" => {
            let (y, m, d, h) = date_args(args, 0)?;
            Ok(Value::Scalar(astro::moon_illumination(y, m, d, h)))
        }
        "moon_age" => {
            let (y, m, d, h) = date_args(args, 0)?;
            Ok(Value::Scalar(astro::moon_age_days(y, m, d, h)))
        }
        "moon_ra" => {
            let (y, m, d, h) = date_args(args, 0)?;
            let (ra, _) = astro::moon_ra_dec(y, m, d, h);
            Ok(Value::Scalar(ra))
        }
        "moon_dec" => {
            let (y, m, d, h) = date_args(args, 0)?;
            let (_, dec) = astro::moon_ra_dec(y, m, d, h);
            Ok(Value::Scalar(dec))
        }
        "planet_lon" => {
            let pname = str_arg(args, 0)?;
            let (y, m, d, h) = date_args(args, 1)?;
            let (lon, _, _) = astro::planet_geocentric(&pname, y, m, d, h)?;
            Ok(Value::Scalar(lon))
        }
        "planet_dist" => {
            let pname = str_arg(args, 0)?;
            let (y, m, d, h) = date_args(args, 1)?;
            let (_, _, r) = astro::planet_geocentric(&pname, y, m, d, h)?;
            Ok(Value::Scalar(r))
        }
        "sunrise" | "sunset" => {
            // sunrise(lat, lon, y, m, d[, hour]) → UTC часы
            let (lat, lon) = (scalar_arg(args, 0)?, scalar_arg(args, 1)?);
            let (y, m, d, _) = date_args(args, 2)?;
            let ss = astro::sunrise_sunset_utc(y, m, d, lat, lon)?;
            match ss {
                None => Ok(Value::Str(
                    if name == "sunrise" { "полярная ночь" } else { "полярный день" }.into(),
                )),
                Some((rise, set)) => Ok(Value::Scalar(if name == "sunrise" { rise } else { set })),
            }
        }

        // ---------- геодезия/навигация ----------
        "dist" => {
            need(args, 4, name)?;
            Ok(Value::Scalar(geodesy::great_circle_km(
                scalar_arg(args, 0)?,
                scalar_arg(args, 1)?,
                scalar_arg(args, 2)?,
                scalar_arg(args, 3)?,
            )))
        }
        "bearing" => {
            need(args, 4, name)?;
            Ok(Value::Scalar(geodesy::bearing_deg(
                scalar_arg(args, 0)?,
                scalar_arg(args, 1)?,
                scalar_arg(args, 2)?,
                scalar_arg(args, 3)?,
            )))
        }
        "midpoint" => {
            need(args, 4, name)?;
            let (lat, lon) = geodesy::midpoint(
                scalar_arg(args, 0)?,
                scalar_arg(args, 1)?,
                scalar_arg(args, 2)?,
                scalar_arg(args, 3)?,
            );
            Ok(Value::List(vec![Value::Scalar(lat), Value::Scalar(lon)]))
        }
        "dest" => {
            need(args, 4, name)?;
            let (lat, lon) = geodesy::destination(
                scalar_arg(args, 0)?,
                scalar_arg(args, 1)?,
                scalar_arg(args, 2)?,
                scalar_arg(args, 3)?,
            );
            Ok(Value::List(vec![Value::Scalar(lat), Value::Scalar(lon)]))
        }
        "earth_radius" => Ok(Value::Scalar(geodesy::earth_radius_km(one_arg(args, name)?))),

        other => Err(format!("неизвестная функция «{other}» (каталог: calc funcs)")),
    }
}

/// Поэлементное применение функции к первому списку в аргументх.
fn map_over_lists(name: &str, args: &[Value]) -> Result<Value, String> {
    for (i, a) in args.iter().enumerate() {
        if let Value::List(items) = a {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                let mut call_args = args.to_vec();
                call_args[i] = it.clone();
                out.push(call_function(name, &call_args)?);
            }
            return Ok(Value::List(out));
        }
    }
    unreachable!("map_over_lists вызывается только при наличии списка")
}

/// Каталог функций для `calc funcs [фильтр]`.
pub fn catalog(filter: &str) -> String {
    let groups: &[(&str, &[&str])] = &[
        ("математика", &[
            "sin cos tan asin acos atan atan2 sinh cosh tanh asinh acosh atanh",
            "sind cosd tand atan2d (градусы)",
            "exp ln log log2 log10 sqrt cbrt hypot pow abs floor ceil round sign fract",
            "deg2rad rad2deg min max mean median std clamp",
        ]),
        ("специальная", &["gamma lgamma erf erfc zeta beta"]),
        ("теория чисел", &[
            "gcd lcm mod is_prime next_prime prev_prime factorize divisors fib binomial catalan",
        ]),
        ("матрицы/квант", &[
            "transpose det inv pinv trace expm charpoly eigen identity eye",
            "rot2 rotx roty rotz so_gen kron dagger tridiag",
            "квант O: schrodinger(H, psi0, t[, hbar]) — уравнение Шрёдингера",
            "pauli_x pauli_y pauli_z hadamard — пресеты кубитных вентилей",
            "A^(-1) — обратная; expm(J·θ) — вращение Ли; kron — тензорное произведение",
        ]),
        ("триты", &["trits trit_val trit_and trit_or trit_not"]),
        ("температура", &["degC(x) degF(x) — конструкторы: degC(25) to degF"]),
        ("астрономия", &[
            "sun_lon sun_ra sun_dec (y, m, d[, час UTC])",
            "moon_lon moon_lat moon_dist moon_phase moon_illum moon_age moon_ra moon_dec",
            "planet_lon planet_dist (\"mars\", y, m, d[, час])",
            "sunrise sunset (lat, lon, y, m, d) → часы UTC",
        ]),
        ("геодезия", &[
            "dist bearing (lat1, lon1, lat2, lon2) — км / град",
            "midpoint dest earth_radius",
        ]),
        ("визуализация (цикл U, сессия-8)", &[
            "viz(x) — авто: число → Белл/шкала, список → вероятности/спектр, матрица → теплокарта",
            "viz_bell(I) — радар CGLMP: классика 2.0 · Цирельсон 2.828 · Ацин 2.915 · квкварт 2.973",
            "viz_scale(x) — лог-шкала Вселенной в метрах: Планковская длина → наблюдаемая Вселенная",
            "viz_prob([p…]) — Born-вероятности столбцами · viz_bars([v…]) — спектр значений",
            "viz_matrix(M) — теплокарта матрицы до 64×64: красный ≥ 0, синий < 0",
            "вывод — SVG 1.1 строкой: сохраняйте в файл и открывайте глазами",
        ]),
        ("суверенный рендер (сессия-9)", &[
            "viz_graph(A, \"м1, м2, …\") — силовая укладка Fruchterman–Reingold: смежность → схема",
            "узлы: радиус/цвет по степени · рёбра: толщина по весу · компоненты union-find",
            "viz_field(M) — изолинии marching squares (7 уровней) поверх дивергентной карты",
            "viz_surf(M) — 3D-поверхность: поворот, ортоскопия, painter's-алгоритм (до 48×48)",
            "математика вырезана из Graphviz/NetworkX/Matplotlib — ноль зависимостей, чистый Rust",
        ]),
        ("GIS-ядро на тритах (сессия-12)", &[
            "de9im(кольцоA, кольцоB[, k]) — топология OGC 9-IM: 9 тритов → сбалансированный код",
            "матрица «−0+…» · предикаты: intersects/contains/within/touches/overlaps/equals…",
            "geo_pred(A, B[, k]) — список сработавших предикатов",
            "точность без epsilon: координаты квантуются в тритную решётку 3⁻ᵏ (k ≤ 12)",
            "viz_iso3(стек срезов, уровень) — изоповерхность: marching tetrahedra, watertight",
            "кольцо — матрица Nx2 [x,y; x,y; …] · стек — список матриц либо блочная (nz·ny)×nx",
        ]),
    ];
    let mut out = String::new();
    for (g, lines) in groups {
        if !filter.is_empty()
            && !g.contains(&filter.to_lowercase())
            && !lines.join(" ").contains(&filter.to_lowercase())
        {
            continue;
        }
        out.push_str(&format!("── {g}\n"));
        for l in *lines {
            out.push_str(&format!("  {l}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, args: &[Value]) -> Result<Value, String> {
        call_function(name, args)
    }
    fn s(v: f64) -> Value {
        Value::Scalar(v)
    }
    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * (1.0 + a.abs() + b.abs())
    }

    #[test]
    fn is_function_registry() {
        assert!(is_function("sin"));
        assert!(is_function("expm"));
        assert!(is_function("moon_illum"));
        assert!(!is_function("sinx"));
        assert!(!is_function("x"));
        assert!(!is_function("pi")); // константа, не функция
    }

    #[test]
    fn special_functions_anchors() {
        // Γ(0.5) = √π
        assert!(close(gamma_impl(0.5).unwrap(), std::f64::consts::PI.sqrt(), 1e-13));
        // Γ(6) = 120
        assert!(close(gamma_impl(6.0).unwrap(), 120.0, 1e-12));
        // Γ(−0.5) = −2√π (отражение)
        assert!(close(gamma_impl(-0.5).unwrap(), -2.0 * std::f64::consts::PI.sqrt(), 1e-12));
        // цикл N: полюса — расходимость на расширенной прямой (не ошибка)
        assert!(gamma_impl(-1.0).unwrap().is_infinite());
        assert!(gamma_impl(-2.0).unwrap().is_infinite());
        assert!(gamma_impl(0.0).unwrap().is_infinite());

        // erf: документированная точность A&S — 1.5e-7
        assert!(close(erf_impl(1.0), 0.8427007929497149, 1.5e-7));
        assert!(close(erf_impl(0.5), 0.5204998778130465, 1.5e-7));
        assert!(close(erf_impl(2.0), 0.9953222650189527, 1.5e-7));
        assert!(close(erf_impl(-1.0), -0.8427007929497149, 1.5e-7));
        assert!(close(erf_impl(0.0), 0.0, 1e-15));
        // erfc(1) — в зоне уверенности A&S (при x=3 катастрофично сокращение)
        assert!(close(1.0 - erf_impl(1.0), 0.15729920705028513, 1e-6));

        // ζ (Эйлер–Маклорен):
        assert!(close(zeta_impl(2.0).unwrap(), std::f64::consts::PI.powi(2) / 6.0, 1e-12));
        assert!(close(zeta_impl(4.0).unwrap(), std::f64::consts::PI.powi(4) / 90.0, 1e-12));
        assert!(close(zeta_impl(0.0).unwrap(), -0.5, 1e-15));
        assert!(close(zeta_impl(-1.0).unwrap(), -1.0 / 12.0, 1e-10));
        assert!(close(zeta_impl(-2.0).unwrap(), 0.0, 1e-15)); // тривиальный нуль
        assert!(close(zeta_impl(3.0).unwrap(), 1.2020569031595942, 1e-10));
        assert!(zeta_impl(1.0).unwrap().is_infinite()); // полюс: ζ(1) = +∞

        // beta(2,3) = 1/12
        let b = call("beta", &[s(2.0), s(3.0)]).unwrap();
        assert!(close(b.as_f64().unwrap(), 1.0 / 12.0, 1e-12));
    }

    // РЕГРЕССИЯ: потерянный внешний множитель t в Хорнере erf давал
    // erf(1) ≈ 0.665 (правильно 0.8427).
    #[test]
    fn erf_outer_t_factor_regression() {
        let v = erf_impl(1.0);
        assert!(v > 0.84 && v < 0.85, "erf(1) = {v}");
        let v = call("erf", &[s(1.0)]).unwrap();
        assert!(close(v.as_f64().unwrap(), 0.84270079, 1.5e-7));
    }

    #[test]
    fn trig_domain_checks() {
        // цикл N: домены расширены — «невозможное → возможное».
        // Вне старой области определения функции возвращают КОМПЛЕКСНЫЙ
        // результат или ∞, а не ошибку.
        // asin(2) = π/2 − i·acosh(2)
        match call("asin", &[s(2.0)]).unwrap() {
            Value::Complex(c) => {
                assert!(close(c.re, std::f64::consts::FRAC_PI_2, 1e-12));
                assert!(c.im < 0.0 && close(c.im, -(2.0f64 + 3.0f64.sqrt()).ln(), 1e-12));
            }
            _ => panic!("asin(2) должен быть комплексным"),
        }
        // acos(−1.5) = π − i·acosh(1.5)
        match call("acos", &[s(-1.5)]).unwrap() {
            Value::Complex(c) => {
                assert!(close(c.re, std::f64::consts::PI, 1e-12));
                assert!(c.im < 0.0);
            }
            _ => panic!("acos(−1.5) должен быть комплексным"),
        }
        // ln(−1) = iπ (главная ветвь)
        match call("ln", &[s(-1.0)]).unwrap() {
            Value::Complex(c) => {
                assert!(c.re.abs() < 1e-15);
                assert!(close(c.im, std::f64::consts::PI, 1e-12));
            }
            _ => panic!("ln(−1) должен быть iπ"),
        }
        // sqrt(−4) = 2i
        match call("sqrt", &[s(-4.0)]).unwrap() {
            Value::Complex(c) => {
                assert!(c.re.abs() < 1e-15);
                assert!(close(c.im, 2.0, 1e-12));
            }
            _ => panic!("sqrt(−4) должен быть 2i"),
        }
        // log(0) = −∞ (предел)
        assert_eq!(call("log", &[s(0.0)]).unwrap().as_f64(), Some(f64::NEG_INFINITY));
        // acosh(0.5) = i·acos(0.5) = i·π/3
        match call("acosh", &[s(0.5)]).unwrap() {
            Value::Complex(c) => {
                assert!(c.re.abs() < 1e-15);
                assert!(close(c.im, std::f64::consts::PI / 3.0, 1e-12));
            }
            _ => panic!("acosh(0.5) должен быть i·π/3"),
        }
        // atanh(1) = ∞ (расходимость на границе)
        assert_eq!(call("atanh", &[s(1.0)]).unwrap().as_f64(), Some(f64::INFINITY));
        // угловые величины
        let v = call("sin", &[Value::Quantity(180.0, units::by_name("deg").unwrap())]).unwrap();
        assert!(close(v.as_f64().unwrap(), 0.0, 1e-14));
    }

    #[test]
    fn aggregates() {
        assert!(close(
            call("mean", &[s(1.0), s(2.0), s(3.0), s(4.0)]).unwrap().as_f64().unwrap(),
            2.5,
            1e-15
        ));
        assert!(close(
            call("median", &[s(1.0), s(9.0), s(5.0)]).unwrap().as_f64().unwrap(),
            5.0,
            1e-15
        ));
        assert!(close(
            call("median", &[s(1.0), s(2.0), s(8.0), s(9.0)]).unwrap().as_f64().unwrap(),
            5.0,
            1e-15
        ));
        assert!(close(
            call("std", &[s(2.0), s(4.0), s(4.0), s(4.0), s(5.0), s(5.0), s(7.0), s(9.0)])
                .unwrap()
                .as_f64().unwrap(),
            2.0,
            1e-12
        ));
        assert!(close(
            call("max", &[s(3.0), s(7.0), s(2.0)]).unwrap().as_f64().unwrap(),
            7.0,
            1e-15
        ));
    }

    #[test]
    fn number_theory_calls() {
        assert_eq!(call("is_prime", &[s(97.0)]).unwrap().as_f64().unwrap(), 1.0);
        assert_eq!(call("is_prime", &[s(561.0)]).unwrap().as_f64().unwrap(), 0.0);
        assert_eq!(call("next_prime", &[s(13.0)]).unwrap().as_f64().unwrap(), 17.0);
        match call("factorize", &[s(360.0)]).unwrap() {
            Value::List(items) => {
                assert_eq!(items.len(), 6); // 2,3,3,5,1,1
                assert_eq!(items[0].as_f64().unwrap(), 2.0);
                assert_eq!(items[1].as_f64().unwrap(), 3.0);
            }
            other => panic!("{other:?}"),
        }
        match call("divisors", &[s(12.0)]).unwrap() {
            Value::List(items) => assert_eq!(items.len(), 6),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn list_mapping() {
        match call("sin", &[Value::List(vec![s(0.0), s(1.0)])]).unwrap() {
            Value::List(items) => {
                assert!(close(items[0].as_f64().unwrap(), 0.0, 1e-15));
                assert!(close(items[1].as_f64().unwrap(), 0.8414709848078965, 1e-15));
            }
            other => panic!("{other:?}"),
        }
        // вектор + скаляр — бродкаст
        match call_function("+", &[Value::List(vec![s(1.0), s(2.0)]), s(10.0)]) {
            Err(_) => {} // + — оператор, не функция; проверим через binary_op в parser-тестах
            Ok(v) => panic!("+ не функция: {v:?}"),
        }
    }

    #[test]
    fn temperature_constructors() {
        let c = call("degC", &[s(100.0)]).unwrap();
        let f = call("degF", &[Value::Scalar(0.0)]).unwrap();
        let _ = (c, f);
        // конверсия через parser: degC(100) to degF = 212
        let mut vars = std::collections::HashMap::new();
        let v = super::super::parser::parse_and_eval("degC(100) to degF", &vars).unwrap();
        assert!(close(v.as_f64().unwrap(), 212.0, 1e-9));
        vars.clear();
        let v = super::super::parser::parse_and_eval("degF(32) to degC", &vars).unwrap();
        assert!(close(v.as_f64().unwrap(), 0.0, 1e-9));
    }

    #[test]
    fn astro_calls() {
        // фаза Луны на солнечном затмении 2024-04-08 ~ 0
        let v = call("moon_illum", &[s(2024.0), s(4.0), s(8.0), s(18.35)]).unwrap();
        assert!(v.as_f64().unwrap() < 0.01);
        // солнце в равноденствие
        let v = call("sun_lon", &[s(2024.0), s(3.0), s(20.0), s(3.1)]).unwrap();
        let lon = v.as_f64().unwrap();
        assert!(lon < 1.5 || lon > 358.5, "sun_lon = {lon}");
        // планета
        let v = call("planet_lon", &[Value::Str("mars".into()), s(2024.0), s(6.0), s(1.0)])
            .unwrap();
        assert!((0.0..360.0).contains(&v.as_f64().unwrap()));
        // восход на экваторе в равноденствие
        let v = call("sunrise", &[s(0.0), s(0.0), s(2024.0), s(3.0), s(20.0)]).unwrap();
        assert!(close(v.as_f64().unwrap(), 6.0, 0.25));
    }

    #[test]
    fn geodesy_calls() {
        let d = call("dist", &[s(50.45), s(30.52), s(49.84), s(24.03)]).unwrap();
        let d = d.as_f64().unwrap();
        assert!(d > 450.0 && d < 475.0, "{d}");
        let b = call("bearing", &[s(0.0), s(0.0), s(0.0), s(10.0)]).unwrap();
        assert!(close(b.as_f64().unwrap(), 90.0, 1e-6));
        match call("midpoint", &[s(0.0), s(10.0), s(0.0), s(20.0)]).unwrap() {
            Value::List(items) => {
                assert!(close(items[1].as_f64().unwrap(), 15.0, 1e-9));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn arity_errors() {
        assert!(call("sin", &[]).is_err());
        assert!(call("atan2", &[s(1.0)]).is_err());
        assert!(call("gcd", &[s(1.0)]).is_err());
        assert!(call("nothing_here", &[]).is_err());
        assert!(call_function("expm", &[s(1.0)]).is_err()); // не матрица
    }

    #[test]
    fn catalog_lists() {
        assert!(catalog("").contains("gamma"));
        assert!(catalog("").contains("moon_illum"));
        assert!(!catalog("zeta").is_empty());
    }

    // ===== ГРАБЛИ сессии-4 → исправления сессии-5 =====

    #[test]
    fn sign_zero_returns_zero() {
        // было: sign(0) = 1.0 (f64::signum отдаёт 1.0 для +0.0)
        // — ломало симметрию разностей CGLMP-сканов
        assert_eq!(call("sign", &[s(0.0)]).unwrap(), s(0.0));
        assert_eq!(call("sign", &[s(-0.0)]).unwrap(), s(0.0));
        assert_eq!(call("sign", &[s(5.0)]).unwrap(), s(1.0));
        assert_eq!(call("sign", &[s(-5.0)]).unwrap(), s(-1.0));
    }

    #[test]
    fn abs_vector_and_frobenius_norm() {
        // вектор-строка [3, 4] → ‖v‖₂ = 5 (было: ошибка «не скаляр»)
        let m = Matrix::from_rows(&[vec![3.0, 4.0]]).unwrap();
        assert_eq!(call("abs", &[Value::Matrix(m)]).unwrap(), s(5.0));
        // вектор-столбец [3; 4]
        let m = Matrix::from_rows(&[vec![3.0], vec![4.0]]).unwrap();
        assert_eq!(call("abs", &[Value::Matrix(m)]).unwrap(), s(5.0));
        // матрица [1, 2; 3, 4] → ‖A‖_F = sqrt(1+4+9+16) = sqrt(30)
        let m = Matrix::from_rows(&[vec![1.0, 2.0], vec![3.0, 4.0]]).unwrap();
        let v = match call("abs", &[Value::Matrix(m)]).unwrap() {
            Value::Scalar(x) => x,
            other => panic!("не скаляр: {other:?}"),
        };
        assert!(close(v, 30.0f64.sqrt(), 1e-12), "{v}");
        // 1×1 по-прежнему модуль элемента
        let m = Matrix::from_rows(&[vec![-7.0]]).unwrap();
        assert_eq!(call("abs", &[Value::Matrix(m)]).unwrap(), s(7.0));
    }

    #[test]
    fn eye_beyond_64_qutrit_pipelines() {
        // было: eye(n) ≤ 64 — кутритные конвейеры 3^4=81, 3^6=729 не собирались
        let v = call("eye", &[s(81.0)]).unwrap();
        match v {
            Value::Matrix(m) => {
                assert_eq!((m.rows, m.cols), (81, 81));
                // единичная: след = n
                let tr: f64 = (0..81).map(|i| m.get(i, i).re).sum();
                assert!(close(tr, 81.0, 1e-12));
            }
            other => panic!("не матрица: {other:?}"),
        }
        let v = call("eye", &[s(729.0)]).unwrap();
        match v {
            Value::Matrix(m) => assert_eq!((m.rows, m.cols), (729, 729)),
            other => panic!("не матрица: {other:?}"),
        }
        // потолок 4096 остаётся защитой dense-памяти (268 МБ)
        assert!(call("eye", &[s(4097.0)]).is_err());
        assert!(call("eye", &[s(0.0)]).is_err());
        assert!(call("eye", &[s(2.5)]).is_err());
    }

    #[test]
    fn viz_session9_sovereign_render_calls() {
        // Сессия-9: реестр + диспетчер для графа/поля/поверхности.
        assert!(is_function("viz_graph"));
        assert!(is_function("viz_field"));
        assert!(is_function("viz_surf"));
        assert!(!is_function("viz_graf"));

        // K4 с метками одной строкой через запятую (грабля 16).
        let k4 = Value::Matrix(
            Matrix::from_rows(&[
                vec![0.0, 1.0, 1.0, 1.0],
                vec![1.0, 0.0, 1.0, 1.0],
                vec![1.0, 1.0, 0.0, 1.0],
                vec![1.0, 1.0, 1.0, 0.0],
            ])
            .unwrap(),
        );
        let v = call(
            "viz_graph",
            &[k4.clone(), Value::Str("a, b, c, d".into())],
        )
        .unwrap();
        match v {
            Value::Str(svg) => {
                assert!(svg.starts_with("<svg"));
                assert!(svg.ends_with("</svg>"));
                assert!(svg.contains("рёбер = 6"));
                assert!(svg.contains("компонент = 1"));
                assert!(svg.contains(">a<") && svg.contains(">d<"));
            }
            other => panic!("viz_graph: не строка {other:?}"),
        }
        // Без меток — авто-имена v0…v3.
        match call("viz_graph", &[k4]).unwrap() {
            Value::Str(svg) => assert!(svg.contains(">v0<")),
            other => panic!("viz_graph: не строка {other:?}"),
        }
        // Метки не строкой и неверное число меток — ошибки.
        let two = Value::Matrix(Matrix::from_rows(&[vec![0.0, 1.0], vec![1.0, 0.0]]).unwrap());
        assert!(call("viz_graph", &[two.clone(), Value::Scalar(1.0)]).is_err());
        assert!(call("viz_graph", &[two, Value::Str("одна".into())]).is_err());

        // Поле и поверхность через тот же реестр.
        let f = Matrix::from_rows(&[vec![0.0, 1.0, 0.0], vec![1.0, 2.0, 1.0]]).unwrap();
        match call("viz_field", &[Value::Matrix(f.clone())]).unwrap() {
            Value::Str(svg) => assert!(svg.contains("изолинии: 7 уровней")),
            other => panic!("viz_field: не строка {other:?}"),
        }
        match call("viz_surf", &[Value::Matrix(f)]).unwrap() {
            Value::Str(svg) => assert!(svg.contains("квадов 2")),
            other => panic!("viz_surf: не строка {other:?}"),
        }
        // Каталог знает новую группу.
        assert!(catalog("").contains("суверенный рендер"));
    }

    // ─────────── GIS-ядро на тритах (сессия-12) ───────────

    fn ring_of(pts: &[(f64, f64)]) -> Value {
        Value::Matrix(
            Matrix::from_rows(
                &pts.iter().map(|&(x, y)| vec![x, y]).collect::<Vec<_>>(),
            )
            .unwrap(),
        )
    }

    #[test]
    fn de9im_overlapping_and_nested() {
        let a = ring_of(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let b = ring_of(&[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]);
        match call("de9im", &[a.clone(), b]).unwrap() {
            Value::Str(rep) => {
                assert!(rep.contains("overlaps"), "отчёт: {rep}");
                assert!(rep.contains("код"));
            }
            other => panic!("de9im: не строка {other:?}"),
        }
        let big = ring_of(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        let small = ring_of(&[(3.0, 3.0), (5.0, 3.0), (5.0, 5.0), (3.0, 5.0)]);
        match call("geo_pred", &[big, small]).unwrap() {
            Value::List(items) => {
                let names: Vec<String> = items
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => s.clone(),
                        o => format!("{o}"),
                    })
                    .collect();
                assert!(names.contains(&"contains".to_string()), "{names:?}");
                assert!(!names.contains(&"overlaps".to_string()));
            }
            other => panic!("geo_pred: не список {other:?}"),
        }
    }

    #[test]
    fn de9im_point_and_disjoint() {
        let sq = ring_of(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let inside = ring_of(&[(2.0, 2.0)]);
        match call("geo_pred", &[inside, sq]).unwrap() {
            Value::List(items) => {
                let names: Vec<String> = items
                    .iter()
                    .map(|v| match v {
                        Value::Str(s) => s.clone(),
                        o => format!("{o}"),
                    })
                    .collect();
                assert!(names.contains(&"within".to_string()), "{names:?}");
            }
            other => panic!("geo_pred: не список {other:?}"),
        }
        let far = ring_of(&[(9.0, 9.0)]);
        match call("de9im", &[far, ring_of(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)])]).unwrap() {
            Value::Str(rep) => assert!(rep.contains("disjoint")),
            other => panic!("de9im: не строка {other:?}"),
        }
        // Неверный масштаб — ошибка.
        assert!(call("de9im", &[ring_of(&[(0.0, 0.0)]), ring_of(&[(1.0, 1.0)]), s(13.0)]).is_err());
    }

    #[test]
    fn viz_iso3_sphere_scene() {
        // Сфера как стек z-срезов (3 среза 8×8): f = r − d.
        let n = 8usize;
        let c = (n - 1) as f64 / 2.0;
        let r = n as f64 * 0.4;
        let slices: Vec<Value> = (0..3)
            .map(|iz| {
                let z = iz as f64;
                let mut rows: Vec<Vec<f64>> = Vec::new();
                for iy in 0..n {
                    let mut row: Vec<f64> = Vec::new();
                    for ix in 0..n {
                        let d = (
                            (ix as f64 - c).powi(2)
                                + (iy as f64 - c).powi(2)
                                + (z - c).powi(2)
                        )
                        .sqrt();
                        row.push(r - d);
                    }
                    rows.push(row);
                }
                Value::Matrix(Matrix::from_rows(&rows).unwrap())
            })
            .collect();
        match call("viz_iso3", &[Value::List(slices)]).unwrap() {
            Value::Str(svg) => {
                assert!(svg.starts_with("<svg"));
                assert!(svg.contains("marching tetrahedra"));
                assert!(svg.contains("треугольников"));
            }
            other => panic!("viz_iso3: не строка {other:?}"),
        }
        // Уровень вне поля — ошибка.
        let flat = Value::Matrix(
            Matrix::from_rows(&[vec![1.0, 1.0], vec![1.0, 1.0]]).unwrap(),
        );
        assert!(call("viz_iso3", &[flat]).is_err());
    }
}
