//! Семантический визуализатор (цикл U, Сессия-8): мост «машина → человек».
//!
//! Идея из полевого диалога (Текстовий файл (2).txt): Калькулятор выдаёт
//! точные значения — 1.616255e-35, I₃ = 2.914854 — но для человека это
//! «ни о чём». Нужен автоматический слой: тип значения → правило → сцена
//! с человеческими якорями. Здесь этот слой реализован как семейство
//! viz-функций, возвращающих компактный SVG 1.1 (UTF-8, monospace,
//! ноль внешних зависимостей — чистые строки).
//!
//! Правила визуализации:
//! - число в [2, 3.2]   → радар нарушения Белла (CGLMP);
//! - положительное число → лог-шкала Вселенной с якорями;
//! - список вероятностей → столбцы Born-распределения;
//! - прочий список       → спектр (бары);
//! - матрица             → теплокарта (знак → цвет, |v| → интенсивность);
//! - комплексное         → бары Re/Im.
//!
//! Грабля 16: литерал `[a, b, c]` в языке Калькулятора — это матрица-строка
//! (не Value::List): viz-слой принимает 1×N/N×1 как список чисел, так что
//! `viz([0.8, 0.2])` даёт Born-бары, а `viz([[1,2],[3,4]])` — теплокарту.
//!
//! Физические якоря замкнуты на результаты Сессий 5–7:
//! классика 2.0 · Цирельсон √8 ≈ 2.828427 · Ацин d=3 ≈ 2.914854 ·
//! квкварт d=4 ≈ 2.972698.

use std::collections::HashMap;

use super::matrix::Matrix;
use super::Value;

// ---------------------------------------------------------------------
// Якоря физики (Сессии 5–7) и шкалы Вселенной
// ---------------------------------------------------------------------

/// Классический локальный предел CGLMP (любое d).
const BELL_CLASSICAL: f64 = 2.0;
/// Цирельсон для кубитов: √8.
const BELL_TSIRELSON: f64 = 2.8284271247461903;
/// Оптимум Ацина d=3 (Сессия 6): 1 + √(11/3).
const BELL_ACIN: f64 = 2.9148542161522637;
/// Некомпактный оптимум d=4 (Сессия 7).
const BELL_QUQUART: f64 = 2.972698267102243;

/// Человеческие якоря лог-шкалы (метры): от Планковской длины
/// до наблюдаемой Вселенной.
const SCALE_ANCHORS: [(&str, f64); 14] = [
    ("Планковская длина", 1.616255e-35),
    ("протон", 1.68e-15),
    ("атом H", 1.06e-10),
    ("ДНК (ширина)", 2.5e-9),
    ("вирус", 1.0e-7),
    ("эритроцит", 8.0e-6),
    ("человек", 1.7),
    ("Эверест", 8.849e3),
    ("Земля", 1.2742e7),
    ("Солнце", 1.3927e9),
    ("орбита Земли", 2.99e11),
    ("парсек", 3.0857e16),
    ("Млечный Путь", 9.5e20),
    ("Вселенная", 8.8e26),
];

/// Человеческий эталон шкалы: рост в метрах.
const HUMAN_M: f64 = 1.7;

// Палитра (единая для всех сцен).
const C_GREEN: &str = "#16a34a";
const C_GREEN_BG: &str = "#dcfce7";
const C_PURPLE: &str = "#7c3aed";
const C_ORANGE: &str = "#ea580c";
const C_RED: &str = "#dc2626";
const C_BLUE: &str = "#2563eb";
const C_BLUE_BG: &str = "#93c5fd";
const C_TEXT: &str = "#111827";
const C_MUTED: &str = "#6b7280";
const C_GRID: &str = "#d1d5db";

// ---------------------------------------------------------------------
// Помощники форматирования и SVG-гигиены
// ---------------------------------------------------------------------

/// XML-экранирование текстовых узлов и атрибутов.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Человекочитаемое число: обычное — без хвостовых нулей,
/// крайние порядки — инженерная запись (1.616e-35).
fn num(x: f64) -> String {
    if !x.is_finite() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    let a = x.abs();
    if (1e-4..1e6).contains(&a) {
        let s = format!("{:.6}", x);
        let s = s.trim_end_matches('0').trim_end_matches('.');
        s.to_string()
    } else {
        format!("{:.3e}", x)
    }
}

/// Отношение к человеку: «в 3.5 раза», «в 1.052e35 раз».
fn human_ratio(x: f64) -> String {
    if x < 1e4 {
        format!("{:.2}", x)
    } else {
        format!("{:.3e}", x)
    }
}

fn svg_open(w: u32, h: u32) -> String {
    format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {w} {h}\" \
         font-family=\"monospace\">\n<rect width=\"{w}\" height=\"{h}\" fill=\"#ffffff\"/>\n"
    )
}

fn title(s: &str, w: u32) -> String {
    format!(
        "<text x=\"{}\" y=\"22\" font-size=\"14\" font-weight=\"bold\" fill=\"{}\" \
         text-anchor=\"middle\">{}</text>\n",
        w / 2,
        C_TEXT,
        esc(s)
    )
}

// ---------------------------------------------------------------------
// viz_bell: радар нарушения неравенства Белла
// ---------------------------------------------------------------------

/// Радар CGLMP: ось I ∈ [1.5, 3.2], зелёная зона классики,
/// вертикали Цирельсона/Ацина/квкварта, маркер значения, вердикт.
pub fn bell_svg(i: f64) -> String {
    const X0: f64 = 60.0;
    const X1: f64 = 610.0;
    const V0: f64 = 1.5;
    const V1: f64 = 3.2;
    let xf = |v: f64| X0 + (v - V0) * (X1 - X0) / (V1 - V0);
    let clamp = |v: f64| v.clamp(V0, V1);

    let mut s = svg_open(640, 240);
    s.push_str(&title("CGLMP: нарушение неравенства Белла", 640));

    // Зона классики: заливка от левого края до 2.0.
    s.push_str(&format!(
        "<rect x=\"{:.1}\" y=\"48\" width=\"{:.1}\" height=\"120\" fill=\"{}\"/>\n",
        X0,
        xf(BELL_CLASSICAL) - X0,
        C_GREEN_BG
    ));
    s.push_str(&format!(
        "<text x=\"{:.1}\" y=\"44\" font-size=\"11\" fill=\"{}\">классика ≤ 2.0</text>\n",
        X0 + 6.0,
        C_GREEN
    ));

    // Оси и сетка.
    s.push_str(&format!(
        "<line x1=\"{X0}\" y1=\"168\" x2=\"{X1}\" y2=\"168\" stroke=\"{}\"/>\n",
        C_GRID
    ));
    for v in [1.5f64, 2.0, 2.5, 3.0] {
        s.push_str(&format!(
            "<line x1=\"{:.1}\" y1=\"168\" x2=\"{:.1}\" y2=\"173\" stroke=\"{}\"/>\n\
             <text x=\"{:.1}\" y=\"186\" font-size=\"10\" fill=\"{}\" text-anchor=\"middle\">{}</text>\n",
            xf(v),
            xf(v),
            C_MUTED,
            xf(v),
            C_MUTED,
            num(v)
        ));
    }

    // Квантовые вертикали: Цирельсон, Ацин d=3, квкварт d=4.
    let markers: [(f64, &str, &str); 3] = [
        (BELL_TSIRELSON, "Цирельсон √8 (кубиты)", C_PURPLE),
        (BELL_ACIN, "Ацин d=3", C_ORANGE),
        (BELL_QUQUART, "квкварт d=4", C_RED),
    ];
    for (v, label, color) in markers {
        let x = xf(v);
        s.push_str(&format!(
            "<line x1=\"{x:.1}\" y1=\"52\" x2=\"{x:.1}\" y2=\"168\" stroke=\"{color}\" \
             stroke-dasharray=\"5 3\" stroke-width=\"1.6\"/>\n\
             <text x=\"{x:.1}\" y=\"{}\" font-size=\"10\" fill=\"{color}\" \
             transform=\"rotate(-38 {x:.1} {})\" font-weight=\"bold\">{}</text>\n",
            128.0,
            128.0,
            esc(label)
        ));
    }

    // Маркер значения.
    let xv = xf(clamp(i));
    s.push_str(&format!(
        "<line x1=\"{xv:.1}\" y1=\"48\" x2=\"{xv:.1}\" y2=\"168\" stroke=\"{}\" stroke-width=\"2.6\"/>\n\
         <circle cx=\"{xv:.1}\" cy=\"108\" r=\"6\" fill=\"#ffffff\" stroke=\"{}\" stroke-width=\"2.6\"/>\n",
        C_RED, C_RED
    ));
    s.push_str(&format!(
        "<text x=\"{xv:.1}\" y=\"98\" font-size=\"12\" font-weight=\"bold\" fill=\"{}\" \
         text-anchor=\"middle\">I = {}</text>\n",
        C_RED,
        num(i)
    ));

    // Вердикт: человеческая расшифровка.
    let (line1, line2) = bell_verdict(i);
    s.push_str(&format!(
        "<text x=\"60\" y=\"208\" font-size=\"12\" fill=\"{}\">{}</text>\n\
         <text x=\"60\" y=\"226\" font-size=\"11\" fill=\"{}\">{}</text>\n</svg>",
        C_TEXT,
        esc(&line1),
        C_MUTED,
        esc(&line2)
    ));
    s
}

/// Человекочитаемый вердикт радара: превышение над классикой и
/// Цирельсоном, ранг размерности, порог шума η* = 2/I.
fn bell_verdict(i: f64) -> (String, String) {
    if !i.is_finite() {
        return ("I не конечен — нечего визуализировать".into(), String::new());
    }
    if i <= BELL_CLASSICAL {
        return (
            format!("I = {} ≤ 2.0 — внутри классического предела", num(i)),
            "локальные стратегии справляются: телепатии нет".into(),
        );
    }
    let over_classical = (i / BELL_CLASSICAL - 1.0) * 100.0;
    let mut line1 = format!(
        "I = {} · +{:.2}% над классикой",
        num(i),
        over_classical
    );
    let mut line2 = format!("η* = 2/I = {:.4} (порог шума)", 2.0 / i);
    if i > BELL_TSIRELSON {
        line1.push_str(&format!(
            " · +{:.2}% над Цирельсоном",
            (i / BELL_TSIRELSON - 1.0) * 100.0
        ));
        let rank = [
            (BELL_TSIRELSON, "кубиты (Цирельсон)"),
            (BELL_ACIN, "кутриты (Ацин d=3)"),
            (BELL_QUQUART, "квкварты (d=4)"),
        ]
        .iter()
        .min_by(|a, b| {
            (a.0 - i).abs().partial_cmp(&(b.0 - i).abs()).unwrap()
        })
        .map(|(_, r)| *r)
        .unwrap_or("?");
        line2.push_str(&format!(" · ближайший ранг: {rank}"));
    } else {
        line2.push_str(" — ниже кубитного предела √8");
    }
    (line1, line2)
}

// ---------------------------------------------------------------------
// viz_scale: лог-шкала Вселенной с человеческими якорями
// ---------------------------------------------------------------------

/// Лог-шкала Вселенной (метры): 1e-35 → 1e+27, якоря от Планковской
/// длины до наблюдаемой Вселенной, позиция значения и отношение
/// к человеческому росту.
pub fn scale_svg(x_m: f64) -> Result<String, String> {
    if !x_m.is_finite() || x_m <= 0.0 {
        return Err(format!(
            "viz_scale: нужно x > 0 (логарифмическая шкала), получено {}",
            num(x_m)
        ));
    }
    const X0: f64 = 70.0;
    const X1: f64 = 620.0;
    const L0: f64 = -35.0;
    const L1: f64 = 27.0;
    let xf = |lg: f64| X0 + (lg - L0) * (X1 - X0) / (L1 - L0);

    let mut s = svg_open(640, 300);
    s.push_str(&title("Шкала Вселенной (логарифм, метры)", 640));

    // Полоса шкалы и декадные засечки.
    s.push_str(&format!(
        "<rect x=\"{X0}\" y=\"130\" width=\"{:.1}\" height=\"40\" fill=\"#f3f4f6\" \
         stroke=\"{}\"/>\n",
        X1 - X0,
        C_GRID
    ));
    for l in [-30i32, -20, -10, 0, 10, 20] {
        let x = xf(l as f64);
        s.push_str(&format!(
            "<line x1=\"{x:.1}\" y1=\"130\" x2=\"{x:.1}\" y2=\"170\" stroke=\"{}\"/>\n\
             <text x=\"{x:.1}\" y=\"184\" font-size=\"9\" fill=\"{}\" \
             text-anchor=\"middle\">1e{l}</text>\n",
            C_GRID, C_MUTED
        ));
    }

    // Якоря: чередуем подписи сверху/снизу полосы.
    for (k, (name, m)) in SCALE_ANCHORS.iter().enumerate() {
        let x = xf(m.log10());
        let up = k % 2 == 0;
        let (y, anchor) = if up { (122.0, "end") } else { (196.0, "start") };
        s.push_str(&format!(
            "<line x1=\"{x:.1}\" y1=\"130\" x2=\"{x:.1}\" y2=\"170\" stroke=\"{}\"/>\n\
             <text x=\"{x:.1}\" y=\"{y}\" font-size=\"9\" fill=\"{}\" \
             text-anchor=\"{anchor}\">{}</text>\n",
            C_MUTED,
            C_MUTED,
            esc(name)
        ));
    }

    // Позиция значения.
    let lg = x_m.log10();
    let xv = xf(lg.clamp(L0, L1));
    s.push_str(&format!(
        "<line x1=\"{xv:.1}\" y1=\"118\" x2=\"{xv:.1}\" y2=\"182\" stroke=\"{}\" \
         stroke-width=\"2.6\"/>\n\
         <text x=\"{xv:.1}\" y=\"106\" font-size=\"12\" font-weight=\"bold\" fill=\"{}\" \
         text-anchor=\"middle\">{} м</text>\n",
        C_RED,
        C_RED,
        num(x_m)
    ));

    // Вердикт: между какими якорями и отношение к человеку.
    let (line1, line2) = scale_verdict(x_m);
    s.push_str(&format!(
        "<text x=\"70\" y=\"234\" font-size=\"12\" fill=\"{}\">{}</text>\n\
         <text x=\"70\" y=\"252\" font-size=\"11\" fill=\"{}\">{}</text>\n\
         <text x=\"70\" y=\"270\" font-size=\"10\" fill=\"{}\">{}</text>\n</svg>",
        C_TEXT,
        esc(&line1),
        C_TEXT,
        esc(&line2),
        C_MUTED,
        esc("якоря: Планк → протон → атом → ДНК → вирус → эритроцит → человек → Эверест → Земля → Солнце → орбита → парсек → Галактика → Вселенная")
    ));
    Ok(s)
}

/// Якорный вердикт: соседние якоря и отношение к росту человека.
fn scale_verdict(x_m: f64) -> (String, String) {
    // Соседние якоря: последний ≤ x и первый > x.
    let mut below: Option<(usize)> = None;
    let mut above: Option<usize> = None;
    for (k, (_, m)) in SCALE_ANCHORS.iter().enumerate() {
        if *m <= x_m {
            below = Some(k);
        } else if above.is_none() {
            above = Some(k);
        }
    }
    let mut line1 = String::new();
    match (below, above) {
        (Some(b), Some(a)) => line1.push_str(&format!(
            "между «{}» и «{}»",
            SCALE_ANCHORS[b].0, SCALE_ANCHORS[a].0
        )),
        (Some(b), None) => line1.push_str(&format!(
            "за пределом «{}» — крупнее наблюдаемой Вселенной",
            SCALE_ANCHORS[b].0
        )),
        (None, Some(a)) => line1.push_str(&format!(
            "мельче «{}» — глубже Планковской длины",
            SCALE_ANCHORS[a].0
        )),
        (None, None) => line1.push_str("вне шкалы"),
    }

    // Отношение к человеку.
    let ratio = x_m / HUMAN_M;
    let line2 = if ratio.log10().abs() < 0.01 {
        format!("это и есть человеческий масштаб ({HUMAN_M} м)")
    } else if ratio > 1.0 {
        format!(
            "в {} раз больше человека ({HUMAN_M} м)",
            human_ratio(ratio)
        )
    } else {
        format!(
            "в {} раз меньше человека ({HUMAN_M} м)",
            human_ratio(1.0 / ratio)
        )
    };
    (line1, line2)
}

// ---------------------------------------------------------------------
// viz_prob: Born-вероятности столбцами
// ---------------------------------------------------------------------

/// Столбчатая диаграмма вероятностей: нормировка по максимуму,
/// проценты от суммы, подсветка argmax, контроль Σ.
pub fn prob_svg(ps: &[f64]) -> Result<String, String> {
    if ps.is_empty() {
        return Err("viz_prob: пустой список вероятностей".into());
    }
    if ps.iter().any(|p| !p.is_finite() || *p < 0.0) {
        return Err("viz_prob: вероятности должны быть ≥ 0".into());
    }
    if ps.len() > 40 {
        return Err(format!(
            "viz_prob: слишком много исходов ({}, до 40)",
            ps.len()
        ));
    }
    let n = ps.len() as f64;
    let bw = (540.0 / n - 8.0).min(78.0);
    let x0 = 60.0 + (540.0 - (bw + 8.0) * n) / 2.0;
    let sum: f64 = ps.iter().sum();
    let pmax = ps.iter().cloned().fold(0.0, f64::max).max(1e-12);
    let amax = ps
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
        .map(|(k, _)| k)
        .unwrap_or(0);

    let mut s = svg_open(640, 240);
    s.push_str(&title("Born: вероятности исходов", 640));
    s.push_str(&format!(
        "<line x1=\"60\" y1=\"180\" x2=\"600\" y2=\"180\" stroke=\"{}\"/>\n",
        C_GRID
    ));
    for (k, p) in ps.iter().enumerate() {
        let x = x0 + (bw + 8.0) * k as f64;
        let h = (p / pmax * 118.0).max(1.5);
        let fill = if k == amax { C_BLUE } else { C_BLUE_BG };
        s.push_str(&format!(
            "<rect x=\"{x:.1}\" y=\"{:.1}\" width=\"{bw:.1}\" height=\"{h:.1}\" \
             fill=\"{fill}\"/>\n",
            180.0 - h
        ));
        s.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"10\" fill=\"{}\" \
             text-anchor=\"middle\">{:.2}%</text>\n",
            x + bw / 2.0,
            172.0 - h,
            if k == amax { C_BLUE } else { C_MUTED },
            if sum > 0.0 { p / sum * 100.0 } else { 0.0 }
        ));
        s.push_str(&format!(
            "<text x=\"{:.1}\" y=\"196\" font-size=\"10\" fill=\"{}\" \
             text-anchor=\"middle\">p{k}</text>\n",
            x + bw / 2.0,
            C_MUTED
        ));
    }
    s.push_str(&format!(
        "<text x=\"60\" y=\"222\" font-size=\"12\" fill=\"{}\">Σ = {} · максимум \
         p{amax} = {} · Born-коллапс: один исход станет реальностью</text>\n</svg>",
        C_TEXT,
        num(sum),
        num(ps[amax])
    ));
    Ok(s)
}

// ---------------------------------------------------------------------
// viz_matrix: теплокарта матрицы
// ---------------------------------------------------------------------

/// Теплокарта: v ≥ 0 → красная интенсивность, v < 0 → синяя,
/// |v|/max нормирует яркость; подписи клеток при n ≤ 8.
/// Ограничение 64×64 (4096 клеток) — гигиена размера сцены.
pub fn matrix_svg(m: &Matrix) -> Result<String, String> {
    if m.rows == 0 || m.cols == 0 {
        return Err("viz_matrix: пустая матрица".into());
    }
    if m.rows > 64 || m.cols > 64 {
        return Err(format!(
            "viz_matrix: {}×{} — слишком крупно для сцены (до 64×64)",
            m.rows, m.cols
        ));
    }
    const CELL: f64 = 46.0;
    let (ox, oy) = (56.0, 34.0);
    let w = ox + m.cols as f64 * CELL + 8.0;
    let h = oy + m.rows as f64 * CELL + 58.0;
    let label_cells = m.rows <= 8 && m.cols <= 8;

    let vmax = (0..m.rows)
        .flat_map(|i| (0..m.cols).map(move |j| m.get(i, j)))
        .map(|c| c.re.hypot(c.im))
        .fold(0.0f64, f64::max)
        .max(1e-12);

    let mut s = svg_open(w as u32, h as u32);
    s.push_str(&title(&format!("Матрица {}×{}", m.rows, m.cols), w as u32));
    for i in 0..m.rows {
        for j in 0..m.cols {
            let c = m.get(i, j);
            let mag = c.re.hypot(c.im) / vmax;
            let base = if c.re >= 0.0 { "#ef4444" } else { "#3b82f6" };
            let x = ox + j as f64 * CELL;
            let y = oy + i as f64 * CELL;
            // Полупрозрачная интенсивность поверх белого.
            s.push_str(&format!(
                "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{}\" height=\"{}\" \
                 fill=\"#ffffff\" stroke=\"{}\"/>\n\
                 <rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{}\" height=\"{}\" \
                 fill=\"{base}\" fill-opacity=\"{:.3}\" stroke=\"{}\"/>\n",
                CELL - 1.0,
                CELL - 1.0,
                C_GRID,
                CELL - 1.0,
                CELL - 1.0,
                (0.12 + 0.88 * mag).min(1.0),
                C_GRID
            ));
            if label_cells {
                let dark = mag > 0.55;
                s.push_str(&format!(
                    "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"10\" fill=\"{}\" \
                     text-anchor=\"middle\">{}</text>\n",
                    x + CELL / 2.0 - 1.0,
                    y + CELL / 2.0 + 4.0,
                    if dark { "#ffffff" } else { C_TEXT },
                    num(c.re)
                ));
            }
        }
    }
    // След по вещественной части (грабля 10: мнимая пыль 1e-17i).
    let tr: f64 = (0..m.rows.min(m.cols)).map(|k| m.get(k, k).re).sum();
    s.push_str(&format!(
        "<text x=\"{ox:.0}\" y=\"{:.1}\" font-size=\"11\" fill=\"{}\">{}×{} · \
         |max| = {} · Re(след) = {} · красный ≥ 0, синий < 0</text>\n</svg>",
        h - 34.0,
        C_TEXT,
        m.rows,
        m.cols,
        num(vmax),
        num(tr)
    ));
    Ok(s)
}

// ---------------------------------------------------------------------
// viz_bars: спектр значений
// ---------------------------------------------------------------------

/// Столбцы спектра: положительные — вверх (синие), отрицательные —
/// вниз (оранжевые); подписи значений и сводка min/max/среднее.
pub fn bars_svg(vs: &[f64]) -> Result<String, String> {
    bars_svg_titled(vs, "Спектр значений")
}

/// То же с произвольным заголовком (для авто-маршрута Re/Im).
pub fn bars_svg_titled(vs: &[f64], head: &str) -> Result<String, String> {
    if vs.is_empty() {
        return Err("viz_bars: пустой список".into());
    }
    if vs.iter().any(|v| !v.is_finite()) {
        return Err("viz_bars: не конечные значения".into());
    }
    if vs.len() > 40 {
        return Err(format!(
            "viz_bars: слишком много значений ({}, до 40)",
            vs.len()
        ));
    }
    let n = vs.len() as f64;
    let bw = (540.0 / n - 8.0).min(78.0);
    let x0 = 60.0 + (540.0 - (bw + 8.0) * n) / 2.0;
    let vmax = vs.iter().cloned().fold(0.0f64, f64::max)
        .max(vs.iter().cloned().fold(0.0f64, f64::min).abs())
        .max(1e-12);
    let base_y = 130.0; // ноль спектра

    let mut s = svg_open(640, 240);
    s.push_str(&title(head, 640));
    s.push_str(&format!(
        "<line x1=\"60\" y1=\"{base_y}\" x2=\"600\" y2=\"{base_y}\" stroke=\"{}\"/>\n",
        C_GRID
    ));
    for (k, v) in vs.iter().enumerate() {
        let x = x0 + (bw + 8.0) * k as f64;
        let frac = v / vmax; // ∈ [-1, 1]
        let (y, hh) = if frac >= 0.0 {
            (base_y - frac * 62.0, frac * 62.0)
        } else {
            (base_y, -frac * 62.0)
        };
        let fill = if frac >= 0.0 { C_BLUE } else { C_ORANGE };
        s.push_str(&format!(
            "<rect x=\"{x:.1}\" y=\"{y:.1}\" width=\"{bw:.1}\" height=\"{:.1}\" \
             fill=\"{fill}\" fill-opacity=\"0.85\"/>\n",
            hh.max(1.5)
        ));
        let ly = if frac >= 0.0 { y - 6.0 } else { y + hh + 14.0 };
        s.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{ly:.1}\" font-size=\"9\" fill=\"{}\" \
             text-anchor=\"middle\">{}</text>\n",
            x + bw / 2.0,
            C_MUTED,
            num(*v)
        ));
        s.push_str(&format!(
            "<text x=\"{:.1}\" y=\"208\" font-size=\"10\" fill=\"{}\" \
             text-anchor=\"middle\">[{k}]</text>\n",
            x + bw / 2.0,
            C_MUTED
        ));
    }
    let mean = vs.iter().sum::<f64>() / n;
    s.push_str(&format!(
        "<text x=\"60\" y=\"228\" font-size=\"12\" fill=\"{}\">n = {} · min = {} · \
         max = {} · среднее = {}</text>\n</svg>",
        C_TEXT,
        vs.len(),
        num(vs.iter().cloned().fold(f64::INFINITY, f64::min)),
        num(vs.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
        num(mean)
    ));
    Ok(s)
}

// ---------------------------------------------------------------------
// viz(x): автоматический маршрут «тип значения → правило → сцена»
// ---------------------------------------------------------------------

/// Автодиспетчер — тот самый «автоматический слой» из идеи:
/// Калькулятор посчитал, слой сам выбирает наглядную сцену
/// по типу значения.
pub fn auto_svg(v: &Value) -> Result<String, String> {
    match v {
        Value::Matrix(m) => {
            // Вектор-матрица (1×N или N×1) — это список чисел
            // в синтаксисе языка: [0.8, 0.2] → Born-бары.
            if let Some(ns) = matrix_as_vector(m) {
                route_numbers(&ns)
            } else {
                matrix_svg(m)
            }
        }
        Value::List(items) => {
            let nums: Option<Vec<f64>> = items
                .iter()
                .map(|x| match x {
                    Value::Scalar(f) => Some(*f),
                    Value::Quantity(q, u) if u.is_dimensionless() => Some(*q),
                    _ => None,
                })
                .collect();
            match nums {
                Some(ns) => route_numbers(&ns),
                None => Err(
                    "viz(список): элементы должны быть числами — например viz_bars([1, -2, 3])"
                        .into(),
                ),
            }
        }
        Value::Quantity(q, u) => {
            let pure_length = u.dim[0] == 1 && u.dim[1..].iter().all(|d| *d == 0);
            if pure_length {
                scale_svg(q * u.factor)
            } else if u.is_dimensionless() {
                auto_scalar(*q)
            } else {
                Err(format!(
                    "viz: величина в «{}» — шкала Вселенной в метрах; \
                     переведите в длину или подайте число",
                    u.display()
                ))
            }
        }
        Value::Scalar(x) => auto_scalar(*x),
        Value::Complex(c) => {
            bars_svg_titled(&[c.re, c.im], "Комплексное число: Re и Im")
        }
        Value::BigInt(n) => Err(format!(
            "viz(BigInt с {N} цифрами): точная большая арифметика — вне \
             лог-шкалы; подайте log10 вручную",
            N = n.trim_start_matches('-').len()
        )),
        Value::Str(s) => Err(format!("viz(строка «{s}»): нечего визуализировать")),
    }
}

/// Маршрут числового набора: вероятности (Σ ≈ 1, все ≥ 0) →
/// Born-бары, иначе спектр.
fn route_numbers(ns: &[f64]) -> Result<String, String> {
    let sum: f64 = ns.iter().sum();
    if !ns.is_empty() && ns.iter().all(|p| *p >= 0.0) && (sum - 1.0).abs() < 0.02 {
        prob_svg(ns)
    } else {
        bars_svg(ns)
    }
}

/// Матрица-вектор (1×N либо N×1) → список чисел (Re-часть).
/// Синтаксис языка: [0.8, 0.2] — это строка-матрица.
pub fn matrix_as_vector(m: &Matrix) -> Option<Vec<f64>> {
    if m.rows == 1 {
        Some((0..m.cols).map(|j| m.get(0, j).re).collect())
    } else if m.cols == 1 {
        Some((0..m.rows).map(|i| m.get(i, 0).re).collect())
    } else {
        None
    }
}

/// Скаляр: окно CGLMP [2, 3.2] → радар Белла; положительное →
/// лог-шкала; иначе — одиночный бар.
fn auto_scalar(x: f64) -> Result<String, String> {
    if (2.0..3.2).contains(&x) {
        Ok(bell_svg(x))
    } else if x > 0.0 {
        scale_svg(x)
    } else {
        bars_svg(&[x])
    }
}

// =====================================================================
// Сессия-9: СУВЕРЕННЫЙ РЕНДЕР — графы и поля нативным ядром
// =====================================================================
// «От перестановки слагаемых сумма не меняется»: чистая математика
// рендеринга, вырезанная из стандартных библиотек, перенесена в
// монолитный движок без единой зависимости:
//   • Fruchterman–Reingold (Graphviz/NetworkX) → viz_graph;
//   • marching squares (Matplotlib contour)    → viz_field;
//   • поворот + ортоскопия + painter's-алгоритм
//     (Matplotlib plot_surface)                → viz_surf.
// Детерминизм: старт узлов — золотая спираль, итерации фиксированы,
// RNG нет вовсе — биты укладки воспроизводимы от запуска к запуску.

/// Точка плоскости (сеточные либо экранные координаты).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Pt {
    x: f64,
    y: f64,
}

/// Золотой угол (радианы): шаг спирали Фибоначчи (Vogel).
const GOLDEN_ANGLE: f64 = 2.399963229728653;

/// Итераций силовой укладки (фиксировано — детерминизм).
const FR_ITERS: usize = 300;

/// Линейная интерполяция hex-цвета: #rrggbb → #rrggbb.
fn lerp_hex(c0: &str, c1: &str, t: f64) -> String {
    let parse = |c: &str| -> [u8; 3] {
        let v = u32::from_str_radix(&c[1..], 16).unwrap_or(0);
        [(v >> 16) as u8, (v >> 8) as u8, v as u8]
    };
    let [r0, g0, b0] = parse(c0);
    let [r1, g1, b1] = parse(c1);
    let t = t.clamp(0.0, 1.0);
    format!(
        "#{:02x}{:02x}{:02x}",
        (r0 as f64 + (r1 as f64 - r0 as f64) * t).round() as u8,
        (g0 as f64 + (g1 as f64 - g0 as f64) * t).round() as u8,
        (b0 as f64 + (b1 as f64 - b0 as f64) * t).round() as u8
    )
}

// ---------------------------------------------------------------------
// viz_graph: силовая укладка Fruchterman–Reingold
// ---------------------------------------------------------------------

/// Укладка Fruchterman–Reingold: отталкивание всех пар k²/d,
/// притяжение рёбер d²/k (вес нормирован на максимум), смещение
/// ограничено температурой, остывающей линейно; рамка жёсткая.
/// Старт — золотая спираль: без RNG, бит-в-бит стабильно.
fn fruchterman_reingold(
    n: usize,
    edges: &[(usize, usize, f64)],
    x0: f64,
    y0: f64,
    w: f64,
    h: f64,
) -> Vec<Pt> {
    let k = (w * h / n.max(1) as f64).sqrt();
    let (cx, cy) = (x0 + w / 2.0, y0 + h / 2.0);
    let rmax = w.min(h) * 0.45;
    let mut pos: Vec<Pt> = (0..n)
        .map(|i| {
            let r = rmax * ((i as f64 + 0.5) / n as f64).sqrt();
            let a = i as f64 * GOLDEN_ANGLE;
            Pt {
                x: cx + r * a.cos(),
                y: cy + r * a.sin(),
            }
        })
        .collect();
    let wmax = edges
        .iter()
        .map(|e| e.2)
        .fold(0.0f64, f64::max)
        .max(1e-12);
    let t0 = w / 10.0;
    for it in 0..FR_ITERS {
        let temp = (t0 * (1.0 - it as f64 / FR_ITERS as f64)).max(0.01);
        let mut disp = vec![(0.0f64, 0.0f64); n];
        // Отталкивание всех пар.
        for i in 0..n {
            for j in (i + 1)..n {
                let (dx, dy) = (pos[i].x - pos[j].x, pos[i].y - pos[j].y);
                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                let f = k * k / d;
                let (ux, uy) = (dx / d, dy / d);
                disp[i].0 += ux * f;
                disp[i].1 += uy * f;
                disp[j].0 -= ux * f;
                disp[j].1 -= uy * f;
            }
        }
        // Притяжение рёбер (вес нормирован).
        for &(a, b, wgt) in edges {
            let (dx, dy) = (pos[b].x - pos[a].x, pos[b].y - pos[a].y);
            let d = (dx * dx + dy * dy).sqrt().max(0.01);
            let f = d * d / k * (wgt / wmax);
            let (ux, uy) = (dx / d, dy / d);
            disp[a].0 -= ux * f;
            disp[a].1 -= uy * f;
            disp[b].0 += ux * f;
            disp[b].1 += uy * f;
        }
        // Шаг с температурным потолком + жёсткая рамка.
        for i in 0..n {
            let (dx, dy) = disp[i];
            let d = (dx * dx + dy * dy).sqrt();
            if d > 1e-12 {
                let step = d.min(temp);
                pos[i].x += dx / d * step;
                pos[i].y += dy / d * step;
            }
            pos[i].x = pos[i].x.clamp(x0, x0 + w);
            pos[i].y = pos[i].y.clamp(y0, y0 + h);
        }
    }
    pos
}

/// Компоненты связности: union-find со сжатием пути.
fn components(n: usize, edges: &[(usize, usize, f64)]) -> usize {
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut Vec<usize>, i: usize) -> usize {
        let mut r = i;
        while p[r] != r {
            r = p[r];
        }
        let mut c = i;
        while p[c] != r {
            let nx = p[c];
            p[c] = r;
            c = nx;
        }
        r
    }
    for &(a, b, _) in edges {
        let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
        if ra != rb {
            parent[ra] = rb;
        }
    }
    (0..n).filter(|&i| find(&mut parent, i) == i).count()
}

/// Силовая схема графа: квадратная матрица смежности (Re-часть,
/// |w| — вес ребра, неориентированная симметризация max(|a_ij|, |a_ji|),
/// петли не рисуются) → SVG. Рёбра — толщиной по весу, узлы —
/// радиусом и цветом по степени, вердикт — со статистикой графа.
pub fn graph_svg(m: &Matrix, labels: Option<&[String]>) -> Result<String, String> {
    if m.rows != m.cols {
        return Err(format!(
            "viz_graph: матрица смежности должна быть квадратной, получено {}×{}",
            m.rows, m.cols
        ));
    }
    if m.rows == 0 {
        return Err("viz_graph: пустая матрица смежности".into());
    }
    if m.rows > 64 {
        return Err(format!(
            "viz_graph: {} узлов — слишком крупно для сцены (до 64)",
            m.rows
        ));
    }
    let n = m.rows;
    for i in 0..n {
        for j in 0..n {
            if !m.get(i, j).re.is_finite() {
                return Err(format!("viz_graph: вес ({i},{j}) не конечен"));
            }
        }
    }
    let names: Vec<String> = match labels {
        Some(ls) => {
            if ls.len() != n {
                return Err(format!(
                    "viz_graph: {} меток для {} узлов — должно совпадать",
                    ls.len(),
                    n
                ));
            }
            ls.to_vec()
        }
        None => (0..n).map(|k| format!("v{k}")).collect(),
    };

    // Рёбра: симметризация, петли считаются, но не рисуются.
    let mut edges: Vec<(usize, usize, f64)> = Vec::new();
    let mut loops = 0usize;
    for i in 0..n {
        if m.get(i, i).re.abs() > 1e-9 {
            loops += 1;
        }
    }
    for i in 0..n {
        for j in (i + 1)..n {
            let w = m.get(i, j).re.abs().max(m.get(j, i).re.abs());
            if w > 1e-9 {
                edges.push((i, j, w));
            }
        }
    }

    let (fx, fy, fw, fh) = (76.0, 56.0, 488.0, 296.0);
    let mut pos = fruchterman_reingold(n, &edges, fx, fy, fw, fh);

    let mut deg = vec![0usize; n];
    for &(a, b, _) in &edges {
        deg[a] += 1;
        deg[b] += 1;
    }
    let dmax = deg.iter().cloned().max().unwrap_or(1).max(1);
    // Радиусы нужны до релаксации: узлы — твёрдые диски.
    let radii: Vec<f64> = (0..n)
        .map(|i| 7.0 + 6.0 * (deg[i] as f64 / dmax as f64).sqrt())
        .collect();
    // Релаксация перекрытий (грабля 20: в плотных графах FR-равновесие
    // допускает слипание узлов): разводим пары вдоль линии центров,
    // детерминированно, в рамке.
    for _ in 0..80 {
        let mut moved = false;
        for i in 0..n {
            for j in (i + 1)..n {
                let (dx, dy) = (pos[j].x - pos[i].x, pos[j].y - pos[i].y);
                let d = (dx * dx + dy * dy).sqrt();
                let min_d = radii[i] + radii[j] + 6.0;
                if d < min_d {
                    let (ux, uy) = if d > 1e-9 {
                        (dx / d, dy / d)
                    } else {
                        // Точное совпадение: разводим по золотому углу пары.
                        (GOLDEN_ANGLE.cos(), GOLDEN_ANGLE.sin())
                    };
                    let push = (min_d - d) / 2.0;
                    pos[i].x -= ux * push;
                    pos[i].y -= uy * push;
                    pos[j].x += ux * push;
                    pos[j].y += uy * push;
                    moved = true;
                }
            }
        }
        for p in pos.iter_mut() {
            p.x = p.x.clamp(fx, fx + fw);
            p.y = p.y.clamp(fy, fy + fh);
        }
        if !moved {
            break;
        }
    }
    let wmax = edges
        .iter()
        .map(|e| e.2)
        .fold(0.0f64, f64::max)
        .max(1e-12);

    let mut s = svg_open(640, 460);
    s.push_str(&title("Граф: силовая укладка Fruchterman–Reingold", 640));
    // Рёбра.
    for &(a, b, w) in &edges {
        let sw = 1.0 + 2.5 * (w / wmax);
        s.push_str(&format!(
            "<line x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\" \
             stroke=\"#64748b\" stroke-opacity=\"0.7\" stroke-width=\"{sw:.2}\"/>\n",
            pos[a].x, pos[a].y, pos[b].x, pos[b].y
        ));
    }
    // Узлы: радиус и цвет растут со степенью.
    for i in 0..n {
        let r = radii[i];
        let fill = lerp_hex(C_BLUE_BG, C_RED, deg[i] as f64 / dmax as f64);
        s.push_str(&format!(
            "<circle cx=\"{:.1}\" cy=\"{:.1}\" r=\"{r:.1}\" fill=\"{fill}\" \
             stroke=\"#1f2937\" stroke-width=\"1\"/>\n",
            pos[i].x, pos[i].y
        ));
        s.push_str(&format!(
            "<text x=\"{:.1}\" y=\"{:.1}\" font-size=\"10\" fill=\"{}\" \
             text-anchor=\"middle\">{}</text>\n",
            pos[i].x,
            pos[i].y + r + 13.0,
            C_TEXT,
            esc(&names[i])
        ));
    }
    // Вердикт: статистика графа.
    let comps = components(n, &edges);
    let mean_deg = 2.0 * edges.len() as f64 / n as f64;
    let density = if n > 1 {
        edges.len() as f64 * 2.0 / (n * (n - 1)) as f64 * 100.0
    } else {
        0.0
    };
    let mut line2 = format!(
        "укладка: {FR_ITERS} итераций FR · k = sqrt(area/N) · старт — золотая \
         спираль (детерминизм, без RNG)"
    );
    if loops > 0 {
        line2.push_str(&format!(" · петель {loops} (не рисуются)"));
    }
    s.push_str(&format!(
        "<text x=\"60\" y=\"396\" font-size=\"12\" fill=\"{}\">N = {} · рёбер = {} \
         · средняя степень = {:.2} · плотность = {:.1}% · компонент = {}</text>\n\
         <text x=\"60\" y=\"416\" font-size=\"11\" fill=\"{}\">{}</text>\n\
         <text x=\"60\" y=\"434\" font-size=\"10\" fill=\"{}\">узлы: радиус и цвет — \
         по степени ({} → {}); рёбра: толщина — по весу</text>\n</svg>",
        C_TEXT,
        n,
        edges.len(),
        mean_deg,
        density,
        comps,
        C_MUTED,
        esc(&line2),
        C_MUTED,
        C_BLUE_BG,
        C_RED
    ));
    Ok(s)
}

// ---------------------------------------------------------------------
// viz_field: изолинии marching squares
// ---------------------------------------------------------------------

/// Сегмент изолинии — два пересечения на рёбрах клетки.
type Seg = [Pt; 2];

/// Marching squares: изолиния уровня `level` по сетке узлов z[i][j]
/// (узел (i, j) лежит в точке (x = j, y = i)). Пересечение на ребре
/// строгое (знаки «выше уровня» должны РАЗЛИЧАТЬСЯ), поэтому узел
/// ровно на уровне не даёт двойных точек; двусмысленные диагонали
/// (углы «выше» через один) разрешаются средним по клетке.
fn marching_squares(z: &[Vec<f64>], level: f64) -> Vec<Seg> {
    let (rows, cols) = (z.len(), z[0].len());
    let cross = |p: Pt, vp: f64, q: Pt, vq: f64| -> Option<Pt> {
        if (vp > level) == (vq > level) {
            return None;
        }
        let t = (level - vp) / (vq - vp);
        Some(Pt {
            x: p.x + (q.x - p.x) * t,
            y: p.y + (q.y - p.y) * t,
        })
    };
    let mut out = Vec::new();
    for i in 0..rows - 1 {
        for j in 0..cols - 1 {
            // Углы клетки: a — верх-лево, b — верх-право,
            // c — низ-право, d — низ-лево.
            let pa = Pt {
                x: j as f64,
                y: i as f64,
            };
            let pb = Pt {
                x: j as f64 + 1.0,
                y: i as f64,
            };
            let pc = Pt {
                x: j as f64 + 1.0,
                y: i as f64 + 1.0,
            };
            let pd = Pt {
                x: j as f64,
                y: i as f64 + 1.0,
            };
            let (a, b, c, d) = (z[i][j], z[i][j + 1], z[i + 1][j + 1], z[i + 1][j]);
            let top = cross(pa, a, pb, b);
            let right = cross(pb, b, pc, c);
            let bottom = cross(pd, d, pc, c);
            let left = cross(pa, a, pd, d);
            let pts: Vec<Pt> = [top, right, bottom, left].into_iter().flatten().collect();
            match pts.len() {
                2 => out.push([pts[0], pts[1]]),
                4 => {
                    // Диагональная двусмысленность: среднее решает.
                    // diag_ac = «выше» углы a и c (иначе b и d);
                    // лента через центр → топ-право + низ-лево,
                    // островки → топ-лево + право-низ.
                    let center_above = (a + b + c + d) / 4.0 > level;
                    let diag_ac = a > level && c > level;
                    let (t_, r_, b_, l_) = (
                        top.unwrap(),
                        right.unwrap(),
                        bottom.unwrap(),
                        left.unwrap(),
                    );
                    if diag_ac == center_above {
                        out.push([t_, r_]);
                        out.push([b_, l_]);
                    } else {
                        out.push([t_, l_]);
                        out.push([r_, b_]);
                    }
                }
                _ => {}
            }
        }
    }
    // Вырожденные сегменты нулевой длины (узел ровно на уровне).
    out.retain(|s| (s[0].x - s[1].x).abs() > 1e-9 || (s[0].y - s[1].y).abs() > 1e-9);
    out
}

/// Сшивка сегментов в полилинии: концы квантуются в ключ 1/1024 узла
/// сетки; замкнутые кольца заканчиваются в стартовой точке.
fn chain_segments(segs: &[Seg]) -> Vec<Vec<Pt>> {
    let key = |p: &Pt| ((p.x * 1024.0).round() as i64, (p.y * 1024.0).round() as i64);
    let mut ends: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (k, seg) in segs.iter().enumerate() {
        ends.entry(key(&seg[0])).or_default().push(k);
        ends.entry(key(&seg[1])).or_default().push(k);
    }
    let mut used = vec![false; segs.len()];
    let mut lines: Vec<Vec<Pt>> = Vec::new();
    for start in 0..segs.len() {
        if used[start] {
            continue;
        }
        used[start] = true;
        let mut line = vec![segs[start][0], segs[start][1]];
        // Рост вперёд от хвоста.
        loop {
            let tip = *line.last().unwrap();
            let next = ends
                .get(&key(&tip))
                .and_then(|c| c.iter().cloned().find(|&k| !used[k]));
            match next {
                Some(k) => {
                    used[k] = true;
                    let other = if key(&segs[k][0]) == key(&tip) {
                        segs[k][1]
                    } else {
                        segs[k][0]
                    };
                    line.push(other);
                }
                None => break,
            }
        }
        // Рост назад от головы.
        loop {
            let head = line[0];
            let next = ends
                .get(&key(&head))
                .and_then(|c| c.iter().cloned().find(|&k| !used[k]));
            match next {
                Some(k) => {
                    used[k] = true;
                    let other = if key(&segs[k][0]) == key(&head) {
                        segs[k][1]
                    } else {
                        segs[k][0]
                    };
                    line.insert(0, other);
                }
                None => break,
            }
        }
        lines.push(line);
    }
    lines
}

/// Скалярное поле: матрица узлов (Re) → изолинии marching squares
/// поверх дивергентной карты (красный ≥ центра, синий <).
/// 7 уровней строго внутри (zmin, zmax); плоское поле рисует фон без
/// изолиний. Гигиена сцены: сетка 2×2…64×64.
pub fn field_svg(m: &Matrix) -> Result<String, String> {
    if m.rows < 2 || m.cols < 2 {
        return Err(format!(
            "viz_field: нужно ≥ 2×2 узлов сетки, получено {}×{}",
            m.rows, m.cols
        ));
    }
    if m.rows > 64 || m.cols > 64 {
        return Err(format!(
            "viz_field: {}×{} — слишком крупно для сцены (до 64×64)",
            m.rows, m.cols
        ));
    }
    let z: Vec<Vec<f64>> = (0..m.rows)
        .map(|i| (0..m.cols).map(|j| m.get(i, j).re).collect())
        .collect();
    if z.iter().any(|r| r.iter().any(|v| !v.is_finite())) {
        return Err("viz_field: значения поля должны быть конечными".into());
    }
    let flat: Vec<f64> = z.iter().flat_map(|r| r.iter().cloned()).collect();
    let zmin = flat.iter().cloned().fold(f64::INFINITY, f64::min);
    let zmax = flat.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

    let cell = (540.0 / (m.rows - 1).max(m.cols - 1) as f64).clamp(7.0, 56.0);
    let (ox, oy) = (56.0, 40.0);
    let plot_w = (m.cols as f64 - 1.0) * cell;
    let plot_h = (m.rows as f64 - 1.0) * cell;
    let w = ox * 2.0 + plot_w;
    let h = oy + plot_h + 96.0;
    let mid = (zmin + zmax) / 2.0;
    let half = ((zmax - zmin) / 2.0).max(1e-12);

    let mut s = svg_open(w as u32, h as u32);
    s.push_str(&title("Поле: изолинии marching squares", w as u32));
    // Фон: дивергентная карта по среднему четырёх узлов клетки.
    for i in 0..m.rows - 1 {
        for j in 0..m.cols - 1 {
            let v = (z[i][j] + z[i][j + 1] + z[i + 1][j + 1] + z[i + 1][j]) / 4.0;
            let t = ((v - mid) / half).clamp(-1.0, 1.0);
            let base = if t >= 0.0 { "#ef4444" } else { "#3b82f6" };
            s.push_str(&format!(
                "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.2}\" height=\"{:.2}\" \
                 fill=\"{base}\" fill-opacity=\"{:.3}\" stroke=\"{}\" \
                 stroke-width=\"0.3\"/>\n",
                ox + j as f64 * cell,
                oy + i as f64 * cell,
                cell,
                cell,
                0.08 + 0.5 * t.abs(),
                C_GRID
            ));
        }
    }

    // Изолинии: 7 уровней строго внутри диапазона.
    let mut total_len = 0.0;
    let mut n_lines = 0usize;
    if zmax - zmin > 1e-12 {
        for (k, lev) in (1..=7)
            .map(|q| zmin + (zmax - zmin) * q as f64 / 8.0)
            .enumerate()
        {
            let segs = marching_squares(&z, lev);
            let lines = chain_segments(&segs);
            let color = lerp_hex("#2563eb", "#dc2626", k as f64 / 6.0);
            for line in &lines {
                let pts: Vec<String> = line
                    .iter()
                    .map(|p| format!("{:.1},{:.1}", ox + p.x * cell, oy + p.y * cell))
                    .collect();
                s.push_str(&format!(
                    "<polyline points=\"{}\" fill=\"none\" stroke=\"{color}\" \
                     stroke-width=\"1.6\" stroke-linejoin=\"round\"/>\n",
                    pts.join(" ")
                ));
                n_lines += 1;
            }
            for seg in &segs {
                total_len +=
                    ((seg[0].x - seg[1].x).powi(2) + (seg[0].y - seg[1].y).powi(2)).sqrt();
            }
        }
    }

    let verdict2 = if zmax - zmin > 1e-12 {
        format!(
            "изолинии: 7 уровней · полилиний {} · суммарная длина ≈ {:.1} ед. сетки \
             · marching squares (двусмысленные клетки — средним)",
            n_lines, total_len
        )
    } else {
        "плоское поле: zmin = zmax — изолиний нет".to_string()
    };
    s.push_str(&format!(
        "<text x=\"{ox:.0}\" y=\"{:.1}\" font-size=\"12\" fill=\"{}\">сетка {}×{} · \
         z ∈ [{}, {}]</text>\n\
         <text x=\"{ox:.0}\" y=\"{:.1}\" font-size=\"11\" fill=\"{}\">{}</text>\n</svg>",
        oy + plot_h + 26.0,
        C_TEXT,
        m.rows,
        m.cols,
        num(zmin),
        num(zmax),
        oy + plot_h + 46.0,
        C_MUTED,
        esc(&verdict2)
    ));
    Ok(s)
}

// ---------------------------------------------------------------------
// viz_surf: 3D-поверхность — поворот, ортоскопия, painter's-алгоритм
// ---------------------------------------------------------------------

/// Ортоскопическая проекция точки (x, y, z): поворот на азимут φ
/// вокруг вертикали, наклон камеры на высоту θ. Возвращает
/// (экран_x, экран_y, глубина) — математика plot_surface.
fn project3(
    x: f64,
    y: f64,
    z: f64,
    cf: f64,
    sf: f64,
    ct: f64,
    st: f64,
) -> (f64, f64, f64) {
    let x1 = x * cf - y * sf;
    let y1 = x * sf + y * cf;
    let sx = x1;
    let sy = z * ct - y1 * st;
    let depth = y1 * ct + z * st;
    (sx, sy, depth)
}

/// 3D-поверхность: матрица узлов — высоты z = f(x, y). Сетка
/// нормируется в куб [−1, 1]³, поворачивается на азимут −60° и
/// наклоняется на 28°, квады сортируются по глубине (painter's:
/// дальние рисуются раньше), цвет — по высоте (синий → красный).
/// Штатив осей — в ближне-нижнем углу. Гигиена: 2×2…48×48.
pub fn surf_svg(m: &Matrix) -> Result<String, String> {
    if m.rows < 2 || m.cols < 2 {
        return Err(format!(
            "viz_surf: нужно ≥ 2×2 узлов высот, получено {}×{}",
            m.rows, m.cols
        ));
    }
    if m.rows > 48 || m.cols > 48 {
        return Err(format!(
            "viz_surf: {}×{} — слишком крупно для сцены (до 48×48)",
            m.rows, m.cols
        ));
    }
    let mut zmin = f64::INFINITY;
    let mut zmax = f64::NEG_INFINITY;
    for i in 0..m.rows {
        for j in 0..m.cols {
            let v = m.get(i, j).re;
            if !v.is_finite() {
                return Err(format!("viz_surf: высота ({i},{j}) не конечна"));
            }
            zmin = zmin.min(v);
            zmax = zmax.max(v);
        }
    }
    let mid = (zmin + zmax) / 2.0;
    let half = ((zmax - zmin) / 2.0).max(1e-12);

    let nx = |j: usize| 2.0 * j as f64 / (m.cols - 1) as f64 - 1.0;
    let ny = |i: usize| 1.0 - 2.0 * i as f64 / (m.rows - 1) as f64; // строка 0 — «север»
    let nz = |v: f64| (v - mid) / half;

    const AZ: f64 = -60.0; // азимут камеры, градусы
    const EL: f64 = 28.0; // высота камеры, градусы
    let (phi, theta) = (AZ.to_radians(), EL.to_radians());
    let (cf, sf) = (phi.cos(), phi.sin());
    let (ct, st) = (theta.cos(), theta.sin());

    // Проекции узлов + штатив осей (входит в общую рамку).
    let mut pts: Vec<(f64, f64)> = Vec::with_capacity(m.rows * m.cols + 4);
    for i in 0..m.rows {
        for j in 0..m.cols {
            let (sx, sy, _) = project3(nx(j), ny(i), nz(m.get(i, j).re), cf, sf, ct, st);
            pts.push((sx, sy));
        }
    }
    // Штатив из углового базиса (как у Matplotlib: угол сетки).
    let (bx, by, bz) = (-1.05, -1.05, -1.05);
    let tripod = [
        (bx + 0.55, by, bz, "x"),
        (bx, by + 0.55, bz, "y"),
        (bx, by, bz + 0.55, "z"),
    ];
    let origin = project3(bx, by, bz, cf, sf, ct, st);
    for (tx, ty, tz, _) in tripod {
        let (sx, sy, _) = project3(tx, ty, tz, cf, sf, ct, st);
        pts.push((sx, sy));
    }

    // Рамка: вписать в канву с сохранением пропорций.
    let (mut x0, mut y0, mut x1, mut y1) = (
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    );
    for (sx, sy) in &pts {
        x0 = x0.min(*sx);
        x1 = x1.max(*sx);
        y0 = y0.min(*sy);
        y1 = y1.max(*sy);
    }
    let (cw, ch) = (552.0, 316.0);
    let scale = (cw / (x1 - x0).max(1e-9)).min(ch / (y1 - y0).max(1e-9));
    let (cxm, cym) = (320.0, 212.0);
    let to_screen = |sx: f64, sy: f64| -> (f64, f64) {
        (
            cxm + (sx - (x0 + x1) / 2.0) * scale,
            cym - (sy - (y0 + y1) / 2.0) * scale,
        )
    };

    let mut s = svg_open(640, 460);
    s.push_str(&title("Поверхность z = f(x, y): 3D-проекция", 640));

    // Квады по глубине (painter's: дальние раньше).
    let mut quads: Vec<(f64, [(f64, f64); 4], f64)> = Vec::new();
    for i in 0..m.rows - 1 {
        for j in 0..m.cols - 1 {
            let corners = [(i, j), (i, j + 1), (i + 1, j + 1), (i + 1, j)];
            let mut screen = [(0.0f64, 0.0f64); 4];
            let mut depth = 0.0;
            let mut zavg = 0.0;
            for (k, &(ci, cj)) in corners.iter().enumerate() {
                let v = m.get(ci, cj).re;
                let (sx, sy, d) = project3(nx(cj), ny(ci), nz(v), cf, sf, ct, st);
                screen[k] = to_screen(sx, sy);
                depth += d;
                zavg += nz(v);
            }
            quads.push((depth / 4.0, screen, zavg / 4.0));
        }
    }
    quads.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    for (_, screen, zt) in &quads {
        let fill = lerp_hex("#3b82f6", "#ef4444", (zt + 1.0) / 2.0);
        let pts_str: Vec<String> = screen
            .iter()
            .map(|(x, y)| format!("{x:.1},{y:.1}"))
            .collect();
        s.push_str(&format!(
            "<polygon points=\"{}\" fill=\"{fill}\" fill-opacity=\"1\" \
             stroke=\"#334155\" stroke-opacity=\"0.35\" stroke-width=\"0.4\"/>\n",
            pts_str.join(" ")
        ));
    }

    // Штатив осей поверх сцены.
    let (osx, osy) = to_screen(origin.0, origin.1);
    for (tx, ty, tz, label) in tripod {
        let (sx, sy, _) = project3(tx, ty, tz, cf, sf, ct, st);
        let (px, py) = to_screen(sx, sy);
        s.push_str(&format!(
            "<line x1=\"{osx:.1}\" y1=\"{osy:.1}\" x2=\"{px:.1}\" y2=\"{py:.1}\" \
             stroke=\"{}\" stroke-width=\"1.2\"/>\n\
             <text x=\"{px:.1}\" y=\"{:.1}\" font-size=\"11\" fill=\"{}\" \
             text-anchor=\"middle\">{label}</text>\n",
            C_MUTED,
            py - 6.0,
            C_MUTED
        ));
    }

    s.push_str(&format!(
        "<text x=\"60\" y=\"404\" font-size=\"12\" fill=\"{}\">сетка {}×{} · квадов {} \
         · z ∈ [{}, {}]</text>\n\
         <text x=\"60\" y=\"424\" font-size=\"11\" fill=\"{}\">камера: азимут {AZ}°, \
         высота {EL}° (ортоскопия) · painter's-алгоритм: дальние квады раньше</text>\n\
         <text x=\"60\" y=\"442\" font-size=\"10\" fill=\"{}\">цвет — высота: синий \
         (мин) → красный (макс) · поворот/проекция — как plot_surface, без \
         зависимостей</text>\n</svg>",
        C_TEXT,
        m.rows,
        m.cols,
        quads.len(),
        num(zmin),
        num(zmax),
        C_MUTED,
        C_MUTED
    ));
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::solve::Complex;

    fn starts_and_ends(s: &str) -> bool {
        s.starts_with("<svg") && s.ends_with("</svg>")
    }

    #[test]
    fn xml_escape_basics() {
        assert_eq!(esc("&<>\""), "&amp;&lt;&gt;&quot;");
        assert_eq!(esc("Ацин d=3"), "Ацин d=3");
    }

    #[test]
    fn num_formats_human_digits() {
        assert_eq!(num(0.0), "0");
        assert_eq!(num(2.0), "2");
        assert_eq!(num(1.7), "1.7");
        assert_eq!(num(1.616255e-35), "1.616e-35");
        assert_eq!(num(2.9148542161522637), "2.914854");
    }

    #[test]
    fn bell_svg_wellformed_and_anchored() {
        let s = bell_svg(BELL_ACIN);
        assert!(starts_and_ends(&s));
        assert!(s.contains("xmlns"));
        // Позиции вертикалей: x = 60 + (v-1.5)·550/1.7.
        let x_ts = 60.0 + (BELL_TSIRELSON - 1.5) * 550.0 / 1.7;
        let x_ac = 60.0 + (BELL_ACIN - 1.5) * 550.0 / 1.7;
        assert!(s.contains(&format!("{x_ts:.1}")));
        assert!(s.contains(&format!("{x_ac:.1}")));
        assert!(s.contains("Цирельсон"));
        assert!(s.contains("Ацин"));
    }

    #[test]
    fn bell_verdict_ranking_and_noise() {
        let (l1, l2) = bell_verdict(BELL_ACIN);
        assert!(l1.contains("над классикой"));
        assert!(l1.contains("над Цирельсоном"));
        assert!(l2.contains("0.6861")); // η* = 2/2.914854
        assert!(l2.contains("кутриты"));
        let (c1, _) = bell_verdict(1.87);
        assert!(c1.contains("внутри классического"));
    }

    #[test]
    fn bell_classical_zone() {
        let s = bell_svg(1.87);
        assert!(s.contains("классика ≤ 2.0"));
        assert!(s.contains("I = 1.87"));
    }

    #[test]
    fn scale_planck_anchor() {
        let s = scale_svg(1.616255e-35).unwrap();
        assert!(starts_and_ends(&s));
        assert!(s.contains("Планков"));
        assert!(s.contains("1.616e-35"));
        let (_, l2) = scale_verdict(1.616255e-35);
        assert!(l2.contains("меньше человека"));
        assert!(l2.contains("1.052e35")); // 1.7/1.616e-35
    }

    #[test]
    fn scale_human_is_reference() {
        let (_, l2) = scale_verdict(1.7);
        assert!(l2.contains("человеческий масштаб"));
        let s = scale_svg(1.7).unwrap();
        assert!(s.contains("1.7 м"));
    }

    #[test]
    fn scale_earth_between_anchors() {
        let (l1, l2) = scale_verdict(6.371e6);
        assert!(l1.contains("Эверест") && l1.contains("Земля"));
        assert!(l2.contains("больше человека"));
        // Отрицательные и нулевые значения отклоняются.
        assert!(scale_svg(-1.0).is_err());
        assert!(scale_svg(0.0).is_err());
    }

    #[test]
    fn prob_born_bars() {
        let s = prob_svg(&[0.8083, 0.0796, 0.1121]).unwrap();
        assert!(starts_and_ends(&s));
        assert!(s.contains("80.83%"));
        assert!(s.contains("Σ = 1"));
        assert!(s.contains("p0"));
        assert!(prob_svg(&[-0.5, 1.5]).is_err());
        assert!(prob_svg(&[]).is_err());
    }

    #[test]
    fn matrix_heatmap_identity() {
        let m = Matrix::identity(3);
        let s = matrix_svg(&m).unwrap();
        assert!(starts_and_ends(&s));
        assert!(s.contains("Матрица 3×3"));
        assert!(s.contains("Re(след) = 3"));
        assert!(s.contains("|max| = 1"));
        // Слишком крупная сцена отклоняется (гигиена размера).
        let big = Matrix::zeros(100, 100);
        assert!(matrix_svg(&big).is_err());
    }

    #[test]
    fn bars_spectrum() {
        let s = bars_svg(&[1.0, -2.0, 3.0]).unwrap();
        assert!(starts_and_ends(&s));
        assert!(s.contains("n = 3"));
        assert!(s.contains("min = -2"));
        assert!(s.contains("max = 3"));
        assert!(bars_svg(&[]).is_err());
    }

    #[test]
    fn auto_routes_by_type() {
        // Матрица → теплокарта (идентичность содержимого).
        let m = Matrix::identity(3);
        assert_eq!(auto_svg(&Value::Matrix(m.clone())).unwrap(), matrix_svg(&m).unwrap());
        // Список вероятностей → Born-бары.
        let probs = Value::List(vec![Value::Scalar(0.5), Value::Scalar(0.5)]);
        assert!(auto_svg(&probs).unwrap().contains("Σ = 1"));
        // Прочий список → спектр.
        let spec = Value::List(vec![Value::Scalar(1.0), Value::Scalar(-2.0)]);
        assert!(auto_svg(&spec).unwrap().contains("Спектр"));
        // Скаляр в окне CGLMP → радар Белла.
        assert!(auto_svg(&Value::Scalar(2.872934)).unwrap().contains("Цирельсон"));
        // Крошечный скаляр → шкала Вселенной.
        assert!(auto_svg(&Value::Scalar(1.616255e-35)).unwrap().contains("Планков"));
        // Комплексное → бары Re/Im.
        assert!(auto_svg(&Value::Complex(Complex::new(3.0, -4.0)))
            .unwrap()
            .contains("Re и Im"));
        // Строка отклоняется.
        assert!(auto_svg(&Value::Str("x".into())).is_err());
    }

    #[test]
    fn quantity_length_converted_to_meters() {
        use super::super::units;
        // 1 км = 1000 м → между «Эверест» и «Земля»? Нет: log10(1000)=3
        // — между «человек» и «Эверест».
        let km = units::by_name("km").expect("единица km есть");
        let s = auto_svg(&Value::Quantity(1.0, km)).unwrap();
        assert!(s.contains("1000 м"));
        let (l1, _) = scale_verdict(1000.0);
        assert!(l1.contains("человек") && l1.contains("Эверест"));
    }

    #[test]
    fn matrix_vector_routes_to_prob_or_bars() {
        // Грабля 16: [a, b, c] в языке — матрица-строка, не List.
        let probs = Matrix::from_rows(&[vec![0.8083, 0.0796, 0.1121]]).unwrap();
        let s = auto_svg(&Value::Matrix(probs)).unwrap();
        assert!(s.contains("Σ = 1"));
        let spec = Matrix::from_rows(&[vec![1.0, -2.0, 3.0]]).unwrap();
        assert!(auto_svg(&Value::Matrix(spec)).unwrap().contains("Спектр"));
        // Истинная 2×2-матрица остаётся теплокартой.
        let m22 = Matrix::from_rows(&[vec![1.0, 0.0], vec![0.0, -1.0]]).unwrap();
        assert!(auto_svg(&Value::Matrix(m22)).unwrap().contains("Матрица 2×2"));
    }

    // ===== Сессия-9: суверенный рендер (граф / поле / поверхность) =====

    #[test]
    fn graph_k4_deterministic_and_stats() {
        let m = Matrix::from_rows(&[
            vec![0.0, 1.0, 1.0, 1.0],
            vec![1.0, 0.0, 1.0, 1.0],
            vec![1.0, 1.0, 0.0, 1.0],
            vec![1.0, 1.0, 1.0, 0.0],
        ])
        .unwrap();
        let s1 = graph_svg(&m, None).unwrap();
        let s2 = graph_svg(&m, None).unwrap();
        assert_eq!(s1, s2, "укладка обязана быть бит-в-бит детерминированной");
        assert!(starts_and_ends(&s1));
        assert!(s1.contains("N = 4"));
        assert!(s1.contains("рёбер = 6"));
        assert!(s1.contains("компонент = 1"));
        assert!(s1.contains("плотность = 100"));
        assert!(s1.contains("Fruchterman"));
        assert!(s1.contains("v0") && s1.contains("v3"));
        // Все узлы внутри рамки укладки.
        assert!(s1.contains("золотая спираль"));
    }

    #[test]
    fn graph_two_islands_and_repulsion() {
        let m = Matrix::from_rows(&[
            vec![0.0, 1.0, 0.0, 0.0],
            vec![1.0, 0.0, 0.0, 0.0],
            vec![0.0, 0.0, 0.0, 1.0],
            vec![0.0, 0.0, 1.0, 0.0],
        ])
        .unwrap();
        let s = graph_svg(&m, None).unwrap();
        assert!(s.contains("компонент = 2"));
        assert!(s.contains("рёбер = 2"));
        // Изолированная пара: чистое отталкивание разводит узлы по рамке.
        let pos = fruchterman_reingold(2, &[], 76.0, 56.0, 488.0, 296.0);
        let d = ((pos[0].x - pos[1].x).powi(2) + (pos[0].y - pos[1].y).powi(2)).sqrt();
        assert!(d > 250.0, "изолированные узлы должны разойтись, d = {d}");
    }

    #[test]
    fn graph_labels_and_errors() {
        let m = Matrix::from_rows(&[vec![0.0, 2.0], vec![2.0, 0.0]]).unwrap();
        let s = graph_svg(&m, Some(&["альфа".to_string(), "бета".to_string()])).unwrap();
        assert!(s.contains("альфа") && s.contains("бета"));
        assert!(s.contains("рёбер = 1"));
        // Не квадрат, не та длина меток, слишком крупно, NaN — ошибки.
        let rect = Matrix::from_rows(&[vec![0.0, 1.0]]).unwrap();
        assert!(graph_svg(&rect, None).is_err());
        assert!(graph_svg(&m, Some(&["одна".to_string()])).is_err());
        assert!(graph_svg(&Matrix::zeros(65, 65), None).is_err());
        let nan_m = Matrix::from_rows(&[vec![f64::NAN, 0.0], vec![0.0, 0.0]]).unwrap();
        assert!(graph_svg(&nan_m, None).is_err());
        // Асимметричная смежность симметризуется: одно ребро, без ошибки.
        let dir = Matrix::from_rows(&[vec![0.0, 1.0], vec![0.0, 0.0]]).unwrap();
        assert!(graph_svg(&dir, None).unwrap().contains("рёбер = 1"));
        // Петли считаются, но не рисуются.
        let looped = Matrix::from_rows(&[vec![5.0, 1.0], vec![1.0, 0.0]]).unwrap();
        assert!(graph_svg(&looped, None).unwrap().contains("петель 1"));
    }

    #[test]
    fn marching_squares_plane_exact() {
        // z = x: узел (i, j) → z = j. Сетка 3×4, уровень 1.5:
        // ровно 2 вертикальных сегмента на x = 1.5.
        let z: Vec<Vec<f64>> = (0..3)
            .map(|_| (0..4).map(|j| j as f64).collect())
            .collect();
        let segs = marching_squares(&z, 1.5);
        assert_eq!(segs.len(), 2, "по сегменту на строку клеток");
        for seg in &segs {
            assert!((seg[0].x - 1.5).abs() < 1e-9);
            assert!((seg[1].x - 1.5).abs() < 1e-9);
            assert!(seg[0].y.fract().abs() < 1e-9 && seg[1].y.fract().abs() < 1e-9);
        }
        // Сшивка: одна полилиния x = 1.5, y = 0 → 2.
        let lines = chain_segments(&segs);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 3);
        let ys: Vec<f64> = lines[0].iter().map(|p| p.y).collect();
        assert_eq!(ys, vec![0.0, 1.0, 2.0]);
    }

    #[test]
    fn marching_squares_saddle_ambiguity() {
        // Диагонально двусмысленное поле: центр каждой клетки выше
        // уровня 0.5 → «лента» соединяет диагональ, контуры —
        // 4 ОТКРЫТЫЕ дуги вокруг граничных «нулевых» узлов.
        let z = vec![
            vec![1.0, 0.0, 1.0],
            vec![0.0, 2.0, 0.0],
            vec![1.0, 0.0, 1.0],
        ];
        let segs = marching_squares(&z, 0.5);
        assert_eq!(segs.len(), 8, "4 клетки × 2 сегмента (двусмысленность)");
        let lines = chain_segments(&segs);
        assert_eq!(lines.len(), 4, "по дуге на каждый «нулевой» узел");
        for line in &lines {
            assert_eq!(line.len(), 3);
            assert_ne!(line[0], *line.last().unwrap(), "дуги открытые");
        }
    }

    #[test]
    fn marching_squares_closed_ring_radial() {
        // Радиальное поле: уровень 1.2 вырезает ОДНО замкнутое кольцо
        // вокруг блока 2×2 внутренних узлов (0.707 < 1.2 < 1.581).
        let z: Vec<Vec<f64>> = (0..4)
            .map(|i| {
                (0..4)
                    .map(|j| {
                        ((i as f64 - 1.5).powi(2) + (j as f64 - 1.5).powi(2)).sqrt()
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let segs = marching_squares(&z, 1.2);
        assert_eq!(segs.len(), 8);
        let lines = chain_segments(&segs);
        assert_eq!(lines.len(), 1, "одно замкнутое кольцо");
        assert_eq!(lines[0].len(), 9); // 8 точек + замыкание
        assert_eq!(lines[0][0], *lines[0].last().unwrap());
    }

    #[test]
    fn field_svg_interference_scene() {
        // Двущелевая картина Born: cos²(πx/3)·exp(−y²/2), сетка 13×17.
        let mut rows = Vec::new();
        for i in 0..13 {
            let y = i as f64 / 12.0 * 2.0 - 1.0;
            rows.push(
                (0..17)
                    .map(|j| {
                        let x = j as f64 / 16.0 * 4.0 - 2.0;
                        (std::f64::consts::PI * x / 3.0).cos().powi(2)
                            * (-y * y / 2.0).exp()
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let m = Matrix::from_rows(&rows).unwrap();
        let s = field_svg(&m).unwrap();
        assert!(starts_and_ends(&s));
        assert!(s.contains("изолинии: 7 уровней"));
        assert!(s.contains("сетка 13×17"));
        assert!(s.matches("<polyline").count() > 0);
        // Ошибки гигиены: мелко, крупно, NaN.
        assert!(field_svg(&Matrix::from_rows(&[vec![1.0, 2.0]]).unwrap()).is_err());
        assert!(field_svg(&Matrix::zeros(65, 65)).is_err());
        let nan = Matrix::from_rows(&[vec![0.0, f64::NAN], vec![1.0, 2.0]]).unwrap();
        assert!(field_svg(&nan).is_err());
    }

    #[test]
    fn field_flat_degenerate() {
        let m = Matrix::from_rows(&[vec![1.0, 1.0], vec![1.0, 1.0]]).unwrap();
        let s = field_svg(&m).unwrap();
        assert!(starts_and_ends(&s));
        assert!(s.contains("плоское поле"));
        assert_eq!(s.matches("<polyline").count(), 0);
    }

    #[test]
    fn graph_dense_no_node_overlap() {
        // Грабля 20: почти полный граф (14 рёбер из 15) — FR-равновесие
        // без релаксации слипает узлы; после неё центры дальше
        // суммы радиусов.
        fn circles_of(s: &str) -> Vec<(f64, f64, f64)> {
            s.split("<circle")
                .skip(1)
                .map(|frag| {
                    let get = |key: &str| -> f64 {
                        let k = frag.find(key).unwrap() + key.len();
                        let rest = &frag[k..];
                        let end = rest.find('"').unwrap();
                        rest[..end].parse().unwrap()
                    };
                    (get("cx=\""), get("cy=\""), get("r=\""))
                })
                .collect()
        }
        let mut a = vec![vec![0.0; 6]; 6];
        for i in 0..6 {
            for j in 0..6 {
                // 14 рёбер из 15: нет связи 0↔5 в ОБЕ стороны
                // (симметризация иначе её восстановит).
                if i != j && !(i == 0 && j == 5) && !(i == 5 && j == 0) {
                    a[i][j] = ((i + 2) * (j + 3)) as f64;
                }
            }
        }
        let m = Matrix::from_rows(&a).unwrap();
        let s = graph_svg(&m, None).unwrap();
        assert!(s.contains("рёбер = 14"));
        let circles = circles_of(&s);
        assert_eq!(circles.len(), 6);
        for i in 0..circles.len() {
            for j in (i + 1)..circles.len() {
                let (dx, dy) = (circles[i].0 - circles[j].0, circles[i].1 - circles[j].1);
                let d = (dx * dx + dy * dy).sqrt();
                assert!(
                    d + 0.2 >= circles[i].2 + circles[j].2,
                    "узлы {i} и {j} слиплись: d = {d:.1} < {}",
                    circles[i].2 + circles[j].2
                );
            }
        }
    }

    #[test]
    fn surf_svg_gaussian_deterministic() {
        // Гауссов бугор exp(−3r²), сетка 11×13 → 120 квадов.
        let mut rows = Vec::new();
        for i in 0..11 {
            let y = i as f64 / 10.0 * 2.0 - 1.0;
            rows.push(
                (0..13)
                    .map(|j| {
                        let x = j as f64 / 12.0 * 2.0 - 1.0;
                        (-3.0 * (x * x + y * y)).exp()
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let m = Matrix::from_rows(&rows).unwrap();
        let s1 = surf_svg(&m).unwrap();
        let s2 = surf_svg(&m).unwrap();
        assert_eq!(s1, s2, "проекция детерминирована");
        assert!(starts_and_ends(&s1));
        assert_eq!(s1.matches("<polygon").count(), 10 * 12); // (R−1)·(C−1)
        assert!(s1.contains("квадов 120"));
        assert!(s1.contains("азимут -60"));
        assert!(s1.contains("painter"));
        assert!(s1.contains(">x<") && s1.contains(">y<") && s1.contains(">z<"));
        // Гигиена сцены.
        assert!(surf_svg(&Matrix::from_rows(&[vec![1.0, 2.0]]).unwrap()).is_err());
        assert!(surf_svg(&Matrix::zeros(49, 49)).is_err());
        let nan = Matrix::from_rows(&[vec![0.0, f64::NAN], vec![1.0, 2.0]]).unwrap();
        assert!(surf_svg(&nan).is_err());
    }
}
