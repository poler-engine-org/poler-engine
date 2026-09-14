---
name: poler-engine
description: AI-Native поисково-аналитический движок на Rust (POLER): локальный резонансный поиск с ε-плотностью и K-hop графом, grep-режим с гарантией полноты, RAG-чанки, AIDDE impact-анализ, веб-краулинг через CDP, MCP-сервер для LLM-агентов. v2.0 sovereign stack — без Google/NotebookLM/Gmail/Drive/OAuth.
---

# POLER-Engine — гиппокамп для LLM-агентов

Поисково-аналитический движок, спроектированный как **инструмент для ИИ**:
индексирует, находит, отдаёт, связывает по метаданным. Не понимает контент,
не генерирует текст, не принимает решений — понимает ИИ, творит автор.

Репозиторий: https://github.com/poler-engine-org/poler-engine
Мастер-план v2.0: `PLAN_POLER_V2.md` (в корне репозитория)

## Принципы (не нарушать)

1. **Инструмент, не ИИ** — индексировать/находить/отдавать/связывать; не
   понимать, не генерировать, не решать.
2. **Суверенный стек (v2.0)** — никаких облачных API. Google/NotebookLM/
   Gmail/Drive/OAuth удалены. Веб-краулинг через CDP и MCP — разрешены
   (это не облачная зависимость).
3. **Rust + CPU only** — 16-32GB RAM target, инференс только через ONNX
   Runtime (план v2.0), без Python runtime и GPU.
4. **Банальность — сила** — как grep/SQL/git: банальный доступ к данным для ИИ.

## Сборка

```bash
cd poler-engine
cargo build --release -j1        # -j1 при RAM < 8GB (LTO + codegen-units=1)
./target/release/poler-engine --version
```

## Основные режимы CLI

```bash
# Резонансный POLER-поиск: ε-плотность, IIR-резонанс R(t), сцены, K-hop граф
poler-engine ~/corpus -q "Алексей" --format ai-json
poler-engine ~/corpus -q "Алексей" --resonance-mode psi|poler|field|hits

# Точный grep-режим (слой 0): ВСЕ совпадения, гарантия полноты, exit-коды grep
poler-engine ~/corpus --grep "fn main" [--grep-regex] [--grep-i] [-A/-B N]
poler-engine ~/corpus --grep "TODO" --grep-json          # byte offsets для агента

# RAG-чанки (слой B): секция→абзац→предложение, byte range якоря
poler-engine document.md --chunk [--chunk-size 384] [--chunk-json]

# AIDDE impact-анализ: call graph + upstream/downstream паспорт символа
poler-engine ./src --impact "run_gateway" [--impact-depth 3] [--impact-cache db]

# Веб: краулинг в локальный индекс (robots.txt, SimHash, PageRank) и поиск
poler-engine https://example.com --crawl --crawl-depth 2 --crawl-max 25
poler-engine --web-search "запрос"                      # Semantic Bridge ru↔en
poler-engine --semantic-expand "запрос"                 # диагностика моста

# Интерфейсы
poler-engine --shell                                     # REPL с Tab-completion
poler-engine --tui                                       # TUI (chat|notes|sources)
poler-engine --gateway                                   # Terminal Gateway (sandbox)
poler-engine --mcp                                       # MCP-сервер (stdio JSON-RPC)
poler-engine --mcp-http 127.0.0.1:8765 --mcp-token X     # MCP через HTTP (Bearer)
poler-engine --license                                   # статус EULA (без гейтов)
```

## MCP-инструменты (для LLM-агентов)

`poler_web_search`, `poler_crawl`, `poler_fetch`, `poler_search`,
`poler_grep`, `poler_chunk`, `poler_box_exec`, `poler_box_status`.

v2.0: `poler_gmail` / `poler_drive` / `poler_nlm` удалены (суверенный стек).

## Бенчмарк

```bash
poler-engine --benchmark [--benchmark-json report.json]
```

## Что удалено в v2.0 (не возвращать)

- `src/google/` (OAuth, Gmail, Drive, NotebookLM, Auth Companion, CDP-профиль Google)
- `dev-stand/` (Google-аутентификационный стенд)
- CLI: `--google-*`, `--nlm-*`, `--auth-ui`, `--import-browser-session`, `--license-import`
- Зависимость `ed25519-dalek` (License Gate гейтил только google-интеграции;
  EULA-уведомление сохранено в `src/license/mod.rs` как метаданные)
- TUI: панель NotebookLM; Doc Browser работает только с локальными источниками

## Сохранено (локальное, не Google)

- `--web`, `--crawl`, `--web-search` (CDP-краулинг — это не Google)
- `--mcp`, `--mcp-http` (MCP-сервер)
- `--gateway` (Docker sandbox Terminal Gateway)
- `--shell` / `--tui`, notes/sources (локальная SQLite)
- VCS-адаптеры gh/gl/gt/gix (GitHub REST — публичные API, не OAuth-профиль)
- `arboard` (локальный буфер обмена TUI)

## Тестовый корпус

Роман «Eteryya» (65K+ файлов): https://github.com/Kotokvit/Eteryya

```bash
git clone --depth 1 https://github.com/Kotokvit/Eteryya.git ~/eteryya
poler-engine ~/eteryya -q "Алексей" --format ai-json | head -50
```

## Дальнейший план (PLAN_POLER_V2.md)

Готово (v2.0): Шаг 1 — отвязка Google/NLM (суверенный стек); Шаг 2 Foundation —
madvise/whatlang/Snowball/UAX#29 + **Teddy SIMD** (`retrieval/teddy.rs`: решёто
якорных байтов pshufb + адаптивные якоря + быстрый фолд кириллицы; LeftmostLongest
эквивалент AC — дифференциальные тесты; ASCII 2.7× быстрее AC, кириллица 2.15×);
Шаг 3 — **Compression** (`src/compression/`: чистый порт FSST с compress-probe
лукапами + `VocabArena` (словарь корпуса 4.1× плотнее HashMap), пер-файловые
словари ID-парами 8×, lz4-парковка постингов, zstd doc store 26×; трейт
`TermFreqs` — ε побитово совпадает между представлениями);
Шаг 4 (кирпич 1 из 3) — **Vector Substrate** (`src/vectors/`: чистый RaBitQ —
рандомизированное вращение Адамара + 1-битные коды, оценки sym (arcsin-MLE,
popcount) и ADC (центрированный, несмещённый); poler-native HNSW над кодами —
граф держит 100% потолка оценщика; mmap-хранилище zero-copy; плотность 768-d
**24× по кодам / 21.3× всего**; скан кодов 8.4 ГБ/с; Embedder-трейт — точка
подключения BGE-M3);
Шаг 4 (кирпич 2) — **Sovereign ML (Part E/F)** — архитектура изменена:
fastembed/ort/ONNX ОТМЕНЕНЫ, нейроинференс нативный через собственное ядро
`pqc` (`src/pqc/`: tensor — AVX2 weight-only int8/int4/f32 кернелы, LayerNorm/
RMSNorm/GELU/SiLU/softmax/RoPE; pqw — формат весов `.pqw` v2: mmap zero-copy,
SHA-256 верификация при открытии, секции по страницам 4096, см.
`docs/PQW_FORMAT.md`; encoder — BERT/XLM-R-спина BGE-M3/GLiNER; sha256 — свой
FIPS 180-4; selftest). Потребители: `src/llm/glm_engine.rs` (GLM-декодер:
RoPE + MQA/GQA + SwiGLU + KV-арена + MoE-роутер топ-k + greedy/temperature/
top-p сэмплирование + UAX#29-детокенизатор), `src/ner/native_gliner.rs`
(span-голова GLiNER), `src/vectors/pqw_bridge.rs` (PqwEmbedder → Embedder →
RaBitQ-субстрат). CLI: `--pqw-selftest` (автономный цикл 6/6), `--semantic
dense --model X.pqw`, `--semantic-corpus PATH` (живой поиск по корпусу:
чанки → нативные эмбеддинги → косинус), `--llm local --model X.pqw`, `--ner
gliner --model X.pqw`. Дифференциальные тесты против наивных fp32-эталонов:
int8 cos>0.999, int4 >0.98, fp32 >0.9999; KV-инвариант побитовый;
SHA-тамперинг ловится.

**Кирпич 2.5 (реальные веса) закрыт:** `scripts/convert_hf_to_pqw.py` —
конвертер torch-zip (pytorch_model.bin) → `.pqw` int8/int4 ПОТОКОВО (блоки
строк; RAM не растёт с моделью — тот же паттерн для 70B); маппинг XLM-R →
конвенция pqw; встраивание токенизатора. `src/pqc/tokenizer.rs` — нативный
XLM-R-токенизатор (Unigram-Viterbi + Metaspace + per-codepoint NFKC-таблица
+ NFC-композиция по сгенерированным таблицам `src/pqc/nfc_tables.rs`):
**40/40 золотых текстов побитово = HF `tokenizers` v0.23.2** (снятых
`scripts/extract_tokenizer_data.py`; интеграционный тест на реальной модели
само-скипается без файла). BGE-M3 int8 573 МБ: послойный дифференциал с
fp32-numpy-эталоном **cos ≥ 0.9999 на всех 24 слоях**, семантика
**0.75/0.29 = fp32-эталон** (тест «связанный текст ближе постороннего»).
Живой поиск: `--semantic dense --model models/bge-m3.pqw --semantic-corpus
<ло́р> -q "…"` (rayon-параллельное эмбеддингирование). **1053 теста
зелёные (+9), ноль новых зависимостей.**

Дальше: DeBERTa-v3-спина (disentangled attention — все публичные GLiNER-
чекпоинты на ней) + конвертер GLM (ChatGLM3-6B → .pqw int4, стримингово,
Фаза 12.7; у владельца диск больше песочницы — конвертер уже в репо) →
кирпич 3 (ColBERT/Matryoshka) → SPLADE → IIR-Resonance Fusion
(уникальная инновация) → Code Intelligence (tree-sitter/Salsa) → KG
(GLiNER-веса → .pqw, Leiden) → Streaming Archives → Agentic/MCP v2 →
Differential Dataflow → .pqw/pqc bridge → **локальный GLM-3 6B/70B (Part F)**.
