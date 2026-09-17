---
name: poler-engine
description: AI-Native суверенный поисково-аналитический движок POLER v0.28.1 (Rust + Zig 0.14.0). Резонансный поиск с ε-плотностью и K-hop графом, grep с гарантией полноты, скан архивов БЕЗ распаковки (zip/tar/tar.gz/tar.zst/gz/zst, пароли ZipCrypto/AES прямо в CLI), RAG-чанки с byte-якорями, крипто-память Vault .pvt (PND v8.2, CBC + KDF 100k + MAC), Суверенный Гиппокамп знаний с MVR-провенансом, нативный инференс .pqw (энкодеры XLM-R/BGE-M3, GLiNER, GLM-декодер int8/int4 — без Python/ONNX/GPU), AIDDE impact-анализ, MCP-сервер для LLM-агентов. Математическая база — пятифазный когнитивный цикл ℘-O-L-ε-R[n]-Ψ и каноническое уравнение dp/dt = -η·Π_Λ[D·p + γJ·p + ∇F]. Использовать для: поиска по данным и архивам, RAG-подготовки, защиты памяти, базы знаний, семантического поиска, NER, работы с сотнями документов (пересказ, противоречия, кластеризация, валидация).
---

# POLER-Engine — суверенный гиппокамп, крипто-субстрат и нативный инференс для LLM-агентов

Поисково-аналитический, крипто-структурный и инференс-движок, спроектированный как
**инструмент для ИИ**: индексирует, находит, отдаёт, защищает память (.pvt Vault),
читает архивы без распаковки, режет RAG-чанки, эмбеддит и извлекает сущности нативно
на CPU. Не понимает контент, не генерирует текст, не принимает решений — понимает ИИ,
творит автор.

Репозиторий: https://github.com/poler-engine-org/poler-engine
Архитектура: `docs/UNIFIED_ARCHITECTURE.md` | Vault: `docs/formats/VAULT_FORMAT.md` |
.pqw: `docs/formats/PQW_FORMAT.md` | CLI: `docs/CLI.md` | **Математика: `MATH.md`** (рядом с этим файлом)

## Принципы (не нарушать)

1. **Инструмент, не ИИ** — индексировать/находить/отдавать/шифровать/связывать; не
   понимать, не генерировать, не решать.
2. **Суверенный стек** — никаких облачных API, нуль внешних ключей. Локальные CPU,
   mmap-структуры, чистый Rust + Zig C-ABI.
3. **Крипто-слой данных (CDL)** — PND v8.2 (256-бит, 32 раунда Feistel, GF(2⁸)),
   KDF 100k итераций, постраничный CBC (4096 Б), цепочечный MAC, внешний SHA-256.
4. **Архивы не распаковываются** — записи читаются напрямую в bounded-память
   виртуальными путями `архив::запись`; ни один байт не пишется на диск
   (zip-slip/бомбы/inode-кража исключены by design).
5. **Нативный инференс** — вес­а `.pqw` v2 (mmap zero-copy, SHA-256 при открытии),
   кернелы pqc (int8/int4/f32, LayerNorm/RoPE/SwiGLU/MoE). Не ONNX, не Python, не GPU.
6. **MVR (Mathematical Verification Record)** — каждое утверждение проверяемо:
   golden-векторы bit-for-bit, дифференциальные тесты против fp32-эталонов, Z3/SMT.
7. **Банальность — сила** — быстрый детерминированный доступ к данным, как grep/SQL/git.

## Сборка и тесты

```bash
cd poler-engine
# Крипто-мост: готовая Zig-библиотека (или zig в PATH / POLER_ZIG)
POLER_CORE_LIB=$PWD/os/core/zig-out/lib cargo build --release --features pnd-ffi
./target/release/poler-engine --version

# Тесты: 1072/1072 (pnd-ffi), zig 33/33, golden 54 626 bit-for-bit
cargo test --features pnd-ffi
(cd os/core && zig test poler_core.zig)
```

## Справочник режимов CLI

### 1. Точный grep (слой 0) — ВСЕ совпадения, гарантия полноты, exit-коды grep

```bash
poler-engine ~/corpus --grep "fn main" [--grep-regex] [--grep-i] [-A/-B N] [--grep-count]
poler-engine ~/corpus --grep "TODO" --grep-json      # byte offsets для агента
```

### 2. Архивы без распаковки (v0.28.1) — виртуальные файлы «архив::запись»

```bash
poler-engine ~/corpus --grep "Subquantum" --archives # zip/tar/tar.gz/tar.zst/gz/zst in-place
poler-engine --archive-list export.zip               # листинг БЕЗ пароля и без распаковки
poler-engine --archive-list export.zip --archive-json # JSON: имена/размеры/шифрование
poler-engine "export.zip::docs/paper.md" --chunk     # RAG-чанки записи архива
poler-engine ~/corpus --grep "secret" --archives --archive-password "PASS"  # пароль в CLI
env POLER_ARCHIVE_KEY="PASS" poler-engine ~/corpus --grep "secret" --archives
poler-engine ~/corpus --grep "x" --archives --archive-max-entry-mb 128     # лимит бомб
```

### 3. RAG-чанки (слой B) — секция→абзац→предложение, byte-якоря + breadcrumb

```bash
poler-engine document.md --chunk [--chunk-size 384] [--chunk-overlap N] [--chunk-json]
poler-engine "dump.zip::17-Active_inference.md" --chunk --chunk-json  # запись архива
```

### 4. Резонансный POLER-поиск — ε-плотность, IIR-резонанс R(t), сцены, K-hop граф

```bash
poler-engine ~/corpus -q "Алексей" --format ai-json    # машинный JSON с ε и R(t)
poler-engine ~/corpus -q "Алексей" --resonance-mode psi|poler|field|hits
poler-engine ~/corpus -q "тема" -k 4                   # K-hop подграф связей
# Гиперпараметры канона — прямо в CLI (см. MATH.md):
#   --psi-eta 0.05 --psi-gamma 0.5 --psi-rho 0.9 --psi-depth 8
#   --poler-eta 0.01 --poler-gamma 0.1 --poler-mix 0.1 --poler-dissipator 0.02
```

### 5. Нативный суверенный ML (.pqw, без Python/ONNX/GPU)

```bash
poler-engine --pqw-selftest                        # автономный цикл 6/6 (энкодер→GLiNER→GLM)
poler-engine doc.md -q "POLER" --semantic dense --model models/encoder.pqw
poler-engine doc.md -q "запрос" --semantic dense --model models/bge-m3.pqw \
    --semantic-corpus ~/corpus --semantic-limit 10 # живой поиск: чанки→эмбеддинги→косинус
poler-engine doc.md -q "..." --ner gliner --model models/gliner.pqw \
    --ner-labels "ORGANIZATION,PERSON,CONCEPT"      # zero-shot NER
poler-engine doc.md -q "..." --llm local --model models/glm.pqw  # GLM-декодер

# Конвертеры реальных весов (стримингово, RAM не растёт с моделью):
python3 scripts/convert_hf_to_pqw.py     # torch-zip → .pqw int8/int4 (XLM-R/BGE-M3)
python3 scripts/convert_gliner_to_pqw.py # GLiNER (mdeberta-спина) → .pqw
python3 scripts/convert_chatglm_to_pqw.py# ChatGLM3-6B → .pqw int4
python3 scripts/gen_demo_pqw.py          # демо-модели для смоук-тестов
```

### 6. Защищённая память Vault (.pvt CDL) — «насмерть», git-переносимая

```bash
export POLER_VAULT_KEY="парольная_фраза"            # или ввод с stdin/TTY
poler-engine --memory-seal <ФАЙЛ> [--memory-out OUT.pvt] [--memory-content-id ID]
poler-engine --memory-open <ФАЙЛ.pvt> [--memory-out OUT.bin]
poler-engine --memory-verify <ФАЙЛ.pvt>             # внешний SHA-256 БЕЗ ключа
poler-engine --memory-info <ФАЙЛ.pvt>               # страницы, соль, KDF-итерации
# Ключ через env: --memory-key-env ИМЯ_ПЕРЕМЕННОЙ; KDF: --memory-kdf-iters N
```

### 7. Суверенный Гиппокамп — база знаний с эпистемической градацией

```bash
poler-engine --knowledge-ingest ~/library/          # 6 слоёв, FNV-дедуп, MVR-разметка
poler-engine --knowledge-search "запрос" [--min-provenance mvr|source|narrative]
poler-engine --knowledge-stats                      # источники/чанки/токены/провенанс
# Эмбеддер: --knowledge-embedder none|hash|pqw (pqw: env POLER_KNOWLEDGE_MODEL=path.pqw)
# БД по умолчанию knowledge.db (+ .graph/.vectors спутники); --knowledge-db PATH
```

### 8. AIDDE impact-анализ — call graph + паспорт символа

```bash
poler-engine ./src --impact "run_gateway" [--impact-depth 3] [--impact-cache db]
```

### 9. Веб (локальный индекс, не облако)

```bash
poler-engine https://example.com --crawl --crawl-depth 2 --crawl-max 25
poler-engine --web-search "запрос"                  # Semantic Bridge ru↔en офлайн
poler-engine --semantic-expand "запрос"             # диагностика моста
```

### 10. Интерфейсы и сервисы

```bash
poler-engine --mcp                                   # MCP-сервер (stdio JSON-RPC)
poler-engine --mcp-http 127.0.0.1:8765 --mcp-token X # MCP через HTTP (Bearer)
poler-engine --shell | --tui | --gateway | --license
poler-engine --benchmark [--benchmark-json r.json]   # Exact/Lexical/Passage/latency/RAM
```

## Когнитивный цикл работы с документами (глаза для модели)

Как читать сотни документов «как человек», а не автоматом: движок даёт якоря и
извлечение, агент — понимание. Фазы ℘-O-L-ε-R[n]-Ψ (полная математика — `MATH.md`):

| Фаза | Смысл | Команда движка |
|------|-------|----------------|
| ℘ Перцепция | осмотр без искажения | `--archive-list --archive-json` (что внутри, до пароля) |
| O Образ | сущности и структура | `--chunk --chunk-json` (byte-якоря, breadcrumb); `--ner gliner` |
| L Логика | противоречия | `--grep --archives` по утверждению → кросс-док сравнение цитат |
| ε Энергия | плотность смысла | `-q --format ai-json` (ε-ранжирование сцен) |
| R[n] Резонанс | темпоральное эхо | итеративный `-q` + `-k` K-hop подграф (накопление контекста) |
| Ψ Аттрактор | верифицированный итог | `--knowledge-search --min-provenance mvr` → MVR-паспорт |

Четыре операции над корпусом (протестированы на 300-документном архиве 32 МБ):

1. **Точный пересказ** — `--chunk` режет с координатами (путь, строки, byte_start/byte_end);
   каждый тезис привязан к строке исходника, галлюцинации исключены конструктивно.
2. **Поиск противоречий** — `--grep --archives "формула/утверждение"` собирает ВСЕ контексты;
   агент сопоставляет и фиксирует расхождения с двумя точными цитатами.
3. **Классификация по темам** — `--ner gliner` извлекает [ТЕОРИЯ]/[АВТОР]/[МЕТОД]/[ИНВАРИАНТ],
   пересечение сущностей + K-hop граф дают кластеры документов.
4. **Паспорт валидации** — градус провенанса каждого тезиса: **MVR** (доказано кодом/Z3) >
   **Source** (рецензируемый источник) > **Narrative** (гипотеза/заметка).

## Архивы: правила для агента

- **Форматы:** zip (stored/deflate/zstd; ZipCrypto и AES-256), tar, tar.gz, tar.zst,
  gz (мульти-член), zst; контейнеры jar/war/epub/odt. bzip2/xz нет (суверенный минимум).
- **Пароль** — цепочка: `--archive-password` → env `POLER_ARCHIVE_KEY` → TTY-промпт.
  Неверный пароль = ошибка в stderr на запись, остальной корпус сканируется.
- **Сначала осмотр** (`--archive-list` без пароля), потом вскрытие.
- **Бомба-гард:** распакованная запись > 64 МиБ (`--archive-max-entry-mb`) пропускается.
- **Селектор `архив::запись`** работает с `--chunk`. Нужен «файл» из архива — используй `::`,
  никогда не unzip. Вложенные архивы (zip в tar) v1 не раскрываются.
- **На диск не пишется ничего** — верифицировано тестами.

## Vault: правила для агента

- Ключ: env `POLER_VAULT_KEY` → stdin (пустой = отказ). Вывод существует → отказ с подсказкой.
- `.pvt` можно хранить в git/переносить: `--memory-verify` проверяет целостность БЕЗ ключа
  (внешний SHA-256), tamper 1 бит ловится. Полный доступ — только с ключом.
- Формат: страницы 4096 Б CBC (IV из соли+номера), цепочечный MAC листьев, KDF 100k.
- Логи/диалоги агента запечатываются в `.pvt` и гоняются через любой git.

## MCP-инструменты (для LLM-агентов)

`poler_web_search`, `poler_crawl`, `poler_fetch`, `poler_search`,
`poler_grep` (аргументы `archives: true`, `archive_password: "…"`),
`poler_chunk`, `poler_box_exec`, `poler_box_status`,
`poler_knowledge` (query, top, min_provenance).

## Верификационные числа (золотой стандарт)

- Rust: **1072/1072** тестов (pnd-ffi); Zig: **33/33**; golden-векторы PND: **54 626 bit-for-bit**.
- pqw/pqc: дифференциалы против fp32-эталонов — int8 cos > 0.999, int4 > 0.98, fp32 > 0.9999.
- Токенизатор XLM-R: 40/40 текстов побитово = HF `tokenizers` v0.23.2.
- BGE-M3 int8 (573 МБ): cos ≥ 0.9999 на всех 24 слоях. GLiNER (urchade/gliner_multi):
  7/7 сущностей, скоры в допуске 0.03.
- GLM-декодер: greedy ~1450 ток/с, MoE int4 ~840 ток/с (2 ядра CPU).
- Vault: 33 МБ за 5.4 с (KDF 100k = 0.62 с фиксированно); tamper-детект побитовый.

## Связанные навыки

- `poler-causal-operator` — операционный манифест ℘-O-L-ε-R[n]-Ψ (как ДУМАТЬ в парадигме).
- `document-translator` — перевод техдоков с защитой формул LaTeX и Mermaid.
- Тестовый корпус: роман «Eteryya» https://github.com/Kotokvit/Eteryya
