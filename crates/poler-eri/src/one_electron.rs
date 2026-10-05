//! Exact Cartesian one-electron integrals (S, T, V) for arbitrary angular momentum.
//!
//! # AUDIT (v3.2.0-d)
//!
//! The original `IntegralEngine` filled ALL Cartesian components of a shell
//! pair with the s-type primitive value — a placeholder that is only correct
//! for l = 0 (H2 worked; LiH produced S(p,p) = 0.63 and identical p-rows).
//! This module computes the real per-component integrals:
//!
//! * **Overlap** — binomial expansion of (x−A)^a (x−B)^b around the Gaussian
//!   product center P, with exact 1D moments
//!   `M(k) = (k−1)!! √(π/p) / (2p)^{k/2}` (odd k → 0).
//! * **Kinetic** — the exact operator identity
//!   `∇²φ_b = [4β²|r−B|² − (6+4Σb_d)β] φ_b + Σ_d b_d(b_d−1) φ_{b−2e_d}`,
//!   which reduces T entirely to overlap integrals.
//! * **Nuclear** — binomial expansion + the Hermite derivative trick
//!   `(x−P) e^{−p(r−P)²} = (1/(2p)) ∂_P e^{−p(r−P)²}`, with the derivatives
//!   of the Boys seed Φ(P) = (2π/p) F₀(p|P−C|²) applied through the exact
//!   structural rule `∂_x [x^m F_j] = m x^{m−1} F_j − 2p x^{m+1} F_{j+1}`.
//!
//! All three reproduce the proven s-type seeds (`overlap_ss`, `kinetic_ss`,
//! `nuclear_ss`) exactly when a = b = (0,0,0).

use crate::boys::boys_f;
use crate::types::Point;
use std::f64::consts::PI;

/// Double factorial (k−1)!! as f64 (k odd or even alike; caller passes k).
fn dfact(k: usize) -> f64 {
    if k == 0 {
        return 1.0;
    }
    let mut r = 1.0f64;
    let mut m = k;
    while m > 0 {
        r *= m as f64;
        if m <= 2 {
            break;
        }
        m -= 2;
    }
    r
}

/// 1D moment of the centered Gaussian: M(k) = ∫ (x−P)^k e^{−p(x−P)²} dx.
fn moment(k: usize, p: f64) -> f64 {
    if k % 2 == 1 {
        return 0.0;
    }
    dfact(k.saturating_sub(1)) * libm::sqrt(PI / p) / libm::pow(2.0 * p, (k / 2) as f64)
}

/// Binomial coefficient C(n, k).
fn binom(n: usize, k: usize) -> f64 {
    if k > n {
        return 0.0;
    }
    let mut r = 1.0f64;
    for i in 0..k {
        r *= (n - i) as f64 / (i + 1) as f64;
    }
    r
}

/// Integer power of f64 (exact for the small exponents used here).
fn ipow(x: f64, n: usize) -> f64 {
    let mut r = 1.0;
    for _ in 0..n {
        r *= x;
    }
    r
}

/// Gaussian product center P = (αA + βB)/p.
fn product_center(ra: &Point, rb: &Point, alpha: f64, beta: f64) -> [f64; 3] {
    let p = alpha + beta;
    [
        (alpha * ra.0[0] + beta * rb.0[0]) / p,
        (alpha * ra.0[1] + beta * rb.0[1]) / p,
        (alpha * ra.0[2] + beta * rb.0[2]) / p,
    ]
}

/// Primitive Cartesian overlap (unnormalized):
/// `∫ (x−A)^{a0}(y−A)^{a1}(z−A)^{a2} · (x−B)^{b0}… e^{−α r_A² − β r_B²} d³r`.
pub fn overlap_cartesian(
    ra: &Point,
    rb: &Point,
    alpha: f64,
    beta: f64,
    a: [usize; 3],
    b: [usize; 3],
) -> f64 {
    let p = alpha + beta;
    let rab2 = ra.dist2(rb);
    let kab = libm::exp(-alpha * beta * rab2 / p);
    let pc = product_center(ra, rb, alpha, beta);

    let mut total = kab;
    for dim in 0..3 {
        let pa_d = pc[dim] - ra.0[dim];
        let pb_d = pc[dim] - rb.0[dim];
        let mut s = 0.0;
        for i in 0..=a[dim] {
            for j in 0..=b[dim] {
                s += binom(a[dim], i)
                    * binom(b[dim], j)
                    * ipow(pa_d, a[dim] - i)
                    * ipow(pb_d, b[dim] - j)
                    * moment(i + j, p);
            }
        }
        total *= s;
    }
    total
}

/// Primitive Cartesian kinetic energy:
/// `−½ ⟨a| ∇² |b⟩` via the exact operator identity (see module docs).
pub fn kinetic_cartesian(
    ra: &Point,
    rb: &Point,
    alpha: f64,
    beta: f64,
    a: [usize; 3],
    b: [usize; 3],
) -> f64 {
    let mut sum_plus = 0.0;
    for d in 0..3 {
        let mut b2 = b;
        b2[d] += 2;
        sum_plus += overlap_cartesian(ra, rb, alpha, beta, a, b2);
    }
    let s = overlap_cartesian(ra, rb, alpha, beta, a, b);
    let mut sum_minus = 0.0;
    for d in 0..3 {
        if b[d] >= 2 {
            let mut b2 = b;
            b2[d] -= 2;
            sum_minus += b[d] as f64 * (b[d] - 1) as f64
                * overlap_cartesian(ra, rb, alpha, beta, a, b2);
        }
    }
    let lsum = (b[0] + b[1] + b[2]) as f64;
    -0.5 * (4.0 * beta * beta * sum_plus - (6.0 + 4.0 * lsum) * beta * s + sum_minus)
}

/// One term of the symbolic derivative state: c · x^mx y^my z^mz F_j(T).
#[derive(Clone, Debug)]
struct BoysTerm {
    mx: usize,
    my: usize,
    mz: usize,
    j: usize,
    c: f64,
}

/// Evaluate ∂_x^{kx} ∂_y^{ky} ∂_z^{kz} [F₀(T)] at T = p|P−C|²,
/// with x ≡ Pₓ−Cₓ etc., via the exact structural rule
/// ∂_x [x^m F_j] = m·x^{m−1} F_j − 2p·x^{m+1} F_{j+1}.
fn boys_derivative(kx: usize, ky: usize, kz: usize, p: f64) -> Vec<BoysTerm> {
    let mut terms = vec![BoysTerm { mx: 0, my: 0, mz: 0, j: 0, c: 1.0 }];
    // apply ∂_x kx times
    for _ in 0..kx {
        let mut next = Vec::with_capacity(terms.len() * 2);
        for t in &terms {
            if t.mx > 0 {
                next.push(BoysTerm { mx: t.mx - 1, my: t.my, mz: t.mz, j: t.j, c: t.c * t.mx as f64 });
            }
            next.push(BoysTerm { mx: t.mx + 1, my: t.my, mz: t.mz, j: t.j + 1, c: t.c * (-2.0 * p) });
        }
        terms = next;
    }
    for _ in 0..ky {
        let mut next = Vec::with_capacity(terms.len() * 2);
        for t in &terms {
            if t.my > 0 {
                next.push(BoysTerm { mx: t.mx, my: t.my - 1, mz: t.mz, j: t.j, c: t.c * t.my as f64 });
            }
            next.push(BoysTerm { mx: t.mx, my: t.my + 1, mz: t.mz, j: t.j + 1, c: t.c * (-2.0 * p) });
        }
        terms = next;
    }
    for _ in 0..kz {
        let mut next = Vec::with_capacity(terms.len() * 2);
        for t in &terms {
            if t.mz > 0 {
                next.push(BoysTerm { mx: t.mx, my: t.my, mz: t.mz - 1, j: t.j, c: t.c * t.mz as f64 });
            }
            next.push(BoysTerm { mx: t.mx, my: t.my, mz: t.mz + 1, j: t.j + 1, c: t.c * (-2.0 * p) });
        }
        terms = next;
    }
    terms
}

/// Primitive Cartesian nuclear attraction:
/// `−Z_C ∫ (poly) e^{−α r_A² − β r_B²} / |r − C| d³r`.
pub fn nuclear_cartesian(
    ra: &Point,
    rb: &Point,
    rc: &Point,
    alpha: f64,
    beta: f64,
    zc: f64,
    a: [usize; 3],
    b: [usize; 3],
) -> f64 {
    let p = alpha + beta;
    let rab2 = ra.dist2(rb);
    let kab = libm::exp(-alpha * beta * rab2 / p);
    let pc = product_center(ra, rb, alpha, beta);
    let x = pc[0] - rc.0[0];
    let y = pc[1] - rc.0[1];
    let z = pc[2] - rc.0[2];
    let t_arg = p * (x * x + y * y + z * z);

    // Accumulate over the per-dimension binomial expansions.
    let mut total = 0.0f64;
    for i0 in 0..=a[0] {
        for j0 in 0..=b[0] {
            let w0 = binom(a[0], i0)
                * binom(b[0], j0)
                * ipow(pc[0] - ra.0[0], a[0] - i0)
                * ipow(pc[0] - rb.0[0], b[0] - j0);
            if w0 == 0.0 {
                continue;
            }
            for i1 in 0..=a[1] {
                for j1 in 0..=b[1] {
                    let w1 = binom(a[1], i1)
                        * binom(b[1], j1)
                        * ipow(pc[1] - ra.0[1], a[1] - i1)
                        * ipow(pc[1] - rb.0[1], b[1] - j1);
                    if w1 == 0.0 {
                        continue;
                    }
                    for i2 in 0..=a[2] {
                        for j2 in 0..=b[2] {
                            let w2 = binom(a[2], i2)
                                * binom(b[2], j2)
                                * ipow(pc[2] - ra.0[2], a[2] - i2)
                                * ipow(pc[2] - rb.0[2], b[2] - j2);
                            if w2 == 0.0 {
                                continue;
                            }
                            let kx = i0 + j0;
                            let ky = i1 + j1;
                            let kz = i2 + j2;
                            let k_all = kx + ky + kz;
                            let hermite_pref = ipow(1.0 / (2.0 * p), k_all);
                            let mut val = 0.0f64;
                            for t in &boys_derivative(kx, ky, kz, p) {
                                val += t.c
                                    * ipow(x, t.mx)
                                    * ipow(y, t.my)
                                    * ipow(z, t.mz)
                                    * boys_f(t.j, t_arg);
                            }
                            total += w0 * w1 * w2 * hermite_pref * val;
                        }
                    }
                }
            }
        }
    }

    -zc * kab * (2.0 * PI / p) * total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s_seed_consistency() {
        let ra = Point([0.0, 0.0, 0.0]);
        let rb = Point([0.5, 0.0, 0.0]);
        let alpha = 1.3;
        let beta = 0.9;
        let expected = crate::overlap::overlap_ss(alpha, beta, ra.dist2(&rb));
        let got = overlap_cartesian(&ra, &rb, alpha, beta, [0, 0, 0], [0, 0, 0]);
        assert!((got - expected).abs() < 1e-14, "s-seed overlap: {got} vs {expected}");
    }

    #[test]
    fn test_t_seed_consistency() {
        let ra = Point([0.0, 0.0, 0.0]);
        let rb = Point([0.5, 0.1, 0.0]);
        let alpha = 1.3;
        let beta = 0.9;
        let expected = crate::kinetic::kinetic_ss(alpha, beta, ra.dist2(&rb));
        let got = kinetic_cartesian(&ra, &rb, alpha, beta, [0, 0, 0], [0, 0, 0]);
        assert!((got - expected).abs() < 1e-13, "s-seed kinetic: {got} vs {expected}");
    }

    #[test]
    fn test_v_seed_consistency() {
        let ra = Point([0.0, 0.0, 0.0]);
        let rb = Point([0.5, 0.1, 0.2]);
        let rc = Point([1.0, 0.3, -0.4]);
        let alpha = 1.3;
        let beta = 0.9;
        let p = alpha + beta;
        let pc = product_center(&ra, &rb, alpha, beta);
        let dx = pc[0] - rc.0[0];
        let dy = pc[1] - rc.0[1];
        let dz = pc[2] - rc.0[2];
        let expected = crate::nuclear::nuclear_ss(
            alpha, beta, ra.dist2(&rb), dx * dx + dy * dy + dz * dz, 2.0,
        );
        let got = nuclear_cartesian(&ra, &rb, &rc, alpha, beta, 2.0, [0, 0, 0], [0, 0, 0]);
        assert!((got - expected).abs() < 1e-14, "s-seed nuclear: {got} vs {expected}");
    }

    #[test]
    fn test_p_self_overlap_symmetry() {
        // Normalized p-shell primitives on the same center:
        // <px|px> = 1-ish scale, <px|py> = 0 exactly.
        let r = Point([0.0, 0.0, 0.0]);
        let s_px_px = overlap_cartesian(&r, &r, 1.0, 1.0, [1, 0, 0], [1, 0, 0]);
        let s_px_py = overlap_cartesian(&r, &r, 1.0, 1.0, [1, 0, 0], [0, 1, 0]);
        // <px|px> = M(2)M(0)M(0) = sqrt(pi)/2 (for p=2)
        assert!(s_px_px > 0.0);
        assert!(s_px_py.abs() < 1e-15, "px/py must be orthogonal: {s_px_py}");
    }

    #[test]
    fn test_nuclear_p_perpendicular_vanishes() {
        // p_y on A, s on B along x-axis, nucleus C on the x-axis:
        // odd in y -> integral = 0.
        let ra = Point([0.0, 0.0, 0.0]);
        let rb = Point([2.0, 0.0, 0.0]);
        let rc = Point([1.0, 0.0, 0.0]);
        let v = nuclear_cartesian(&ra, &rb, &rc, 1.0, 1.0, 1.0, [0, 1, 0], [0, 0, 0]);
        assert!(v.abs() < 1e-14, "perpendicular p must vanish: {v}");
    }
}
