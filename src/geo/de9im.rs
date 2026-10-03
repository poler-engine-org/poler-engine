//! DE-9IM на тритах — топологическая матрица пересечений (Сессия-12).
//!
//! Порт якоря `geo.poler::geo-main/geo/src/algorithm/relate/mod.rs`
//! (georust/geo, `IntersectionMatrix`) на тритное ядро. Матрица OGC 9-IM:
//! строки — Interior / Boundary / Exterior геометрии A, столбцы — B.
//!
//! ## Тритная упаковка (теория Сессии-11)
//!
//! Классическая ячейка хранит РАЗМЕРНОСТЬ пересечения dim ∈ {∅, 0, 1, 2} —
//! четыре состояния, байт в JTS. Тритное ядро сжимает её до трёх:
//!
//! | трит | смысл                          | dim-эквивалент |
//! |------|--------------------------------|----------------|
//! |  −1  | пересечение пусто (Exterior)   | ∅              |
//! |   0  | касание в точке (Boundary)     | dim 0          |
//! |  +1  | внутреннее пересечение (Int.)  | dim ≥ 1        |
//!
//! 9 ячеек × 1 трит = **одно сбалансированно-троичное 9-тритное число**
//! (3⁹ = 19 683 состояния, укладывается в i16). «6-тритный блок + резерв
//! 3 трита»: полный 12-тритный трайт-блок несёт матрицу (9) + флаги (3).
//! Трит = кутрит: S_z|m⟩ = mℏ|m⟩ — ячейка матрицы буквально хранит
//! проекцию спина-1 топологии.
//!
//! ## Точность
//!
//! Все предикаты считаются на тритной решётке (`trit_coord`): знаки
//! ориентации, принадлежность ребру, пересечения рёбер — ЦЕЛОЧИСЛЕННО,
//! без epsilon. Касания «вершина-в-вершину» и коллинеарные перекрытия
//! различаются честно (0 против +1), что f64-геометрии даётся только
//! адаптивной арифметикой Шевчука.

use crate::calc::trits::{Trit, Trits};
use crate::geo::trit_coord::{
    cross_sign, on_segment, point_in_ring, seg_classify, SegX, TritPoint,
};

/// Матрица DE-9IM: 9 тритов.
/// Индексация `cells[r*3 + c]`, r ∈ {0: I(A), 1: B(A), 2: E(A)},
/// c ∈ {0: I(B), 1: B(B), 2: E(B)}.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct De9im {
    pub cells: [Trit; 9],
}

macro_rules! cell {
    ($s:expr, $r:expr, $c:expr) => {
        $s.cells[$r * 3 + $c]
    };
}

impl De9im {
    pub const EMPTY: De9im = De9im {
        cells: [-1; 9],
    };

    #[inline]
    pub fn ii(&self) -> Trit {
        cell!(self, 0, 0)
    } // I(A) ∩ I(B)
    #[inline]
    pub fn ib(&self) -> Trit {
        cell!(self, 0, 1)
    } // I(A) ∩ B(B)
    #[inline]
    pub fn ie(&self) -> Trit {
        cell!(self, 0, 2)
    } // I(A) ∩ E(B)
    #[inline]
    pub fn bi(&self) -> Trit {
        cell!(self, 1, 0)
    } // B(A) ∩ I(B)
    #[inline]
    pub fn bb(&self) -> Trit {
        cell!(self, 1, 1)
    } // B(A) ∩ B(B)
    #[inline]
    pub fn be(&self) -> Trit {
        cell!(self, 1, 2)
    } // B(A) ∩ E(B)
    #[inline]
    pub fn ei(&self) -> Trit {
        cell!(self, 2, 0)
    } // E(A) ∩ I(B)
    #[inline]
    pub fn eb(&self) -> Trit {
        cell!(self, 2, 1)
    } // E(A) ∩ B(B)
    #[inline]
    pub fn ee(&self) -> Trit {
        cell!(self, 2, 2)
    } // E(A) ∩ E(B)

    /// Сбалансированно-троичный 9-тритный код матрицы
    /// (старшая цифра — II). ±(3⁹−1)/2 = ±9841.
    pub fn trit_code(&self) -> i64 {
        let mut v: i64 = 0;
        for &c in &self.cells {
            v = v * 3 + c as i64;
        }
        v
    }

    /// Из тритного кода (обратное к `trit_code`).
    pub fn from_trit_code(code: i64) -> Result<De9im, String> {
        if code.abs() > 9841 {
            return Err(format!("de9im: код {code} вне диапазона 9 тритов (±9841)"));
        }
        let mut cells = [0i8; 9];
        let mut n = code;
        for i in (0..9).rev() {
            let r = n.rem_euclid(3);
            cells[i] = match r {
                0 => 0,
                1 => 1,
                _ => -1,
            };
            n = (n - cells[i] as i64) / 3;
        }
        Ok(De9im { cells })
    }

    /// Человекочитаемая 9-символьная строка «−0+…» (строки I,B,E × столбцы).
    pub fn bal_string(&self) -> String {
        self.cells
            .iter()
            .map(|&c| match c {
                -1 => '−',
                0 => '0',
                _ => '+',
            })
            .collect()
    }

    /// OGC-подобная строка шаблона (F/T/*) — как в JTS relate().
    pub fn pattern_string(&self) -> String {
        self.cells
            .iter()
            .map(|&c| if c == -1 { 'F' } else { 'T' })
            .collect()
    }

    // ── предикаты OGC (3-значные ячейки: −1 = ∅, 0/+1 = непусто) ──

    /// Пересекаются ли (любое непустое пересечение кроме E∩E).
    pub fn intersects(&self) -> bool {
        !(self.disjoint())
    }

    /// Не имеют ни одной общей точки (кроме внешности).
    pub fn disjoint(&self) -> bool {
        self.ii() == -1 && self.ib() == -1 && self.bi() == -1 && self.bb() == -1
    }

    /// Совпадают: внутренности пересекаются, ни одна внутренность
    /// не выходит во внешность другой.
    pub fn equals(&self) -> bool {
        self.ii() != -1 && self.ie() == -1 && self.ei() == -1
    }

    /// A содержит B: внутренности пересекаются, B не выходит за A
    /// (ни внутренностью, ни границей).
    pub fn contains(&self) -> bool {
        self.ii() != -1 && self.ei() == -1 && self.eb() == -1
    }

    /// A внутри B (транспонированный contains).
    pub fn within(&self) -> bool {
        self.ii() != -1 && self.ie() == -1 && self.be() == -1
    }

    /// A покрывает B: B целиком в замыкании A (граница может касаться).
    pub fn covers(&self) -> bool {
        self.ei() == -1
            && self.eb() == -1
            && (self.ii() != -1 || self.bi() != -1 || self.bb() != -1)
    }

    /// A покрыт B (транспонированный covers).
    pub fn covered_by(&self) -> bool {
        self.ie() == -1
            && self.be() == -1
            && (self.ii() != -1 || self.ib() != -1 || self.bb() != -1)
    }

    /// Касаются: внутренности не пересекаются, но границы/граница-внутренность
    /// соприкасаются.
    pub fn touches(&self) -> bool {
        self.ii() == -1 && (self.bb() != -1 || self.ib() == 0 || self.bi() == 0)
    }

    /// Перекрываются (равная размерность): внутренности пересекаются
    /// «площадно», обе выглядывают наружу друг друга.
    pub fn overlaps(&self) -> bool {
        self.ii() == 1 && self.ie() == 1 && self.ei() == 1
    }

    /// Пересекаются накрест (для смешанных размерностей — линия сквозь
    /// полигон): внутренностное пересечение точечное, обе выглядывают.
    pub fn crosses(&self) -> bool {
        self.ii() == 0 && self.ie() != -1 && self.ei() != -1
    }

    /// Имена всех сработавших предикатов (для отчёта).
    pub fn predicate_names(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if self.intersects() {
            v.push("intersects");
        }
        if self.disjoint() {
            v.push("disjoint");
        }
        if self.equals() {
            v.push("equals");
        }
        if self.contains() {
            v.push("contains");
        }
        if self.within() {
            v.push("within");
        }
        if self.covers() {
            v.push("covers");
        }
        if self.covered_by() {
            v.push("covered_by");
        }
        if self.touches() {
            v.push("touches");
        }
        if self.overlaps() {
            v.push("overlaps");
        }
        if self.crosses() {
            v.push("crosses");
        }
        v
    }
}

// ────────────── вычисление relate на тритной решётке ──────────────

/// Точка↔точка.
pub fn relate_point_point(a: &TritPoint, b: &TritPoint) -> Result<De9im, String> {
    let mut m = De9im::EMPTY;
    if a == b {
        m.cells[0] = 0; // II: точка (dim 0)
    } else {
        m.cells[2] = 0; // IE: {a} во внешности B
        m.cells[6] = 0; // EI
    }
    m.cells[8] = 1; // EE: внешности площадно пересекаются
    Ok(m)
}

/// Точка↔кольцо (полигон без дыр).
pub fn relate_point_ring(p: &TritPoint, ring: &[TritPoint]) -> Result<De9im, String> {
    if ring.len() < 3 {
        return Err("de9im: кольцо < 3 вершин".into());
    }
    let pos = point_in_ring(p, ring);
    let mut m = De9im::EMPTY;
    match pos {
        1 => m.cells[0] = 0,   // II: точка внутри (dim 0)
        0 => m.cells[1] = 0,   // IB: точка на границе
        _ => m.cells[2] = 0,   // IE: точка снаружи
    }
    // E(A) — всё кроме точки; внутренность/граница кольца заведомо
    // площадно/линейно выходят за пределы одной точки.
    m.cells[6] = 1; // EI: внутренность кольца минус точка — площадь
    m.cells[7] = 1; // EB: граница кольца минус возможная точка — линия
    m.cells[8] = 1; // EE
    Ok(m)
}

/// Кольцо↔кольцо — центральный кейс. Точные свидетели:
/// * вершины, середины рёбер и СМЕЩЁННЫЕ ВНУТРЬ узлы (offset-точки на
///   один квант от середины ребра к внутренности кольца) — строго
///   внутри/снаружи другого кольца (offset-свидетель закрывает случай
///   совпадающих/вложенных без «нырков» — идентичные квадраты); 
/// * классифицированные пересечения рёбер (Proper/Touch/Collinear).
pub fn relate_ring_ring(a: &[TritPoint], b: &[TritPoint]) -> Result<De9im, String> {
    if a.len() < 3 || b.len() < 3 {
        return Err("de9im: кольцо < 3 вершин".into());
    }
    if a[0].k != b[0].k {
        return Err("de9im: кольца на решётках разного масштаба".into());
    }

    // ── свидетели ──
    let mut a_vert_in = false; // вершина A строго внутри B
    let mut a_vert_out = false; // вершина A строго снаружи B
    let mut a_mid_in = false; // середина ребра A строго внутри B
    let mut a_mid_out = false;
    let mut a_off_in = false; // offset-узел A (квант внутрь) строго внутри B
    let mut a_off_out = false;
    let mut b_vert_in = false;
    let mut b_vert_out = false;
    let mut b_mid_in = false;
    let mut b_mid_out = false;
    let mut b_off_in = false;
    let mut b_off_out = false;
    let mut proper = false; // собственное пересечение рёбер
    let mut touch = false; // касание границ в точке
    let mut collinear = false; // коллинеарное перекрытие границ

    for p in a {
        match point_in_ring(p, b) {
            1 => a_vert_in = true,
            -1 => a_vert_out = true,
            _ => {}
        }
    }
    for p in b {
        match point_in_ring(p, a) {
            1 => b_vert_in = true,
            -1 => b_vert_out = true,
            _ => {}
        }
    }
    // Середины рёбер И offset-узлы: середина = (p+q)/2 (обе пробы
    // floor/ceil); offset = середина + один квант решётки к ВНУТРЕННОСТИ
    // кольца (по знаку обхода) — прямой свидетель I(кольца) у ребра.
    let orient_a = ring_orientation(a)?;
    let orient_b = ring_orientation(b)?;
    for ring_is_a in [true, false] {
        let (r, other) = if ring_is_a { (a, b) } else { (b, a) };
        let orient = if ring_is_a { orient_a } else { orient_b };
        let n = r.len();
        for i in 0..n {
            let p = &r[i];
            let q = &r[(i + 1) % n];
            let mx = (p.x.to_i64() + q.x.to_i64());
            let my = (p.y.to_i64() + q.y.to_i64());
            for (dx, dy) in [(0i64, 0i64), (1, 0), (0, 1), (1, 1)] {
                let cx = (mx + dx) / 2;
                let cy = (my + dy) / 2;
                let mid = TritPoint {
                    x: Trits::from_i64(cx)?,
                    y: Trits::from_i64(cy)?,
                    k: p.k,
                };
                match point_in_ring(&mid, other) {
                    1 => {
                        if ring_is_a {
                            a_mid_in = true
                        } else {
                            b_mid_in = true
                        }
                    }
                    -1 => {
                        if ring_is_a {
                            a_mid_out = true
                        } else {
                            b_mid_out = true
                        }
                    }
                    _ => {}
                }
                // offset-проба: середина + квант к внутренности (только
                // для точной середины — dx=dy даёт ceil-вариант, пропускаем).
                if dx == 0 && dy == 0 {
                    let ddx = q.x.to_i64() - p.x.to_i64();
                    let ddy = q.y.to_i64() - p.y.to_i64();
                    if (ddx, ddy) != (0, 0) {
                        // левая нормаль к ребру; при CW-обходе внутренность
                        // справа — меняем знак.
                        let (mut nx, mut ny) = (-ddy, ddx);
                        if orient == -1 {
                            (nx, ny) = (ddy, -ddx);
                        }
                        let sx = nx.signum();
                        let sy = ny.signum();
                        if (sx, sy) != (0, 0) {
                            let off = TritPoint {
                                x: Trits::from_i64(cx + sx)?,
                                y: Trits::from_i64(cy + sy)?,
                                k: p.k,
                            };
                            match point_in_ring(&off, other) {
                                1 => {
                                    if ring_is_a {
                                        a_off_in = true
                                    } else {
                                        b_off_in = true
                                    }
                                }
                                -1 => {
                                    if ring_is_a {
                                        a_off_out = true
                                    } else {
                                        b_off_out = true
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
    // Пересечения рёбер.
    let (na, nb) = (a.len(), b.len());
    for i in 0..na {
        for j in 0..nb {
            let class = seg_classify(
                &a[i],
                &a[(i + 1) % na],
                &b[j],
                &b[(j + 1) % nb],
            );
            match class {
                SegX::Proper => proper = true,
                SegX::Touch => touch = true,
                SegX::Collinear => collinear = true,
                SegX::None => {}
            }
        }
    }

    // ── сборка матрицы ──
    let mut m = De9im::EMPTY;

    // II: внутренности площадно пересекаются ⟺ «нырок», накрытие или
    // совпадение (offset-свидетель).
    if a_vert_in || b_vert_in || proper || a_mid_in || b_mid_in || a_off_in || b_off_in {
        m.cells[0] = 1;
    }

    // IB: граница B проходит по внутренности A (линейно).
    if b_vert_in || b_mid_in || proper {
        m.cells[1] = 1;
    }
    // BI: симметрично.
    if a_vert_in || a_mid_in || proper {
        m.cells[3] = 1;
    }

    // BB: коллинеарное перекрытие (+1), точечное касание ИЛИ собственное
    // пересечение рёбер (0 — точка на обеих границах).
    if collinear {
        m.cells[4] = 1;
    } else if touch || proper {
        m.cells[4] = 0;
    }

    // IE: часть внутренности A снаружи B (площадно).
    if a_vert_out || a_mid_out || a_off_out || proper || b_vert_in || b_mid_in {
        m.cells[2] = 1;
    }
    // EI: симметрично.
    if b_vert_out || b_mid_out || b_off_out || proper || a_vert_in || a_mid_in {
        m.cells[6] = 1;
    }

    // BE: часть границы A снаружи B (линейно).
    if a_vert_out || a_mid_out || proper {
        m.cells[5] = 1;
    }
    // EB: симметрично.
    if b_vert_out || b_mid_out || proper {
        m.cells[7] = 1;
    }

    // EE: внешности двух ограниченных колец всегда пересекаются площадно.
    m.cells[8] = 1;

    Ok(m)
}

/// Универсальный вход: relate двух геометрий (точка = 1 вершина,
/// отрезок = 2, кольцо ≥ 3; кольцо замыкаем автоматически).
pub fn relate(a: &[TritPoint], b: &[TritPoint]) -> Result<De9im, String> {
    let a = close_ring(a);
    let b = close_ring(b);
    match (a.len(), b.len()) {
        (1, 1) => relate_point_point(&a[0], &b[0]),
        (1, _) => relate_point_ring(&a[0], &b),
        (_, 1) => {
            let m = relate_point_ring(&b[0], &a)?;
            Ok(transpose(&m))
        }
        _ => relate_ring_ring(&a, &b),
    }
}

/// Транспонирование матрицы (A↔B).
pub fn transpose(m: &De9im) -> De9im {
    let mut t = De9im::EMPTY;
    for r in 0..3 {
        for c in 0..3 {
            t.cells[c * 3 + r] = m.cells[r * 3 + c];
        }
    }
    t
}

/// Знак обхода кольца: +1 — CCW (положительная площадь), −1 — CW,
/// 0 — вырожденное. Точно: удвоенная площадь в i64 с проверкой переполнения.
fn ring_orientation(ring: &[TritPoint]) -> Result<Trit, String> {
    let n = ring.len();
    let mut area2: i64 = 0;
    for i in 0..n {
        let p = &ring[i];
        let q = &ring[(i + 1) % n];
        let t = p
            .x
            .to_i64()
            .checked_mul(q.y.to_i64())
            .and_then(|v| {
                q.x.to_i64()
                    .checked_mul(p.y.to_i64())
                    .and_then(|w| v.checked_sub(w))
            })
            .ok_or_else(|| "de9im: переполнение площади кольца".to_string())?;
        area2 = area2
            .checked_add(t)
            .ok_or_else(|| "de9im: переполнение площади кольца".to_string())?;
    }
    Ok(match area2 {
        x if x > 0 => 1,
        x if x < 0 => -1,
        _ => 0,
    })
}

/// Замыкание кольца (первая вершина == последняя); точку/отрезок не трогаем.
fn close_ring(ring: &[TritPoint]) -> Vec<TritPoint> {
    if ring.len() >= 3 {
        let mut v = ring.to_vec();
        if v[0] != v[v.len() - 1] {
            v.push(v[0].clone());
        }
        v
    } else {
        ring.to_vec()
    }
}

// ────────────────────────── тесты ──────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const K: u8 = 6;

    fn tp(x: f64, y: f64) -> TritPoint {
        TritPoint::quantize(x, y, K).unwrap()
    }

    fn ring(pts: &[(f64, f64)]) -> Vec<TritPoint> {
        pts.iter().map(|&(x, y)| tp(x, y)).collect()
    }

    #[test]
    fn trit_code_roundtrip() {
        let mut m = De9im::EMPTY;
        m.cells[0] = 1;
        m.cells[4] = 0;
        m.cells[8] = -1;
        let code = m.trit_code();
        let back = De9im::from_trit_code(code).unwrap();
        assert_eq!(m, back);
        // диапазон
        assert!(De9im::from_trit_code(9842).is_err());
        // строка (символы, не байты — '−' трехбайтный)
        let mut n = De9im::EMPTY;
        n.cells[0] = -1;
        n.cells[1] = 0;
        n.cells[2] = 1;
        let s: String = n.bal_string().chars().take(3).collect();
        assert_eq!(s, "−0+");
    }

    #[test]
    fn disjoint_squares() {
        let a = ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let b = ring(&[(5.0, 5.0), (7.0, 5.0), (7.0, 7.0), (5.0, 7.0)]);
        let m = relate(&a, &b).unwrap();
        assert!(m.disjoint(), "матрица: {}", m.bal_string());
        assert!(!m.intersects());
        assert!(m.predicate_names().contains(&"disjoint"));
    }

    #[test]
    fn nested_squares_contains() {
        let big = ring(&[(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)]);
        let small = ring(&[(3.0, 3.0), (5.0, 3.0), (5.0, 5.0), (3.0, 5.0)]);
        let m = relate(&big, &small).unwrap();
        assert!(m.contains(), "матрица: {}", m.bal_string());
        assert!(relate(&small, &big).unwrap().within());
        assert!(!m.overlaps(), "вложение — не перекрытие");
    }

    #[test]
    fn identical_squares_equals() {
        let a = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let b = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let m = relate(&a, &b).unwrap();
        assert!(m.equals(), "матрица: {}", m.bal_string());
        assert_eq!(m.bb(), 1, "границы совпадают — коллинеарное перекрытие");
        assert!(m.contains() && m.within());
    }

    #[test]
    fn overlapping_squares() {
        // Классический учебный кейс: A=[0,2]², B=[1,3]².
        let a = ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let b = ring(&[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]);
        let m = relate(&a, &b).unwrap();
        assert!(m.overlaps(), "матрица: {}", m.bal_string());
        assert!(!m.contains() && !m.within());
        assert_eq!(m.ii(), 1);
        assert_eq!(m.bb(), 0, "границы пересекаются в двух точках");
    }

    #[test]
    fn touching_edge_squares_touches() {
        // Квадраты с общей стороной: внутренности не пересекаются,
        // границы перекрываются коллинеарно.
        let a = ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let b = ring(&[(2.0, 0.0), (4.0, 0.0), (4.0, 2.0), (2.0, 2.0)]);
        let m = relate(&a, &b).unwrap();
        assert!(m.touches(), "матрица: {}", m.bal_string());
        assert_eq!(m.bb(), 1, "общая сторона — линейное перекрытие границ");
        assert!(!m.intersects() || m.touches());
    }

    #[test]
    fn corner_touch_squares() {
        // Квадраты с общей вершиной: BB = 0 (точечное касание).
        let a = ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let b = ring(&[(2.0, 2.0), (4.0, 2.0), (4.0, 4.0), (2.0, 4.0)]);
        let m = relate(&a, &b).unwrap();
        assert!(m.touches(), "матрица: {}", m.bal_string());
        assert_eq!(m.bb(), 0, "касание вершинами — точка");
    }

    #[test]
    fn point_cases() {
        let sq = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        // внутри
        let m = relate(&[tp(2.0, 2.0)], &sq).unwrap();
        assert!(m.within());
        // на границе
        let m2 = relate(&[tp(2.0, 0.0)], &sq).unwrap();
        assert!(m2.covered_by() && !m2.within(), "на границе: покрыт, не внутри");
        // снаружи
        let m3 = relate(&[tp(9.0, 9.0)], &sq).unwrap();
        assert!(m3.disjoint());
        // точки между собой
        assert!(relate(&[tp(1.0, 1.0)], &[tp(1.0, 1.0)])
            .unwrap()
            .equals());
        assert!(relate(&[tp(1.0, 1.0)], &[tp(2.0, 2.0)])
            .unwrap()
            .disjoint());
    }

    #[test]
    fn transpose_symmetry() {
        let a = ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let b = ring(&[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]);
        let m = relate(&a, &b).unwrap();
        let t = relate(&b, &a).unwrap();
        assert_eq!(transpose(&m), t, "relate(b,a) = transpose(relate(a,b))");
    }

    #[test]
    fn pattern_string_ogc() {
        // Два далёких квадрата: F/F по взаимным внутренностям и границам,
        // но внутренности/границы лежат во внешности друг друга (T).
        let a = ring(&[(0.0, 0.0), (2.0, 0.0), (2.0, 2.0), (0.0, 2.0)]);
        let b = ring(&[(5.0, 5.0), (7.0, 5.0), (7.0, 7.0), (5.0, 7.0)]);
        let m = relate(&a, &b).unwrap();
        let s = m.pattern_string();
        let head: String = s.chars().take(4).collect();
        assert_eq!(head, "FFTF");
        // T только во внешностных ячейках: IE, BE, EI, EB, EE.
        assert_eq!(s.chars().filter(|&c| c == 'T').count(), 5);
    }

    #[test]
    fn exact_boundary_no_epsilon() {
        // Вершина B ТОЧНО на ребре A — тритная решётка решает без epsilon.
        let a = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let b = ring(&[(2.0, 0.0), (3.0, 0.0), (3.0, 1.0), (2.0, 1.0)]);
        let m = relate(&a, &b).unwrap();
        // B примыкает к нижнему ребру A изнутри: A покрывает B,
        // границы коллинеарно перекрываются.
        assert!(m.covers(), "матрица: {}", m.bal_string());
        assert_eq!(m.bb(), 1, "коллинеарное прилегание рёбер");
    }

    #[test]
    fn point_on_edge_classified_exactly() {
        // Точка ровно на границе кольца — IB = 0, не «примерно внутри».
        let sq = ring(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let m = relate_point_ring(&tp(2.0, 0.0), &sq).unwrap();
        assert_eq!(m.ib(), 0);
        assert_eq!(m.ii(), -1);
        // Угловая точка
        let m2 = relate_point_ring(&tp(4.0, 4.0), &sq).unwrap();
        assert_eq!(m2.ib(), 0);
    }
}
