//! Rust-близнец математики P³ (Zig: `src/p3_kernel.zig`, `p3_idempotent.zig`).
//!
//! Это НЕ вызов Zig — это независимая реализация тех же формул на Rust.
//! Совпадение с Zig-стороны (см. `conformance.rs`) доказывает, что оба
//! движка вычисляют ОДНУ И ТУ ЖЕ математику проективного пространства.
//!
//! Конвенции (зеркально ядру P³):
//! - `Hom4` — однородный вектор [X:Y:Z:W], точка P³;
//! - `Pgl4` — column-major [16]f64, `data[col*4 + row]`;
//! - FS-метрика: d = arccos(|⟨a,b⟩|/(‖a‖·‖b‖)) ∈ [0, π/2].

/// Однородный 4-вектор [X:Y:Z:W] — точка проективного пространства P³.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hom4(pub [f64; 4]);

impl Hom4 {
    #[inline]
    pub fn new(x: f64, y: f64, z: f64, w: f64) -> Self {
        Hom4([x, y, z, w])
    }

    /// Скалярное произведение гильбертова пространства R⁴.
    #[inline]
    pub fn dot(a: &Hom4, b: &Hom4) -> f64 {
        a.0[0] * b.0[0] + a.0[1] * b.0[1] + a.0[2] * b.0[2] + a.0[3] * b.0[3]
    }

    /// Норма ‖v‖.
    #[inline]
    pub fn norm(&self) -> f64 {
        Self::dot(self, self).sqrt()
    }
}

/// 4×4 матрица PGL(4,ℝ), column-major: `data[col*4 + row]`.
#[derive(Clone, Copy, Debug)]
pub struct Pgl4 {
    pub data: [f64; 16],
}

impl Pgl4 {
    pub fn identity() -> Self {
        Pgl4 {
            data: [
                1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        }
    }

    #[inline]
    pub fn get(&self, row: usize, col: usize) -> f64 {
        self.data[col * 4 + row]
    }

    #[inline]
    pub fn set(&mut self, row: usize, col: usize, val: f64) {
        self.data[col * 4 + row] = val;
    }

    /// Из row-major [4][4] (конвертация в column-major).
    pub fn from_row_major(m: [[f64; 4]; 4]) -> Self {
        let mut r = Pgl4::identity();
        for col in 0..4 {
            for row in 0..4 {
                r.set(row, col, m[row][col]);
            }
        }
        r
    }

    /// Матричное умножение A·B (композиция PGL(4)).
    pub fn mul(&self, b: &Pgl4) -> Pgl4 {
        let mut r = Pgl4::identity();
        for col in 0..4 {
            for row in 0..4 {
                let mut sum = 0.0;
                for k in 0..4 {
                    sum += self.get(row, k) * b.get(k, col);
                }
                r.set(row, col, sum);
            }
        }
        r
    }

    pub fn transpose(&self) -> Pgl4 {
        let mut r = Pgl4::identity();
        for row in 0..4 {
            for col in 0..4 {
                r.set(row, col, self.get(col, row));
            }
        }
        r
    }

    /// Действие на точку P³: v → M·v.
    pub fn apply(&self, v: &Hom4) -> Hom4 {
        let mut out = [0.0f64; 4];
        for row in 0..4 {
            let mut sum = 0.0;
            for col in 0..4 {
                sum += self.get(row, col) * v.0[col];
            }
            out[row] = sum;
        }
        Hom4(out)
    }

    /// Определитель (разложение Лапласа по первой строке + Саррюс для 3×3)
    /// — зеркально `PGL4.det()` в Zig.
    pub fn det(&self) -> f64 {
        fn minor3(s: &[[f64; 3]; 3]) -> f64 {
            s[0][0] * (s[1][1] * s[2][2] - s[1][2] * s[2][1])
                - s[0][1] * (s[1][0] * s[2][2] - s[1][2] * s[2][0])
                + s[0][2] * (s[1][0] * s[2][1] - s[1][1] * s[2][0])
        }
        fn minor(m: &Pgl4, r: usize, c: usize) -> f64 {
            let mut s = [[0.0f64; 3]; 3];
            let mut si = 0;
            for i in 0..4 {
                if i == r {
                    continue;
                }
                let mut sj = 0;
                for j in 0..4 {
                    if j == c {
                        continue;
                    }
                    s[si][sj] = m.get(i, j);
                    sj += 1;
                }
                si += 1;
            }
            minor3(&s)
        }
        self.get(0, 0) * minor(self, 0, 0)
            - self.get(0, 1) * minor(self, 0, 1)
            + self.get(0, 2) * minor(self, 0, 2)
            - self.get(0, 3) * minor(self, 0, 3)
    }
}

/// Метрика Фубини–Штуди d_FS(a,b) = arccos(|⟨a,b⟩|/(‖a‖·‖b‖)) ∈ [0, π/2].
/// Зеркально `p3_kernel.fsDistance` (включая guard'ы и clamp).
pub fn fs_distance(a: &Hom4, b: &Hom4) -> f64 {
    let n1 = a.norm();
    let n2 = b.norm();
    if n1 < 1e-15 || n2 < 1e-15 {
        return 0.0;
    }
    let d = Hom4::dot(a, b).abs() / (n1 * n2);
    let cos_theta = d.min(1.0);
    cos_theta.acos()
}

/// Гивенс-поворот G(i,j,θ): ортогонален, det = +1. Зеркально `p3ffi_givens4`.
pub fn givens4(i: u32, j: u32, theta: f64) -> Pgl4 {
    let mut g = Pgl4::identity();
    if i == j || i > 3 || j > 3 {
        return g;
    }
    let (c, s) = (theta.cos(), theta.sin());
    let (ii, jj) = (i as usize, j as usize);
    g.set(ii, ii, c);
    g.set(jj, jj, c);
    g.set(ii, jj, -s);
    g.set(jj, ii, s);
    g
}

/// Спектральный проектор ранга 1: P = v·vᵀ/(vᵀv). Идемпотент P² = P.
pub fn spectral_projector(v: &Hom4) -> Pgl4 {
    let vv = Hom4::dot(v, v);
    let mut p = Pgl4::identity();
    if vv < 1e-300 {
        return p;
    }
    for row in 0..4 {
        for col in 0..4 {
            p.set(row, col, v.0[row] * v.0[col] / vv);
        }
    }
    p
}

/// Остаток идемпотентности max|P² − P| (по всем 16 элементам).
pub fn idempotent_residual(m: &Pgl4) -> f64 {
    let p2 = m.mul(m);
    let mut worst = 0.0f64;
    for i in 0..16 {
        worst = worst.max((p2.data[i] - m.data[i]).abs());
    }
    worst
}

/// Остаток ортогональности max|UᵀU − I|.
pub fn ortho_residual(m: &Pgl4) -> f64 {
    let utu = m.transpose().mul(m);
    let mut worst = 0.0f64;
    for row in 0..4 {
        for col in 0..4 {
            let want = if row == col { 1.0 } else { 0.0 };
            worst = worst.max((utu.get(row, col) - want).abs());
        }
    }
    worst
}

/// След (ранг идемпотента).
pub fn trace(m: &Pgl4) -> f64 {
    (0..4).map(|i| m.get(i, i)).sum()
}

// =============================================================================
// ТЕСТЫ (чистая математика, без FFI — работают на любой платформе)
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn fs_metric_bounds_and_special_cases() {
        // Ортогональные векторы → π/2
        let d = fs_distance(&Hom4::new(1.0, 0.0, 0.0, 0.0), &Hom4::new(0.0, 1.0, 0.0, 0.0));
        assert!((d - PI / 2.0).abs() < 1e-14, "d = {d}");

        // Антипараллельные — та же точка P³ → 0
        let d = fs_distance(&Hom4::new(1.0, 0.0, 0.0, 0.0), &Hom4::new(-2.0, 0.0, 0.0, 0.0));
        assert!(d.abs() < 1e-14);

        // Гомогенность: d_FS(λa, μb) = d_FS(a,b)
        let (a, b) = (
            Hom4::new(0.3, -1.2, 0.7, 1.0),
            Hom4::new(-0.8, 0.1, 0.5, 2.0),
        );
        let d0 = fs_distance(&a, &b);
        let a2 = Hom4([a.0[0] * 10.0, a.0[1] * 10.0, a.0[2] * 10.0, a.0[3] * 10.0]);
        let b2 = Hom4([-b.0[0] * 0.5, -b.0[1] * 0.5, -b.0[2] * 0.5, -b.0[3] * 0.5]);
        assert!((fs_distance(&a2, &b2) - d0).abs() < 1e-12);

        // Нулевой вектор → 0 (guard)
        assert_eq!(fs_distance(&Hom4::new(0.0, 0.0, 0.0, 0.0), &b), 0.0);

        // Диапазон [0, π/2] на псевдослучайных точках
        let mut s = 0x9E3779B97F4A7C15u64;
        for _ in 0..256 {
            let mut next = || {
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                ((s >> 11) as f64 / (1u64 << 53) as f64) * 2.0 - 1.0
            };
            let a = Hom4::new(next(), next(), next(), next());
            let b = Hom4::new(next(), next(), next(), next());
            let d = fs_distance(&a, &b);
            assert!((0.0..=PI / 2.0 + 1e-12).contains(&d), "d = {d}");
            assert!(d.is_finite());
        }
    }

    #[test]
    fn givens_orthogonality_and_det() {
        for &(i, j, th) in &[
            (0u32, 1u32, 0.3f64),
            (0, 3, 0.7),
            (1, 2, -1.1),
            (2, 3, 2.05),
        ] {
            let g = givens4(i, j, th);
            assert!(ortho_residual(&g) < 1e-14);
            assert!((g.det() - 1.0).abs() < 1e-12, "det({i},{j})");
        }
        // Вырожденный случай i == j → единичная
        let g = givens4(2, 2, 1.0);
        assert!(ortho_residual(&g) < 1e-15);
    }

    #[test]
    fn projector_idempotent_rank1() {
        let v = Hom4::new(1.0, 2.0, 3.0, 4.0);
        let p = spectral_projector(&v);
        assert!(idempotent_residual(&p) < 1e-14);
        assert!((trace(&p) - 1.0).abs() < 1e-14);
        // Симметричен
        for r in 0..4 {
            for c in 0..4 {
                assert!((p.get(r, c) - p.get(c, r)).abs() < 1e-15);
            }
        }
        // Дополнение I − P тоже идемпотент
        let mut q = Pgl4::identity();
        for k in 0..16 {
            q.data[k] -= p.data[k];
        }
        assert!(idempotent_residual(&q) < 1e-14);
        assert!((trace(&q) - 3.0).abs() < 1e-14);
    }

    #[test]
    fn pgl_mul_apply_consistency() {
        // (A·B)v = A(Bv)
        let a = givens4(0, 1, 0.4);
        let b = givens4(2, 3, -0.9);
        let v = Hom4::new(0.5, -0.3, 0.8, 1.0);
        let ab = a.mul(&b);
        let lhs = ab.apply(&v);
        let rhs = a.apply(&b.apply(&v));
        for k in 0..4 {
            assert!((lhs.0[k] - rhs.0[k]).abs() < 1e-14);
        }
        // det(AB) = det(A)·det(B)
        assert!((ab.det() - a.det() * b.det()).abs() < 1e-12);
    }
}

// =============================================================================
// НАТИВНЫЙ РАСТЕРИЗАТОР (цикл W, v0.59.0) — зеркало p3-engine/src/p3_ffi.zig
// =============================================================================
// Тот же алгоритм, что и в Zig-стороне C-ABI: классический целочисленный
// Брезенхем + глубина Фубини–Штуди, пересчитываемая из интерполированной
// 3D-точки + плюс-спрайты концов. Зачем два одинаковых растеризатора:
//   1. Rust-путь работает даже если libp3ffi.so не загрузилась
//      (чужая архитектура, нет файла) — `game demo` и `p3 render`
//      больше не падают SIGILL/ошибкой загрузки НИКОГДА;
//   2. конформанс-тест сверяет оба кремния на одной фикстуре —
//      «одна математика P³» доказана попиксельно.
// Детерминизм: чистая функция, без аллокаций в горячем цикле.

/// Параметры, идентичные C-ABI `p3ffi_render_frame`.
#[derive(Clone, Copy, Debug)]
pub struct NativeRenderCamera {
    pub focal: f64,
    pub cam_dist: f64,
    pub yaw: f64,
    pub pitch: f64,
}

pub const FRAC_PI_2_F64: f64 = std::f64::consts::FRAC_PI_2;

/// Палитра 15 оттенков (зеркало PALETTE в p3_ffi.zig; seg 0 — фон).
const PALETTE: [[f64; 3]; 15] = [
    [90.0, 140.0, 230.0],  // 1 — синий
    [230.0, 90.0, 110.0],  // 2 — красный
    [90.0, 200.0, 120.0],  // 3 — зелёный
    [235.0, 190.0, 80.0],  // 4 — золотой
    [120.0, 200.0, 230.0], // 5 — циан
    [210.0, 120.0, 230.0], // 6 — пурпур
    [240.0, 150.0, 70.0],  // 7 — оранж
    [80.0, 210.0, 190.0],  // 8 — бирюза
    [200.0, 220.0, 90.0],  // 9 — лайм
    [150.0, 160.0, 255.0], // 10 — лаванда
    [255.0, 120.0, 160.0], // 11 — розовый
    [130.0, 230.0, 160.0], // 12 — мятный
    [190.0, 140.0, 100.0], // 13 — песочный
    [170.0, 180.0, 200.0], // 14 — стальной
    [250.0, 220.0, 140.0], // 15 — шампань
];

#[inline]
fn palette_of(s: u8) -> [f64; 3] {
    if s == 0 {
        return [10.0, 12.0, 22.0];
    }
    PALETTE[((s - 1) as usize) % 15]
}

#[inline]
fn clamp_u8(v: f64) -> u8 {
    if v <= 0.0 {
        0
    } else if v >= 255.0 {
        255
    } else {
        v.round() as u8
    }
}

/// Глубина Фубини–Штуди от начала координат камеры: acos(1/‖(x,y,z,1)‖).
#[inline]
fn fs_depth(px: f64, py: f64, pz: f64) -> f64 {
    let q = 1.0 / (px * px + py * py + pz * pz + 1.0).sqrt();
    q.min(1.0).acos()
}

#[derive(Clone, Copy)]
struct Proj {
    vx: f64,
    vy: f64,
    vz: f64, // координаты в пространстве камеры (до сдвига cam_dist)
    ix: i64,
    iy: i64,
    ok: bool,
}

/// Проекция: yaw (вокруг Y) → pitch (вокруг X) → перспектива.
#[allow(clippy::too_many_arguments)]
fn project_point(
    x: f64,
    y: f64,
    z: f64,
    cy: f64,
    sy: f64,
    cp: f64,
    sp: f64,
    f: f64,
    cam_dist: f64,
    w2: f64,
    h2: f64,
) -> Proj {
    let x1 = cy * x + sy * z;
    let z1 = -sy * x + cy * z;
    let y2 = cp * y - sp * z1;
    let z2 = sp * y + cp * z1;
    let zc = z2 + cam_dist;
    if zc <= 1e-9 {
        return Proj { vx: x1, vy: y2, vz: z2, ix: 0, iy: 0, ok: false };
    }
    let u = f * x1 / zc + w2;
    let v = h2 - f * y2 / zc;
    if !u.is_finite() || !v.is_finite() || u.abs() > 9.0e15 || v.abs() > 9.0e15 {
        return Proj { vx: x1, vy: y2, vz: z2, ix: 0, iy: 0, ok: false };
    }
    Proj { vx: x1, vy: y2, vz: z2, ix: u.round() as i64, iy: v.round() as i64, ok: true }
}

struct Ctx<'a> {
    w: i64,
    h: i64,
    rgb: &'a mut [u8],
    depth: &'a mut [f32],
    seg: &'a mut [u8],
}

#[inline]
fn plot(
    ctx: &mut Ctx,
    x: i64,
    y: i64,
    d: f64,
    bias: f64,
    r: f64,
    g: f64,
    b: f64,
    s: u8,
) {
    if x < 0 || y < 0 || x >= ctx.w || y >= ctx.h {
        return;
    }
    let idx = (y * ctx.w + x) as usize;
    let dd = (d + bias) as f32;
    if dd < ctx.depth[idx] {
        ctx.depth[idx] = dd;
        ctx.seg[idx] = s;
        ctx.rgb[idx * 3] = clamp_u8(r);
        ctx.rgb[idx * 3 + 1] = clamp_u8(g);
        ctx.rgb[idx * 3 + 2] = clamp_u8(b);
    }
}

/// Плюс-спрайт радиуса R вокруг конечной точки (штраф глубины 1e-4·(m+1)).
fn draw_endpoint_sprite(
    ctx: &mut Ctx,
    p: Proj,
    col: [f64; 3],
    s: u8,
    radius: i64,
) {
    let d = fs_depth(p.vx, p.vy, p.vz);
    let bright = 0.55 + 0.45 * (1.0 - d / FRAC_PI_2_F64);
    let (r, g, b) = (col[0] * bright, col[1] * bright, col[2] * bright);
    let mut oy = -radius;
    while oy <= radius {
        let mut ox = -radius;
        while ox <= radius {
            let ax = if ox > 0 { ox } else { -ox };
            let ay = if oy > 0 { oy } else { -oy };
            let manh = ax + ay;
            if manh > radius {
                ox += 1;
                continue; // форма «плюс/ромб»
            }
            let bias = 1e-4 * (manh as f64 + 1.0);
            plot(ctx, p.ix + ox, p.iy + oy, d, bias, r, g, b, s);
            ox += 1;
        }
        oy += 1;
    }
}

/// Нативный рендер тройного буфера — зеркало `p3ffi_render_frame` (Zig).
/// Контракты буферов: `rgb.len() == 3·w·h`, `depth.len() == w·h`,
/// `seg.len() == w·h`, `pts.len() == n_pts·4`, `pairs` — пары индексов.
#[allow(clippy::too_many_arguments)]
pub fn render_frame_native(
    pts: &[f64],
    seg_ids: &[u8],
    pairs: &[u32],
    width: u32,
    height: u32,
    camera: NativeRenderCamera,
    thickness: u32,
    rgb: &mut [u8],
    depth: &mut [f32],
    seg: &mut [u8],
) {
    let (w, h) = (width as i64, height as i64);
    // --- 1. Фон ---
    for i in 0..(w * h) as usize {
        rgb[i * 3] = 10;
        rgb[i * 3 + 1] = 12;
        rgb[i * 3 + 2] = 22;
        depth[i] = 1e30;
        seg[i] = 0;
    }
    let n_pts = seg_ids.len();
    if n_pts == 0 || pairs.is_empty() {
        return;
    }

    let f = if camera.focal > 0.0 {
        camera.focal
    } else {
        1.15 * height as f64
    };
    let w2 = width as f64 / 2.0;
    let h2 = height as f64 / 2.0;
    let cy = camera.yaw.cos();
    let sy = camera.yaw.sin();
    let cp = camera.pitch.cos();
    let sp = camera.pitch.sin();
    let radius: i64 = std::cmp::max(1, thickness as i64 - 1);

    let mut ctx = Ctx { w, h, rgb, depth, seg };

    // --- 2. Рёбра ---
    for e in 0..pairs.len() / 2 {
        let ia = pairs[e * 2] as usize;
        let jb = pairs[e * 2 + 1] as usize;
        if ia >= n_pts || jb >= n_pts {
            continue;
        }
        let (s0, s1) = (seg_ids[ia], seg_ids[jb]);
        let pa = [pts[ia * 4], pts[ia * 4 + 1], pts[ia * 4 + 2]];
        let pb = [pts[jb * 4], pts[jb * 4 + 1], pts[jb * 4 + 2]];

        let a = project_point(pa[0], pa[1], pa[2], cy, sy, cp, sp, f, camera.cam_dist, w2, h2);
        let b = project_point(pb[0], pb[1], pb[2], cy, sy, cp, sp, f, camera.cam_dist, w2, h2);
        if !a.ok || !b.ok {
            continue;
        }

        let col0 = palette_of(s0);
        let col1 = palette_of(s1);
        let cseg = s0; // сегмент ребра — первая вершина (конвенция ABI)

        // --- Брезенхем ---
        let (mut x, mut y) = (a.ix, a.iy);
        let (x1, y1) = (b.ix, b.iy);
        let dx = x1 - x;
        let dy = y1 - y;
        let adx = if dx > 0 { dx } else { -dx };
        let ady = if dy > 0 { dy } else { -dy };
        let n: i64 = std::cmp::max(adx, ady);
        let sx: i64 = if dx > 0 { 1 } else { -1 };
        let sy2: i64 = if dy > 0 { 1 } else { -1 };
        let mut err: i64 = adx - ady;

        let mut k: i64 = 0;
        while k <= n {
            let t: f64 = if n > 0 { k as f64 / n as f64 } else { 1.0 };
            let px = a.vx + (b.vx - a.vx) * t;
            let py = a.vy + (b.vy - a.vy) * t;
            let pz = a.vz + (b.vz - a.vz) * t;
            let d = fs_depth(px, py, pz);
            let bright = 0.55 + 0.45 * (1.0 - d / FRAC_PI_2_F64);
            let r = (col0[0] + (col1[0] - col0[0]) * t) * bright;
            let g = (col0[1] + (col1[1] - col0[1]) * t) * bright;
            let b = (col0[2] + (col1[2] - col0[2]) * t) * bright;
            plot(&mut ctx, x, y, d, 0.0, r, g, b, cseg);

            // шаг Брезенхема
            if 2 * err > -ady {
                err -= ady;
                x += sx;
            }
            if 2 * err < adx {
                err += adx;
                y += sy2;
            }
            k += 1;
        }

        // --- Спрайты концов (толщина) ---
        draw_endpoint_sprite(&mut ctx, a, col0, cseg, radius);
        draw_endpoint_sprite(&mut ctx, b, col1, cseg, radius);
    }
}

#[cfg(test)]
mod render_native_tests {
    use super::*;

    /// Дым: квадрат — те же якоря, что у C-ABI-теста (зеркальность).
    #[test]
    fn native_render_square_smoke() {
        let pts: Vec<f64> = vec![
            -0.5, -0.5, 0.0, 1.0, 0.5, -0.5, 0.0, 1.0, 0.5, 0.5, 0.0, 1.0, -0.5, 0.5, 0.0, 1.0,
        ];
        let segs = [1u8, 2, 3, 4];
        let pairs = [0u32, 1, 1, 2, 2, 3, 3, 0];
        let (w, h) = (96u32, 72u32);
        let mut rgb = vec![0u8; (w * h * 3) as usize];
        let mut depth = vec![0f32; (w * h) as usize];
        let mut seg = vec![0u8; (w * h) as usize];
        render_frame_native(
            &pts, &segs, &pairs, w, h,
            NativeRenderCamera { focal: 0.0, cam_dist: 3.4, yaw: -0.62, pitch: 0.34 },
            2, &mut rgb, &mut depth, &mut seg,
        );
        let painted = seg.iter().filter(|&&s| s != 0).count();
        assert!(painted > 20, "painted = {painted}");
        assert_eq!(depth[0], 1e30);
        assert_eq!((rgb[0], rgb[1], rgb[2]), (10, 12, 22));
        let maxd = depth.iter().copied().filter(|&d| d < 1e29).fold(0.0f32, f32::max);
        assert!((0.0..=FRAC_PI_2_F64 as f32 + 1e-4).contains(&maxd));
    }

    /// Детерминизм: повторный вызов — бит-в-бит тот же тройной буфер.
    #[test]
    fn native_render_deterministic() {
        let pts: Vec<f64> = vec![
            -0.3, -0.4, 0.2, 1.0, 0.4, -0.2, -0.1, 1.0, 0.2, 0.5, 0.3, 1.0, -0.4, 0.3, -0.2, 1.0,
        ];
        let segs = [1u8, 3, 5, 9];
        let pairs = [0u32, 1, 1, 2, 2, 3, 3, 0, 0, 2];
        let (w, h) = (128u32, 96u32);
        let mut bufs = |tag: &str| {
            let mut rgb = vec![0u8; (w * h * 3) as usize];
            let mut depth = vec![0f32; (w * h) as usize];
            let mut seg = vec![0u8; (w * h) as usize];
            render_frame_native(
                &pts, &segs, &pairs, w, h,
                NativeRenderCamera { focal: 0.0, cam_dist: 3.0, yaw: -1.1, pitch: 0.5 },
                2, &mut rgb, &mut depth, &mut seg,
            );
            let _ = tag;
            (rgb, depth, seg)
        };
        let (rgb1, d1, s1) = bufs("a");
        let (rgb2, d2, s2) = bufs("b");
        assert_eq!(rgb1, rgb2);
        assert_eq!(d1, d2);
        assert_eq!(s1, s2);
    }
}
