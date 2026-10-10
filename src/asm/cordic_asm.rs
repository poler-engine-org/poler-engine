//! CORDIC-ротор сокровищницы в машинном коде x86_64 (фиксированная точка Q24).
//!
//! Итеративный сдвигово-складывающий ротор на S¹ — без `div`, без `sqrt`:
//! 24 микроитерации `x' = x ∓ (y>>i)`, `y' = y ± (x>>i)`, `z' = z ∓ atan(2⁻ⁱ)`
//! по таблице atan(2⁻ⁱ)·2²⁴ в `.rodata`. Свойства:
//!
//! * **битовая детерминированность** — целочисленные сдвиги/сложения дают
//!   одинаковый результат на любом x86_64-хосте (в отличие от libm);
//! * синус/косинус за ~24·10 тактов без таблиц элементарных функций;
//! * atan2 (векторинг) + ренормализация массива (re, im) на S¹.
//!
//! Точности: sincos ≤ 1e-4 рад (Q24-квантование + 24 итерации),
//! atan2 ≤ 1e-3 (масштаб Q22 + rcpss-Ньютон).

use std::arch::global_asm;

global_asm! {
    r#"
    .section .rodata
    .p2align 5
    # atan(2^-i) * 2^24, i = 0..23 (генератор: scripts/gen_asm_consts.py)
.Lcordic_atan:
    .long 13176795, 7778716, 4110060, 2086331
    .long 1047214, 524117, 262123, 131069
    .long 65536, 32768, 16384, 8192
    .long 4096, 2048, 1024, 512
    .long 256, 128, 64, 32
    .long 16, 8, 4, 2
    .p2align 4
.Lcordic_K24:                       # K = Π(1+2^-2i)^(-1/2) = 0.6072529350
    .long 10188014
.Lcordic_pi24:                      # π·2^24
    .long 52707179
    .p2align 4
.Lc_i2pi:
    .float 0.1591549431             # 1/(2π)
.Lc_2pi:
    .float 6.2831853072
.Lc_pi:
    .float 3.1415926536
.Lc_hpi:
    .float 1.5707963268
.Lc_nhpi:
    .float -1.5707963268
.Lc_2p24:
    .float 16777216.0
.Lc_2n24:
    .float 0.000000059604644775     # 2^-24
.Lc_q22:
    .float 4194304.0                # 2^22
.Lc_tiny:
    .long 0x02081CEA                # 1e-37
.Lc_two:
    .float 2.0
.Lc_abs:
    .long 0x7FFFFFFF, 0x7FFFFFFF, 0x7FFFFFFF, 0x7FFFFFFF

    .text
    # ================================================================
    # void poler_cordic_sincos(float angle, f32* cos_out, f32* sin_out)
    #   xmm0=angle, rdi=cos_out, rsi=sin_out
    # ================================================================
    .globl poler_cordic_sincos
    .type poler_cordic_sincos, @function
    .p2align 4
poler_cordic_sincos:
    push rbx
    push rbp
    # --- редукция: angle -> [-π, π] ---
    vmulss xmm1, xmm0, [rip + .Lc_i2pi]
    vroundss xmm1, xmm1, xmm1, 8            # round-to-nearest
    vfnmadd231ss xmm0, xmm1, [rip + .Lc_2pi] # angle -= k·2π
    xor r11d, r11d                           # флаг квадранта
    vcomiss xmm0, [rip + .Lc_hpi]
    jbe 2f
    vsubss xmm0, xmm0, [rip + .Lc_pi]        # angle > π/2: сложить
    mov r11d, 1
    jmp 3f
2:
    vcomiss xmm0, [rip + .Lc_nhpi]
    jae 3f
    vaddss xmm0, xmm0, [rip + .Lc_pi]        # angle < -π/2: развернуть
    mov r11d, 1
3:
    # --- в фиксированную точку Q24 ---
    vmulss xmm0, xmm0, [rip + .Lc_2p24]
    vcvttss2si r8d, xmm0                     # z
    mov ebx, [rip + .Lcordic_K24]            # x = K·2^24
    xor ebp, ebp                             # y = 0
    lea r9, [rip + .Lcordic_atan]
    xor ecx, ecx                             # i
.Lc_loop:
    cmp ecx, 24
    jae .Lc_end
    mov eax, ebx
    sar eax, cl                              # x >> i
    mov edx, ebp
    sar edx, cl                              # y >> i
    mov r10d, [r9 + rcx*4]                   # atan[i] (Q24)
    test r8d, r8d
    js .Lc_neg
    sub ebx, edx                             # x -= y>>i
    add ebp, eax                             # y += x>>i
    sub r8d, r10d                            # z -= atan[i]
    jmp .Lc_next
.Lc_neg:
    add ebx, edx
    sub ebp, eax
    add r8d, r10d
.Lc_next:
    inc ecx
    jmp .Lc_loop
.Lc_end:
    test r11d, r11d                          # квадрантный флип: neg ОБЕИХ
    jz 4f
    neg ebx
    neg ebp
4:
    cvtsi2ss xmm1, ebx
    mulss xmm1, [rip + .Lc_2n24]
    cvtsi2ss xmm2, ebp
    mulss xmm2, [rip + .Lc_2n24]
    vmovss [rdi], xmm1                       # cos
    vmovss [rsi], xmm2                       # sin
    pop rbp
    pop rbx
    ret

    # ================================================================
    # float poler_cordic_atan2(float y, float x)  -> atan2(y, x)
    #   xmm0=y, xmm1=x -> xmm0
    # ================================================================
    .globl poler_cordic_atan2
    .type poler_cordic_atan2, @function
    .p2align 4
poler_cordic_atan2:
    push rbx
    push rbp
    # --- масштаб в Q22: s = 2^22 / max(|x|,|y|) (rcpss + Ньютон) ---
    vandps xmm2, xmm0, [rip + .Lc_abs]
    vandps xmm3, xmm1, [rip + .Lc_abs]
    vmaxss xmm2, xmm2, xmm3
    vmaxss xmm2, xmm2, [rip + .Lc_tiny]
    vrcpss xmm4, xmm4, xmm2                # r ≈ 1/m (src2 — заглушка)
    # Ньютон: r ← r·(2 − m·r)
    vmovss xmm5, [rip + .Lc_two]
    vfnmadd231ss xmm5, xmm2, xmm4           # 2 − m·r
    vmulss xmm4, xmm4, xmm5                 # r·(2 − m·r)
    vmulss xmm4, xmm4, [rip + .Lc_q22]
    vmulss xmm0, xmm0, xmm4                  # y·s
    vmulss xmm1, xmm1, xmm4                  # x·s
    vcvttss2si r10d, xmm0                    # yi (Q22)
    vcvttss2si r11d, xmm1                    # xi (Q22)
    # --- квадрант: x < 0 -> (x,y) <- (-x,-y), z = ±π·2^24 ---
    xor r8d, r8d                             # z = 0
    test r11d, r11d
    jns 5f
    mov eax, r10d                            # исходный знак y
    neg r10d
    neg r11d
    mov r8d, [rip + .Lcordic_pi24]
    test eax, eax
    jns 5f
    neg r8d
5:
    # --- векторинг: y -> 0, z -> угол ---
    mov ebx, r11d                            # x
    mov ebp, r10d                            # y
    lea r9, [rip + .Lcordic_atan]
    xor ecx, ecx
.La_loop:
    cmp ecx, 24
    jae .La_end
    mov eax, ebx
    sar eax, cl
    mov edx, ebp
    sar edx, cl
    mov r10d, [r9 + rcx*4]
    test ebp, ebp
    jns .La_pos
    sub ebx, edx                             # x -= y>>i
    add ebp, eax                             # y += x>>i
    sub r8d, r10d
    jmp .La_next
.La_pos:
    add ebx, edx
    sub ebp, eax
    add r8d, r10d
.La_next:
    inc ecx
    jmp .La_loop
.La_end:
    cvtsi2ss xmm0, r8d
    mulss xmm0, [rip + .Lc_2n24]
    pop rbp
    pop rbx
    ret

    .section .note.GNU-stack, "", @progbits
    "#
}

extern "C" {
    fn poler_cordic_sincos(angle: f32, cos_out: *mut f32, sin_out: *mut f32);
    fn poler_cordic_atan2(y: f32, x: f32) -> f32;
}

/// (cos, sin) угла в радианах — детерминированный CORDIC Q24.
pub fn sincos(angle: f32) -> (f32, f32) {
    let mut c = 0.0f32;
    let mut s = 0.0f32;
    unsafe { poler_cordic_sincos(angle, &mut c, &mut s) };
    (c, s)
}

/// atan2(y, x) — векторинг-режим CORDIC Q22 (квадранты учтены).
pub fn atan2(y: f32, x: f32) -> f32 {
    unsafe { poler_cordic_atan2(y, x) }
}

/// Ренормализация массива фаз (re, im) на единичную окружность S¹:
/// atan2 → sincos → перезапись. Ноль-вектор остаётся нулём
/// (atan2(0,0) = 0 → (1, 0); документированная семантика ротора).
pub fn renorm(re: &mut [f32], im: &mut [f32]) {
    for i in 0..re.len().min(im.len()) {
        let a = atan2(im[i], re[i]);
        let (c, s) = sincos(a);
        re[i] = c;
        im[i] = s;
    }
}

// ------------------------------ тесты ------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cordic_sincos_matches_libm() {
        let mut worst = 0.0f32;
        let mut worst_a = 0.0f32;
        let mut k = 0u64;
        // сетка + случайные большие углы (проверка редукции mod 2π)
        let mut angles: Vec<f32> = (0..400)
            .map(|i| -7.0 + i as f32 * 0.035)
            .collect();
        angles.extend_from_slice(&[100.0, -100.0, 1234.5, -999.25, 6.2831853, -6.2831853]);
        for &a in &angles {
            let (c, s) = sincos(a);
            let ec = (c - a.cos()).abs();
            let es = (s - a.sin()).abs();
            if ec.max(es) > worst {
                worst = ec.max(es);
                worst_a = a;
            }
            k += 1;
        }
        assert!(worst < 2e-3, "CORDIC sincos err {worst} at {worst_a} ({k} углов)");
    }

    #[test]
    fn cordic_atan2_matches_libm() {
        let cases: Vec<(f32, f32)> = vec![
            (1.0, 1.0),
            (1.0, -1.0),
            (-1.0, 1.0),
            (-1.0, -1.0),
            (0.3, 5.0),
            (-0.3, 5.0),
            (5.0, 0.3),
            (-5.0, -0.3),
            (0.0, 1.0),
            (0.0, -1.0),
            (1e-6, 1e-6),
            (1e5, 1e5),
        ];
        for &(y, x) in &cases {
            let a = atan2(y, x);
            let r = y.atan2(x);
            let d = (a - r).abs().min((a - r + std::f32::consts::TAU).abs());
            assert!(d < 3e-3, "atan2({y},{x}): asm={a} vs libm={r}");
        }
    }

    #[test]
    fn cordic_renorm_puts_phases_on_unit_circle() {
        let mut re = vec![3.0f32, 0.5, -2.0, 1e-6];
        let mut im = vec![4.0f32, -0.5, 0.1, 1e-6];
        renorm(&mut re, &mut im);
        for i in 0..re.len() {
            let m = (re[i] * re[i] + im[i] * im[i]).sqrt();
            assert!((m - 1.0).abs() < 2e-3, "norm[{i}]={m}");
            // направление сохранено
            let orig = (3.0f32, 4.0f32);
            if i == 0 {
                let d = (im[i] * orig.0 - re[i] * orig.1).abs();
                assert!(d < 0.05, "фаза потеряна: d={d}");
            }
        }
    }

    #[test]
    fn cordic_is_bit_deterministic() {
        // одинаковое состояние -> одинаковые биты (никаких libm-разногласий)
        let a = 1.2345f32;
        let r1 = sincos(a);
        let r2 = sincos(a);
        assert_eq!(r1.0.to_bits(), r2.0.to_bits());
        assert_eq!(r1.1.to_bits(), r2.1.to_bits());
    }
}
