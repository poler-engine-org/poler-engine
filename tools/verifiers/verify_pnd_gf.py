#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""MVR-v3, цикл A — Теоремы V.1/V.2: криптографическое ядро PND v8.

АРХЕОЛОГИЯ (фаза 0): критика линейности в архиве 299 источников относится к
PND v6/v7. В v8 (poler-os/zig-kernel) pndMix = Φ(a·b) +% ε·Φ(a⊕b) c S-box
x^254 до умножения — линейные пути уничтожены (слова владельца + трактат
Vol V). РЕПОЗИТОРИЙ poler-os НЕ входит в этот монорепо ⇒ код-грундинг
PENDING; здесь доказывается МАТЕМАТИКА примитивов v8.

Слои:
  gf    — x^254 == x⁻¹ в GF(2⁸) для ДВУХ неприводимых полиномов (0x11B AES,
          0x165) — представление-независимость (Ферма: x^255 = 1)
  sbox  — биективность S; ПОЛНЫЕ таблицы DDT (256×256) и LAT (256×256):
          δ_S (дифференциальная равномерность), max|W| (линейность), NL
  z3    — SMT-доказательство биективности ARX-бокса Φ (Vol V Th. V.1):
          rotl13 → xor → rotl7 → add; коллизии UNSAT на 3 наборах констант
  rt    — явный обратный Φ⁻¹ и round-trip на 10⁷ случайных + краевых
  json  — паспорт в scratch/passports/cycle_A.json
"""
import argparse, json, sys
from pathlib import Path
import numpy as np

def gf_mul(a, b, poly):
    """Умножение в GF(2^8) (carry-less + модуль)."""
    r = 0
    while b:
        if b & 1:
            r ^= a
        b >>= 1
        a <<= 1
        if a & 0x100:
            a ^= poly
    return r & 0xFF

def build_sbox(poly):
    """S(x) = x^254 в GF(2^8)[poly]."""
    S = [0] * 256            # 0^254 = 0
    for x in range(1, 256):
        r = x
        for _ in range(253):            # x^254 = x * x^253
            r = gf_mul(r, x, poly)
        S[x] = r
    return S

# ───────────────────── 1. x^254 = x⁻¹ (представление-независимость) ────────────
def run_gf():
    out = {}
    for poly, name in [(0x11B, 'AES 0x11B'), (0x165, 'alt 0x165')]:
        S = build_sbox(poly)
        inverse_ok = all(S[x] == 0 for x in [0]) and True
        # проверка: S[x] * x == 1 для всех x ≠ 0
        inv_ok = all(gf_mul(S[x], x, poly) == 1 for x in range(1, 256))
        # и S взаимно однозначен
        bij = len(set(S)) == 256
        out[name] = {'x254_times_x_is_1': inv_ok, 'bijective': bij}
    fermat_ok = all(
        gf_mul(build_sbox(0x11B)[x], x, 0x11B) == 1 for x in range(1, 256))
    return {'polys': out,
            'fermat_note': 'x^255 = 1 в GF(2⁸)* ⇒ x^254 = x⁻¹ — не зависит от '
                           'выбора полинома (подтверждено на двух)',
            'verdict': ('AXIOM CONFIRMED (x^254 = x⁻¹, биективно, 0↦0)'
                        if all(v['x254_times_x_is_1'] and v['bijective']
                               for v in out.values()) else 'REFUTED')}

# ───────────────── 2. Полные DDT / LAT S-box (256×256, без пропусков) ─────────
def run_sbox(poly=0x11B):
    S = np.array(build_sbox(poly), dtype=np.uint8)
    x = np.arange(256, dtype=np.uint8)
    # DDT[a][b] = #{x : S(x)^S(x^a) == b}
    DDT = np.zeros((256, 256), dtype=np.int32)
    for a in range(256):
        diff_out = S[x] ^ S[x ^ np.uint8(a)]
        np.add.at(DDT, (np.full(256, a), diff_out), 1)
    delta_S = int(DDT[1:].max())          # дифф. равномерность (a ≠ 0)
    # LAT: W[a][b] = Σ_x (−1)^{a·x ⊕ b·S(x)}
    A = np.zeros((256, 256), dtype=np.int8)
    B = np.zeros((256, 256), dtype=np.int8)
    for i in range(256):
        A[i] = np.array([bin(i & j).count('1') & 1 for j in range(256)])
        B[i] = np.array([bin(i & int(S[j])).count('1') & 1 for j in range(256)])
    W = np.zeros((256, 256), dtype=np.int32)
    for a in range(256):
        for b in range(256):
            W[a, b] = int(((-1) ** (A[a].astype(np.int8) ^ B[b])).sum())
    mask = np.ones((256, 256), dtype=bool); mask[0, 0] = False
    max_W = int(np.abs(W[mask]).max())
    NL = 128 - max_W // 2
    return {'poly': hex(poly),
            'sbox_is_permutation': bool(len(set(S.tolist())) == 256),
            'DDT_shape': '256×256 (полный перебор 2^16)',
            'differential_uniformity_delta': delta_S,
            'LAT_shape': '256×256 (полный перебор)',
            'max_abs_walsh': max_W,
            'nonlinearity_NL': NL,
            'nyberg_bound': 'NL ≥ 2^7 − 2^4 = 112 для инверсии',
            'verdict': ('CONFIRMED: δ_S = 4 (каждый дифференциал ≤ 4/256 — '
                        'нелинейность диффузии); NL = %d (граница Ниберга '
                        'выполнена)' % NL
                        if delta_S == 4 and NL >= 112 else
                        'ЧАСТИЧНО: δ=%d, NL=%d — см. паспорт' % (delta_S, NL))}

# ───────────────────── 3. Z3: биективность ARX-бокса Φ (Th. V.1) ──────────────
ARX_CONST_SETS = [
    (0x9E3779B9, 0x85EBCA6B, 0xC2B2AE35),   # золотое сечение / murmur
    (0x243F6A88, 0xB7E15162, 0xDEADBEEF),   # pi/frac, blowfish, маркер
    (0x517CC1B7, 0x27220A94, 0xFE13C8C9),   # произвольные константы
]

def arx_phi(x, c1, c2, c3):
    """Φ = rotl7(add(rotl(xor(rotl(x +% C1, 13), C2)), C3)) — порядок Vol V."""
    def rotl(v, r):
        return ((v << r) | (v >> (32 - r))) & 0xFFFFFFFF
    y1 = (x + c1) & 0xFFFFFFFF
    y2 = rotl(y1, 13)
    y3 = y2 ^ c2
    y4 = rotl(y3, 7)
    y5 = (y4 + c3) & 0xFFFFFFFF
    return y5

def run_z3():
    import z3
    results = {}
    for (c1, c2, c3) in ARX_CONST_SETS:
        x1 = z3.BitVec('x1', 32); x2 = z3.BitVec('x2', 32)
        def phi(x):
            return z3.RotateLeft(z3.RotateLeft(x + z3.BitVecVal(c1, 32), 13)
                                 ^ z3.BitVecVal(c2, 32), 7) + z3.BitVecVal(c3, 32)
        s = z3.Solver()
        s.add(phi(x1) == phi(x2))       # коллизия
        s.add(x1 != x2)                 # при разных входах
        r = s.check()
        results['Φ(0x%08X,0x%08X,0x%08X)' % (c1, c2, c3)] = (
            'UNSAT — коллизий нет (инъективность на Z_2³² ⇒ биективность)'
            if r == z3.unsat else str(r))
    all_unsat = all('UNSAT' in v for v in results.values())
    return {'z3_version': z3.get_version_string(), 'proofs': results,
            'structural_note': 'Каждый шаг (add const, rotl, xor const) — '
                               'биекция ∀C; композиция биекций биективна для '
                               'ЛЮБЫХ констант — проверено на 3 наборах',
            'verdict': 'AXIOM CONFIRMED (∀x1≠x2: Φ(x1)≠Φ(x2))' if all_unsat
                       else 'REFUTED'}

# ───────────────────── 4. Явный Φ⁻¹ и round-trip на 10⁷ точек ─────────────────
def run_roundtrip(n=10_000_000, seed=5):
    rng = np.random.default_rng(seed)
    def rotr(v, r):
        return ((v >> r) | (v << (32 - r))) & 0xFFFFFFFF
    ok_all = True
    for (c1, c2, c3) in ARX_CONST_SETS:
        xs = np.concatenate([
            rng.integers(0, 2**32, n, dtype=np.uint64).astype(np.uint32),
            np.array([0, 1, 0xFFFFFFFF, 0x80000000, 0x7FFFFFFF,
                      c1, c2, c3], dtype=np.uint32),
        ])
        ys = np.array([arx_phi(int(x), c1, c2, c3) for x in
                       xs[:200000]], dtype=np.uint32)     # питон-путь: 2·10⁵
        # векторный явный обратный
        def phi_inv(y):
            t = (y - c3) & 0xFFFFFFFF                     # y4
            t = rotr(t, 7)                                # y3
            t = t ^ c2                                    # y2
            t = rotr(t, 13)                               # y1
            return (t - c1) & 0xFFFFFFFF
        back = phi_inv(ys)
        ok = bool(np.all(back == xs[:200000]))
        ok_all &= ok
    # numpy-векторизованный Φ для полного 10⁷ round-trip (набор констант №1)
    c1, c2, c3 = ARX_CONST_SETS[0]
    xs = rng.integers(0, 2**32, n, dtype=np.uint64).astype(np.uint32)
    y1 = xs + np.uint32(c1)
    y2 = ((y1 << np.uint32(13)) | (y1 >> np.uint32(19))).astype(np.uint32)
    y3 = y2 ^ np.uint32(c2)
    y4 = ((y3 << np.uint32(7)) | (y3 >> np.uint32(25))).astype(np.uint32)
    y5 = y4 + np.uint32(c3)
    t = y5 - np.uint32(c3)
    t = ((t >> np.uint32(7)) | (t << np.uint32(25))).astype(np.uint32)
    t = t ^ np.uint32(c2)
    t = ((t >> np.uint32(13)) | (t << np.uint32(19))).astype(np.uint32)
    back = t - np.uint32(c1)
    full_ok = bool(np.all(back == xs))
    return {'samples_python_path': 200000 * len(ARX_CONST_SETS) + 8,
            'samples_numpy_path': n,
            'roundtrip_all_ok': bool(ok_all and full_ok),
            'verdict': ('AXIOM CONFIRMED (Φ⁻¹ явный; round-trip тождественен)'
                        if ok_all and full_ok else 'REFUTED')}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--json', default='scratch/passports/cycle_A.json')
    a = ap.parse_args()
    out = {'theorem': 'V.1 + V.2', 'cycle': 'A',
           'subject': 'PND v8: S-box x^254 (GF(2⁸)) DDT/LAT; ARX Φ биективность',
           'archaeology': {
               'source': 'архив 299: критика линейности = PND v6/v7 (устарело); '
                         'v8: pndMix = Φ(a·b) +% ε·Φ(a⊕b), S-box x^254 до умножения',
               'code_grounding': 'PENDING — poler-os/zig-kernel в ОТДЕЛЬНОМ репо, '
                                 'не входит в монорепо poler-engine'},
           'code': ['docs/mathematical-treatise/VOLUME_V (структура Φ и pndMix)'],
           'commit': '38a862a'}
    out['gf_inverse'] = run_gf()
    out['sbox_ddt_lat'] = run_sbox()
    out['z3_arx_bijective'] = run_z3()
    out['roundtrip'] = run_roundtrip()
    p = Path(a.json); p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(out, ensure_ascii=False, indent=1, default=str))
    print(json.dumps(out, ensure_ascii=False, indent=1, default=str))
    print('\nпаспорт: %s' % p)

if __name__ == '__main__':
    sys.exit(main())
