#!/usr/bin/env python3
"""Верификация агрегированного CSR против исходного feather FlyWire v783.

V1. Заголовки/флаги, монотонность offsets, сортировка targets
V2. nodes.bin согласован (138 639; ⊆ proofread_root_ids, 616 изолированных)
V3. ПОЛНАЯ сохранность массы: для КАЖДОГО пре-нейрона Σ весов CSR-строки
    == Σ syn_count всех feather-строк с этим pre (покрытие 100%)
V4. Парная точность на 500 случайных уникальных парах: weight и nt_class
    пересчитываются из feather независимо (bincount по маске) и сверяются
V5. Ядро: все веса >= 5; масса full == core + слабые
V6. E/I-баланс (правдоподобие 80/20)
"""
import struct
import numpy as np
import pyarrow.feather as feather
import zstandard as zstd

RAW = "/home/z/my-project/poler-engine/docs/flywire-connectome/raw"
OUT = "/home/z/my-project/poler-engine/docs/flywire-connectome"
NT = ["gaba", "ach", "glut", "oct", "ser", "da"]
results = []

def ok(name, cond):
    results.append(bool(cond))
    print(f"  [{'OK' if cond else 'FAIL'}] {name}")

def load_csr(fname):
    blob = zstd.ZstdDecompressor().decompress(open(f"{OUT}/{fname}", "rb").read(), max_output_size=1 << 29)
    assert blob[:7] == b"FLYCSR1", "магия"
    core_flag = blob[7]
    n_nodes, n_edges = struct.unpack_from("<II", blob, 8)
    off = 16
    offsets = np.frombuffer(blob, dtype="<u4", count=n_nodes + 1, offset=off); off += (n_nodes + 1) * 4
    targets = np.frombuffer(blob, dtype="<u4", count=n_edges, offset=off); off += n_edges * 4
    weights = np.frombuffer(blob, dtype="<u2", count=n_edges, offset=off); off += n_edges * 2
    nt = np.frombuffer(blob, dtype="u1", count=n_edges, offset=off)
    return core_flag, n_nodes, n_edges, offsets, targets, weights, nt

print("V1: структура артефактов")
cf, nn, ne, offF, tF, wF, ntF = load_csr("flywire_v783_full.csr.zst")
ok(f"full: core_flag=0, n_nodes={nn}, n_edges={ne}", cf == 0)
ok("offsets монотонны, last==n_edges", bool(np.all(np.diff(offF) >= 0)) and offF[-1] == ne)
samp_rows = [range(offF[i], offF[i + 1]) for i in np.linspace(0, nn - 1, 400, dtype=int)]
ok("targets отсортированы внутри строк", all(np.all(np.diff(tF[r]) >= 0) for r in samp_rows if len(r) > 1))

print("V2: узлы")
# ВАЖНО: int64 (как в feather) — смешение u8/i8 продвигается в f64 и теряет
# точность на root_id ~7.2e17; читаем узлы как знаковые (значения положительны).
nodes = np.fromfile(f"{OUT}/flywire_v783_nodes.bin", dtype="<i8")
ids = np.load(f"{RAW}/proofread_root_ids_783.npy").astype(np.int64)
ok(f"nodes.bin == n_nodes ({len(nodes)})", len(nodes) == nn)
ok(f"nodes ⊆ proofread_ids; вне графа {len(ids) - len(np.intersect1d(nodes, ids))} изолированных",
   len(np.intersect1d(nodes, ids)) == len(nodes))

print("V3: полная сохранность массы по пре-нейронам (100% покрытие)")
t = feather.read_table(f"{RAW}/proofread_connections_783.feather", memory_map=True)
n = t.num_rows
pre = t.column("pre_pt_root_id").to_numpy(zero_copy_only=False)
syn = t.column("syn_count").to_numpy(zero_copy_only=False)
# ожидаемо: Σ syn по pre-нейрону
exp = np.zeros(nn, dtype=np.uint64)
pi_all = np.searchsorted(nodes, pre)
np.add.at(exp, pi_all, syn.astype(np.uint64))
# фактически: Σ весов CSR-строки
csr_sum = np.zeros(nn, dtype=np.uint64)
np.add.at(csr_sum, np.arange(nn), wF.astype(np.uint64)[offF[:-1]] if False else 0)
# (корректно: дифференциальные суммы через reduceat)
starts, ends = offF[:-1], offF[1:]
nz = ends > starts
csr_sum2 = np.zeros(nn, dtype=np.uint64)
csr_sum2[nz] = np.add.reduceat(wF.astype(np.uint64), starts[nz])
ok("Σ syn feather == Σ весов CSR для всех 138 639 пре-нейронов",
   bool(np.array_equal(exp, csr_sum2)))
ok(f"общая масса: {int(csr_sum2.sum())} синапсов (ожидалось 54 492 922)", int(csr_sum2.sum()) == 54_492_922)
del pre, syn, exp, csr_sum, csr_sum2

print("V4: парная точность (500 случайных уникальных пар, пересчёт из feather)")
rng = np.random.default_rng(783)
ei = rng.integers(0, ne, size=500)
rows = np.searchsorted(offF[1:], ei, side="right")
sel_keys = (rows.astype(np.uint64) << np.uint64(32)) | tF[ei].astype(np.uint64)
ukeys, inv_map = np.unique(sel_keys, return_inverse=True)  # sorted unique
# пересчёт из feather: маска строк с этими парами
pre_all = t.column("pre_pt_root_id").to_numpy(zero_copy_only=False)
post_all = t.column("post_pt_root_id").to_numpy(zero_copy_only=False)
kp = np.searchsorted(nodes, pre_all).astype(np.uint64)
kq = np.searchsorted(nodes, post_all).astype(np.uint64)
key_all = (kp << np.uint64(32)) | kq
del pre_all, post_all, kp, kq
mask = np.isin(key_all, ukeys)
sub_inv = np.searchsorted(ukeys, key_all[mask])
sub_syn = t.column("syn_count").to_numpy(zero_copy_only=False)[mask].astype(np.float64)
w_exp = np.bincount(sub_inv, weights=sub_syn, minlength=len(ukeys))
nt_exp = np.zeros((6, len(ukeys)))
for j, nm in enumerate(NT):
    sc = t.column(f"{nm}_avg").to_numpy(zero_copy_only=False)[mask].astype(np.float64) * sub_syn
    nt_exp[j] = np.bincount(sub_inv, weights=sc, minlength=len(ukeys))
cls_exp = np.argmax(nt_exp, axis=0)
w_got = wF[ei].astype(np.float64)
cls_got = ntF[ei]
ok(f"веса {len(ukeys)}/{len(ukeys)} уникальных пар совпали (Σ syn по нейропилям)",
   bool(np.allclose(w_exp[inv_map], w_got, atol=0.5)))
ok(f"nt_class {int((cls_exp[inv_map] == cls_got).sum())}/500 совпал",
   int((cls_exp[inv_map] == cls_got).sum()) >= 495)
del key_all, mask, sub_inv, sub_syn, nt_exp

print("V5: ядро")
cc, nnc, nec, offC, tC, wC, ntC = load_csr("flywire_v783_core.csr.zst")
ok(f"core: flag=1, n_nodes={nnc}, n_edges={nec}", cc == 1 and nnc == nn)
ok("все веса core >= 5", bool(np.all(wC >= 5)))
ok("масса: full == core + слабые",
   int(wF.astype(np.uint64).sum()) == int(wC.astype(np.uint64).sum()) + int(wF[wF < 5].astype(np.uint64).sum()))

print("V6: E/I-баланс")
w64 = wF.astype(np.uint64)
exc = int(w64[(ntF == 1) | (ntF == 2)].sum())
inh = int(w64[ntF == 0].sum())
mod = int(w64[(ntF == 3) | (ntF == 4) | (ntF == 5)].sum())
tot = int(w64.sum())
print(f"  возб: {exc/1e6:.1f}M ({100*exc/tot:.1f}%) | торм: {inh/1e6:.1f}M ({100*inh/tot:.1f}%) | модул: {mod/1e6:.1f}M ({100*mod/tot:.1f}%)")
ok("E:I в диапазоне 3–6 (≈80:20)", 3.0 < exc / inh < 6.0)

print()
print("ИТОГ:", "ВСЕ ПРОВЕРКИ ПРОЙДЕНЫ" if all(results) else "ЕСТЬ НЕУДАЧИ")
raise SystemExit(0 if all(results) else 1)
