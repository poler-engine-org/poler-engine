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

/// Native Rust-растеризатор сцены P³ (чистый Rust без AVX2/FFI требований).
pub fn render_frame_native(
    pts: &[f64],
    segs: &[u8],
    pairs: &[u32],
    width: u32,
    height: u32,
    camera: crate::p3::ffi::Camera,
    thickness: u32,
    rgb: &mut [u8],
    depth: &mut [f32],
    seg: &mut [u8],
) -> Result<(), String> {
    let npix = width as usize * height as usize;
    if rgb.len() != npix * 3 || depth.len() != npix || seg.len() != npix {
        return Err("некорректный размер буферов рендера".into());
    }

    // Фоновая заливка глубокого космоса [10, 12, 22]
    for px in rgb.chunks_exact_mut(3) {
        px[0] = 10;
        px[1] = 12;
        px[2] = 22;
    }
    depth.fill(1e30f32);
    seg.fill(0);

    const PALETTE: [[u8; 3]; 10] = [
        [10, 12, 22],   // 0: background
        [96, 202, 252], // 1: planet (cyan)
        [252, 128, 144],// 2: moon (rose)
        [148, 252, 128],// 3: green
        [252, 214, 96], // 4: star (gold)
        [186, 148, 252],// 5: probe (violet)
        [128, 252, 214],// 6: teal
        [252, 168, 96], // 7: orbit (orange)
        [172, 176, 190],// 8: silver
        [140, 180, 255],// 9: box (light blue)
    ];

    let n_pts = pts.len() / 4;
    let mut projected: Vec<(f64, f64, f32, bool)> = Vec::with_capacity(n_pts);

    let (sin_y, cos_y) = camera.yaw.sin_cos();
    let (sin_p, cos_p) = camera.pitch.sin_cos();
    let focal = if camera.focal > 0.0 {
        camera.focal
    } else {
        (width.min(height) as f64) * 1.15
    };
    let half_w = width as f64 * 0.5;
    let half_h = height as f64 * 0.5;

    for i in 0..n_pts {
        let x = pts[i * 4];
        let y = pts[i * 4 + 1];
        let z = pts[i * 4 + 2];

        // 1. Поворот вокруг оси Y (yaw)
        let rx = x * cos_y + z * sin_y;
        let rz = -x * sin_y + z * cos_y;
        let ry = y;

        // 2. Поворот вокруг оси X (pitch)
        let y2 = ry * cos_p - rz * sin_p;
        let z2 = ry * sin_p + rz * cos_p + camera.cam_dist;

        // Глубина d_FS
        let dist = (x * x + y * y + z * z).sqrt();
        let d_fs = ((dist / camera.cam_dist.max(0.1)).min(1.5707963)) as f32;

        if z2 > 0.05 {
            let px = half_w + (rx / z2) * focal;
            let py = half_h - (y2 / z2) * focal;
            projected.push((px, py, d_fs, true));
        } else {
            projected.push((0.0, 0.0, d_fs, false));
        }
    }

    let th = (thickness as i32).max(1);
    let n_pairs = pairs.len() / 2;

    for k in 0..n_pairs {
        let i0 = pairs[k * 2] as usize;
        let i1 = pairs[k * 2 + 1] as usize;
        if i0 >= n_pts || i1 >= n_pts {
            continue;
        }

        let (x0, y0, d0, v0) = projected[i0];
        let (x1, y1, d1, v1) = projected[i1];
        if !v0 || !v1 {
            continue;
        }

        let seg_id = if i0 < segs.len() { segs[i0] } else { 1 };
        let col = PALETTE[(seg_id as usize) % PALETTE.len()];

        // Bresenham line drawing
        let mut ix0 = x0.round() as i32;
        let mut iy0 = y0.round() as i32;
        let ix1 = x1.round() as i32;
        let iy1 = y1.round() as i32;

        let dx = (ix1 - ix0).abs();
        let dy = -(iy1 - iy0).abs();
        let sx = if ix0 < ix1 { 1 } else { -1 };
        let sy = if iy0 < iy1 { 1 } else { -1 };
        let mut err = dx + dy;

        let total_dist = ((ix1 - ix0).pow(2) + (iy1 - iy0).pow(2)) as f32;
        let start_x = ix0;
        let start_y = iy0;

        loop {
            // Расчёт интерполированной глубины
            let cur_dist = ((ix0 - start_x).pow(2) + (iy0 - start_y).pow(2)) as f32;
            let t = if total_dist > 0.0 { (cur_dist / total_dist).sqrt().min(1.0) } else { 0.0 };
            let d_fs = d0 * (1.0 - t) + d1 * t;

            // Рисование точки с толщиной thickness
            for oy in -th + 1..=th - 1 {
                for ox in -th + 1..=th - 1 {
                    let px = ix0 + ox;
                    let py = iy0 + oy;
                    if px >= 0 && px < width as i32 && py >= 0 && py < height as i32 {
                        let idx = py as usize * width as usize + px as usize;
                        if d_fs <= depth[idx] {
                            depth[idx] = d_fs;
                            seg[idx] = seg_id;
                            let rgb_idx = idx * 3;
                            rgb[rgb_idx] = col[0];
                            rgb[rgb_idx + 1] = col[1];
                            rgb[rgb_idx + 2] = col[2];
                        }
                    }
                }
            }

            if ix0 == ix1 && iy0 == iy1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                ix0 += sx;
            }
            if e2 <= dx {
                err += dx;
                iy0 += sy;
            }
        }
    }

    Ok(())
}
