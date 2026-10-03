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
        "<text x=\"{}\" y=\"22\" font-size=\"14\" font-weight=\"bold\" fill=\"{}\">{}</text>\n",
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
}
