# Реєстр інструментальних верифікаторів (MVR-v3)

Джерело істини для циклів верифікації. Протокол: `docs/MVR_PROTOCOL.md`.
Правило: жодна теорема тракту не отримує `AXIOM CONFIRMED` без рядка в цій
таблиці з виконаним скриптом і числами в томі.

| Цикл | Верифікатор | Теорема | Том | Інструменти | Останній запуск | Вердикт |
|---|---|---|---|---|---|---|
| B | `verify_vg8_masks.py` | II.1 (маски {−1,0,+1}) | II | Z3 5.1.0 (FP-теорія), NumPy 2.1.3 | 2026-09-16, commit 38a862a | CONFIRMED WITH CAVEATS (домен: скінченні x; кавет ±0.0) |
| D | `verify_rotor_norm.py` | I.1 (ротор J=U−Uᵀ) + властивості precess_step | I | SymPy 1.14.0, NumPy 2.1.3, rustc (тест gyro) | 2026-09-16, commit 38a862a | AXIOM CONFIRMED (ротор); CODE PROPERTIES CONFIRMED (lockstep; Σθ НЕ інваріант) |
| C | `verify_rabitq_arcsin.py` | III.1 (arcsin-MLE), III.2 (стиснення) | III | NumPy 2.1.3 (MC/FWHT/CRB) | 2026-09-16, commit 38a862a | AXIOM CONFIRMED (GW; MLE≈CRB; ADC; 144 Б) |
| A | `verify_pnd_gf.py` | V.1 (ARX Φ), V.2 (S-box x^254) | V | NumPy 2.1.3 (GF(2⁸), DDT/LAT), Z3 5.1.0 (BV) | 2026-09-16, commit 38a862a | V.1 AXIOM CONFIRMED ∀C; V.2 примітиви CONFIRMED (δ_S=4, NL=112); pndMix — PENDING poler-os |
| E | `verify_iir_z.py` | IV.1 (IIR ⟺ слід Вольтерри) | IV | SymPy 1.14.0 (rsolve/полюси), NumPy 2.1.3 | 2026-09-16, commit 38a862a | AXIOM CONFIRMED |

## Rust-тести, народжені верифікацією

| Тест | Модуль | Цикл | Що охороняє |
|---|---|---|---|
| `precess_step_edge_lockstep_preserves_pair_difference` | `crates/pqc/src/gyro.rs` | D | lockstep-інваріант ребра направленого транспорту + побітова відповідність формулі θ̇=−η·J·sin(Δθ) |

## Ключові знахідки (для історії — «документація зберігає провали нарівні з перемогами»)

1. **Цикл D, implementation-gap кейс:** перше прочитання `precess_step` (L640
   `+=`) було ПОМИЛКОВИМ — Python-верифікатор підтвердив моє читання, а не код.
   Rust-тест на реальному коді спростував (Σθ-дрейф −10.9 рад). Двоє знань:
   (а) інструментальна перевірка без код-грундингу — це перевірка власних
   фантазій; (б) у направленого Курамото-транспорту немає Σθ-інваріанта —
   і том I виправлений.
2. **Цикл B, семантика рівності:** Z3 `=` на FP-сорті — БІТОВЕ рівенство
   (+0 ≠ −0); IEEE-числове — `fp.eq`. Контрприклад x=−0.0 знайдено солвером,
   твердження переформульовано точно (fp.eq-семантика, домен скінченних).
3. **Цикл B, домен:** c=0 × {NaN, ±Inf} — поза теоремою (0·Inf=NaN≠+0.0) —
   SMT-контрприклад зафіксував необхідність обмеження домену.
4. **Том I, містифікація:** неіснуючий тест `test_rotor_energy_conservation`
   (0 збігів grep) видалений; реальні якоря: born.rs#L19 + новий тест gyro.

## Стан інструментів (LOADED/STANDBY)

LOADED: python3.12 + numpy 2.1.3 + sympy 1.14.0 + z3-solver 5.1.0; rustc/cargo
(rustc ≥1.87). STANDBY: cargo-asm (`cargo install cargo-asm`), galois
(`pip install galois`), criterion-бенчі (`cargo bench --workspace`).
НЕ ДОСТУПНО в цьому репо: poler-os/zig-kernel (окремий репозиторій).
