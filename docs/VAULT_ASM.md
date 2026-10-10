# VAULT-ASM — сокровищница в машинном коде (v0.87.0)

> Директива владельца: «реализуй всё абсолютно из сокровищницы на
> ассемблере внутри движка — так мы выведем работу с данными на
> непостижимый человеку и современному ИИ уровень».

Пять микроядер x86_64 на чистом ассемблере (`global_asm!`, Intel-синтаксис),
портированных из `docs/vault_drafts/` (DeepSeek Vault + POLER-Quantum):

| Ядро | Источник сокровищницы | Базис | Точка движка |
|------|----------------------|-------|--------------|
| `fep_asm` | `poler_quantum/archive/poler-core/src/fep_loss.rs` (принцип свободной энергии Фристона) | AVX2+FMA | `literary/engine.rs`: `free_energy()`, `grad_f()` |
| `lens_asm` | `archive/poler-lens/src/lens_index.rs` (No-Hits барьер, 99.2% сжатия) | SSE2 (базовый уровень!) | `engine.rs`: `build_nexus()` — LENS-фильтр K-hop рёбер |
| `synapse_asm` | vault блоки 0986/0984 (синапс-атомарная SSN, «1.7 Б/синапс») | AVX2+FMA+F16C | слой вывода SSN (CLI `--asm-bench`) |
| `cordic_asm` | P3/CORDIC S¹ (p3_poler.zig) | SSE2 + целочисленный Q24 | детерминированный ротор (renorm/atan2/sincos) |
| `stdp_asm` | LanguageCoreV2 (vault 0601: трёхфакторный STDP + WTA) | AVX2+FMA | PlasticityCompiler-совместимое правило |

## FEP-контур (Фристон)

```text
F(p, o)  = Σᵢ wᵢ·(pᵢ − Ω(o)ᵢ)² + 0.5·λ·Σᵢ pᵢ²
∇F(i)    = 2·wᵢ·(pᵢ − Ω(o)ᵢ) + λ·pᵢ
p_new    = p − η·∇F
```

* Три микроядра: `poler_fep_energy` (только F), `poler_fep_grad` (∇F),
  `poler_fep_step` (полный шаг).
* `gw = NULL` → единичная метрика G (`.rodata`-таблица 64×1.0f; при n > 64
  обёртка уходит в скалярный эталон — literary `dims = 64` покрыт).
* В литературном движке: `free_energy()` считает ‖p−o‖² микроядром,
  λ·‖J_c·p‖² остаётся в Rust (разреженный проектор); `grad_f()` получает
  ровно 2(p−o) контрактом (G=I, λ=0).

## LENS No-Hits барьер

```text
keep(i) ⟺ w[i] > 0.05 ∧ (flags[i] & require) == require ∧ (flags[i] & forbid) == 0
```

* `poler_lens_filter` — компакция индексов (голый SSE2: `comiss` + ветвления).
* `poler_lens_popcount` — плотность масок (`popcnt`).
* В Causal Nexus: `extract_k_hop_weighted` даёт рёбрам вес
  `edge.weight · 0.5^depth` (затухание по дистанции обхода); барьер 0.05
  отсекает связи глубже ~4–5 хопов — «No-Hits» защита от галлюцинаций
  дальних обходов. Дистанции ≤ 4 (вес ≥ 0.0625) проходят — поведение
  неглубоких K-hop v0.86 сохранено. Статистика в `NexusNode.lens`
  (JSON-поле скрыто при `None` — обратная совместимость).

## Синапс-атомарная SSN

Плотность перекрывает цифру сокровищницы (1.7 Б/синапс):

```text
fp16:  w: u16 (binary16)             → 2.0 Б/синапс payload
i8:    w: i8 × глобальный scale      → 1.0 Б/синапс payload («85 МБ мозг»)
func_id: u4 на ИСТОЧНИК (амортизация) → ~0 Б/синапс
bias:    f32 на ПОСТ-НЕЙРОН           → ~0 Б/синапс
```

50M синапсов FlyWire → 50–100 МБ RAM.

```text
dst(s)  = (s·α) & (n_post−1)          — хэш-адресация (Кнут, α=2654435761)
acc(s)  = Σ_k prim_{func[s]}( w[s,k] · pre[s+k] )
post[d] = decay·post[d] + acc + bias[d]
```

Распаковка весов — одной инструкцией: `vcvtph2ps` (F16C) превращает
8×fp16 в 8×f32; i8 — `vpmovsxbd + vcvtdq2ps + vmulps(scale)`.

### 16 примитивов ObservationCircuit (u4 func_id → jump table)

`0 identity, 1 relu, 2 sigmoid, 3 tanh, 4 expdecay, 5 spike, 6 gauss,
7 abs, 8 sign, 9 softplus, 10 sin, 11 gate, 12 delay, 13 trace,
14 clamp, 15 parity`

* jump table хранит ОТНОСИТЕЛЬНЫЕ офсеты (`.quad .Lp - .Ljt`) —
  PIE-безопасно, ноль релокаций; диспетчеризация `movq rax, xmm9; jmp rax`
  (адрес примитива источника кэшируется в xmm9).
* Трансцендентные — мантисса-полиномы по битам float, без libm:
  * exp: y=x·log2e → floor → f ∈ [0,1) → `2^f−1 ≈ f·(c1+f·(c2+f·c3))`
    собирается прямо в поле экспоненты через `vpaddd/vpslld` (12 инструкций);
  * tanh: рационал `x(27+x²)/(27+9x²)` с клампом ±4 (макс. ошибка 2.4%);
  * sigmoid = 0.5·tanh(0.5x)+0.5; softplus ≈ x·σ(x);
  * sin: кламп ±π/2 + Горнер `x(1−x²/6+x⁴/120)`.
* Контракт хвоста: последняя 8-lane упаковка источника при
  `fanout % 8 ≠ 0` маскируется битовой keep-маской ПОСЛЕ примитива
  (`.Lssn_keep`: j единиц `0xFFFFFFFF`). Маска подменяется только на
  последней упаковке и сбрасывается полной на каждый источник.
* Семантика delay (`vpermilps 0x93`): ротация окна на 1 такт в каждой
  128-бит половине; на хвостовой упаковке новейшие сэмплы вытесняются
  за маску (каузальная задержка с потерей — отражено в скалярном эталоне).
* Контракт паддинга для прямых вызовов asm: `w16/w8` — до
  `n_pre·fanout` округлённого вверх кратного 8 +16 Б; `pre` —
  `n_pre + fanout + 8` элементов.

## CORDIC-ротор Q24

24 сдвигово-складывающие итерации по atan-таблице в `.rodata`
(генератор: `scripts/gen_asm_consts.py`). Свойства:

* **битовая детерминированность** — целочисленные сдвиги/сложения дают
  идентичный результат на любом x86_64 (в отличие от libm);
* sincos: редукция mod 2π (`vroundss`+`vfnmadd`) + квадрантный фолд;
* atan2: масштаб Q22 через `vrcpss`+Ньютон `r·(2−m·r)`, векторинг
  с квадрантной предротацией ±π;
* `renorm(re, im)` — ренормализация массива фаз на S¹ (atan2 → sincos).

## STDP + WTA (LanguageCoreV2)

```text
trace_i ← ρ·trace_i + pre_i            (eligibility, vfmadd213)
Δw_i    = η·reward·trace_i              (трёхфакторное правило)
Δw_i    ← −Δw_i для 25% тормозных       (vpxor, lane 2 и 6)
w_i     ← clamp(w_i + Δw_i, ±w_max)    (vminps/vmaxps)
```

WTA-argmax — турнир `comiss` (первый максимум, голый SSE2).

## Бенчмарк (Xeon, AVX2+FMA+F16C; `poler-engine --asm-bench`)

```text
FEP  step        1048576 ×100: 1383.9 M MAC8/s    parity PASS
FEP  energy      10000000:     1571.3 M elem/s    (12.57 GB/s)
LENS filter      10000000:      219.5 M cand/s    keep 11.8%
LENS popcount    10000000:      949.1 M flags/s
SSN  f16         10M syn ×10:    48.3 M syn/s     payload 20 МБ (2.0 Б/син)
SSN  i8          10M syn ×10:    47.8 M syn/s     payload 10 МБ (1.0 Б/син)
     parity 16 примитивов: 2396.1016 vs 2396.1013  PASS
CORDIC renorm    2000000:       16.0 M elem/s      max‖·‖err 6.6e-7
STDP step        10M ×10:      1988.2 M syn/s
WTA  argmax      1000000:      1030.3 M rate/s
```

19/19 юнит-тестов: asm против скалярных эталонов и libm.

## Сборка

`global_asm!` собирается LLVM IAS (Intel-синтаксис). Замечания тулчейна:

* мнемоника широковещательного элемента — `vbroadcastss`
  (не `vpbroadcastss`);
* смешение xmm/ymm операндов в одной инструкции запрещено — редукция
  `vextractf128 → vaddps xmm → vhaddps ×2`;
* адресный scale ∈ {1,2,4,8} — таблицы масок адресуются `shl eax,5` + `rax*8`;
* PIE: jump table только с относительными офсетами;
* rustc 1.99 + lld: `--gc-sections` роняет global_asm-символы
  (win_call64 и др.) — обходится `-C link-arg=-fuse-ld=bfd`
  (сами ядра и winpe не менялись).

## Роадмап v0.88

1. **FlyWire CSR-импорт** в SynapseField (реальный коннектом вместо
   хэш-адресации) + `--ssn-load`;
2. **func_id → graph_asm JIT**: 16 специализированных слитых циклов
   (диспетчеризация исчезает из внутреннего цикла);
3. `vdivps → vrcpps+Ньютон` в tanh/sigmoid/softplus (×3–5 SSN);
4. AVX-512-вариант SSN (16 lanes, `vpermt2ps`-ротация delay);
5. FEP-шаг в `poler.rs` (канонический POLER-цикл 2D);
6. SubquantumEntangler (vault 0496): трёхчастичная сцепка на CORDIC-роторах.
