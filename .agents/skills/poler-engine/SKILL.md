---
name: poler-engine
description: AI-Native суверенный поисково-аналитический движок POLER v0.32.0 (Rust + Zig 0.14.0). Резонансный поиск с ε-плотностью и K-hop графом, grep с гарантией полноты, скан архивов БЕЗ распаковки (zip/tar/tar.gz/tar.zst/gz/zst, пароли ZipCrypto/AES прямо в CLI), RAG-чанки с byte-якорями, крипто-память Vault .pvt (PND v8.2, CBC + KDF 100k + MAC), Суверенный Гиппокамп знаний с MVR-провенансом, нативный инференс .pqw (энкодеры XLM-R/BGE-M3, GLiNER, GLM-декодер int8/int4 — без Python/ONNX/GPU), ИДЕАЛЬНЫЙ ИСПОЛНИТЕЛЬ КОМАНД poler_exec E2 (ядро Zig raw-syscalls: таймауты TERM→grace→KILL группе, захват голова+маркер+хвост O(1), pidfd — зомби невозможны, PTY 200x50 для isatty-программ, cwd/env, PATH разрешает ребёнок, отмена cancel_flag за ≤25 мс, ФОНОВЫЕ задачи poler_exec_async/kill), CLI --exec и MCP-семейство из 5 инструментов с ПАРАЛЛЕЛЬНЫМ исполнением (пул воркеров), РЕЗИДЕНТНЫЙ MCP-сервер для LLM-агентов (тёплые хэндлы в RAM: grep p50≈650 мкс, knowledge p99≈250 мкс; стриминг логов в зашифрованный Vault на лету), Rust-ридер коннектома FLYCSR1 — мозг мухи FlyWire v783 (138 639 нейронов, 54,5 млн синапсов) как живая матрица A: ротор J = A − Aᵀ, K-hop BFS, CSC-impact прямо из zstd-артефакта. Математическая база — пятифазный когнитивный цикл ℘-O-L-ε-R[n]-Ψ и каноническое уравнение dp/dt = -η·Π_Λ[D·p + γJ·p + ∇F]. Использовать для: поиска по данным и архивам, RAG-подготовки, защиты памяти, базы знаний, семантического поиска, NER, НАДЁЖНОГО запуска команд с таймаутами/PTY/фоновым режимом и лимитом вывода, работы с сотнями документов (пересказ, противоречия, кластеризация, валидация), графовых запросов к биологическому эталону связности.
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

### 2. Идеальный исполнитель команд E2/v0.32.0 (фича pnd-ffi) — замена падающим Bash-инструментам

Рождён диагностикой исходников GNU bash 5.2 самим движком
(docs/EXEC_AUDIT.md: 424 unsafe-вызова в 75 .c-файлах, free() в trap-механике,
REINSTALL_SIGCHLD-гонка, неограниченный $(...), ноль таймаутов). Ядро —
Zig с raw-syscall слоем: классы bash-ошибок исключены конструктивно.

```bash
# флаги — ДО --exec; после --exec — команда целиком (включая её -флаги)
poler-engine --exec-timeout-ms 5000 --exec make -j4
poler-engine --exec-max-out 65536 --exec dd if=/dev/zero bs=1M count=64
poler-engine --exec-stdin "вопрос" --exec cat
# E2: рабочий каталог, окружение, терминал, голова+хвост
poler-engine --exec-cwd /var/log --exec ls -la
poler-engine --exec-env LC_ALL=C --exec-env DEBUG=1 --exec sort file.txt
poler-engine --exec-pty --exec sudo -n apt-get update      # isatty-программы
poler-engine --exec-capture head_tail --exec-max-out 4096 --exec make -j8  # и голова, и хвост лога
# коды: ребёнка | 124 таймаут | 127 не найдено | 126 без права | 125 плохой cwd
```

Гарантии: таймаут timerfd(MONOTONIC) + SIGTERM → grace → SIGKILL группе;
захват — O(1) памяти (tail: хвост max_out; head_tail: первые B/2 + маркер
«dropped N» + последние B/2 — стек-трейс в начале гигантского лога
не теряется); зомби невозможны (pidfd + wait4 в ppoll-цикле); шелл-инъекции
невозможны (argv массивом, без парсинга); fds-гигиена CLOEXEC; PTY —
настоящий терминал 200x50 (stdout/stderr слиты); PATH разрешает сам ребёнок
(ноль stat в родителе); отмена — атомарный флаг, TERM→KILL за ≤25 мс.

### 3. Архивы без распаковки (v0.28.1) — виртуальные файлы «архив::запись»

```bash
poler-engine ~/corpus --grep "Subquantum" --archives # zip/tar/tar.gz/tar.zst/gz/zst in-place
poler-engine --archive-list export.zip               # листинг БЕЗ пароля и без распаковки
poler-engine --archive-list export.zip --archive-json # JSON: имена/размеры/шифрование
poler-engine "export.zip::docs/paper.md" --chunk     # RAG-чанки записи архива
poler-engine ~/corpus --grep "secret" --archives --archive-password "PASS"  # пароль в CLI
env POLER_ARCHIVE_KEY="PASS" poler-engine ~/corpus --grep "secret" --archives
poler-engine ~/corpus --grep "x" --archives --archive-max-entry-mb 128     # лимит бомб
```

### 4. RAG-чанки (слой B) — секция→абзац→предложение, byte-якоря + breadcrumb

```bash
poler-engine document.md --chunk [--chunk-size 384] [--chunk-overlap N] [--chunk-json]
poler-engine "dump.zip::17-Active_inference.md" --chunk --chunk-json  # запись архива
```

### 5. Резонансный POLER-поиск — ε-плотность, IIR-резонанс R(t), сцены, K-hop граф

```bash
poler-engine ~/corpus -q "Алексей" --format ai-json    # машинный JSON с ε и R(t)
poler-engine ~/corpus -q "Алексей" --resonance-mode psi|poler|field|hits
poler-engine ~/corpus -q "тема" -k 4                   # K-hop подграф связей
# Гиперпараметры канона — прямо в CLI (см. MATH.md):
#   --psi-eta 0.05 --psi-gamma 0.5 --psi-rho 0.9 --psi-depth 8
#   --poler-eta 0.01 --poler-gamma 0.1 --poler-mix 0.1 --poler-dissipator 0.02
```

### 6. Нативный суверенный ML (.pqw, без Python/ONNX/GPU)

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

### 7. Защищённая память Vault (.pvt CDL) — «насмерть», git-переносимая

```bash
export POLER_VAULT_KEY="парольная_фраза"            # или ввод с stdin/TTY
poler-engine --memory-seal <ФАЙЛ> [--memory-out OUT.pvt] [--memory-content-id ID]
poler-engine --memory-open <ФАЙЛ.pvt> [--memory-out OUT.bin]
poler-engine --memory-verify <ФАЙЛ.pvt>             # внешний SHA-256 БЕЗ ключа
poler-engine --memory-info <ФАЙЛ.pvt>               # страницы, соль, KDF-итерации
# Ключ через env: --memory-key-env ИМЯ_ПЕРЕМЕННОЙ; KDF: --memory-kdf-iters N
```

### 8. Суверенный Гиппокамп — база знаний с эпистемической градацией

```bash
poler-engine --knowledge-ingest ~/library/          # 6 слоёв, FNV-дедуп, MVR-разметка
poler-engine --knowledge-search "запрос" [--min-provenance mvr|source|narrative]
poler-engine --knowledge-stats                      # источники/чанки/токены/провенанс
# Эмбеддер: --knowledge-embedder none|hash|pqw (pqw: env POLER_KNOWLEDGE_MODEL=path.pqw)
# БД по умолчанию knowledge.db (+ .graph/.vectors спутники); --knowledge-db PATH
```

### 9. AIDDE impact-анализ — call graph + паспорт символа

```bash
poler-engine ./src --impact "run_gateway" [--impact-depth 3] [--impact-cache db]
```

### 10. Коннектом FLYCSR1 (v0.30.0) — мозг мухи как матрица A

```bash
poler-engine --connectome docs/flywire-connectome/flywire_v783_core.csr.zst
poler-engine --connectome ...core.csr.zst --connectome-nodes ...nodes.bin \
  --connectome-node 0                # паспорт нейрона (индекс или root_id)
poler-engine --connectome ... --connectome-edge 0:6135       # вес/медиатор/знак + J = A − Aᵀ
poler-engine --connectome ... --connectome-khop 0 -k 2      # BFS потока сигнала: [13, 443], 457
poler-engine --connectome ... --connectome-impact 0         # CSC «кто управляет нейроном»
poler-engine --connectome ... --connectome-json            # JSON для агентов
```

- Знак связи: **+1** ach/glut, **−1** gaba, **0** модуляторы (oct/ser/da);
  фильтр потока: `--connectome-sign all|exc|inh`. Exit-коды grep: 0 найдено / 1 нет / 2 ошибка.
- Артефакты в git (~43 МБ): full 35 МБ / core 7 МБ / nodes 1.1 МБ; загрузка
  core ≈ 70 мс (≈ 25 МБ RAM), full ≈ 400 мс; сырьё 852 МБ feather не нужно.

### 11. Веб (локальный индекс, не облако)

```bash
poler-engine https://example.com --crawl --crawl-depth 2 --crawl-max 25
poler-engine --web-search "запрос"                  # Semantic Bridge ru↔en офлайн
poler-engine --semantic-expand "запрос"             # диагностика моста
```

### 12. Интерфейсы и сервисы

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
`poler_exec` (E2: идеальный запуск команд — command/args/timeout_ms/
grace_ms/max_out_bytes/stdin/cwd/env/pty/capture → JSON exit_code/signal/
stdout/stderr/timed_out/cancelled/truncated/duration_us/pid; ядро Zig
raw-syscalls; умолчание capture=head_tail),
`poler_exec_async` (фоновый запуск → task_id мгновенно),
`poler_exec_task` (опрос/ожидание: task_id + wait_ms),
`poler_exec_kill` (отмена: TERM→KILL за ≤25 мс),
`poler_exec_list` (обзор фоновых задач),
`poler_knowledge` (query, top, min_provenance).

**E2: параллельное исполнение.** stdio-сервер исполняет exec-инструменты
в пуле воркеров (2–8 потоков): агент может отправлять N запросов подряд —
они выполняются ПАРАЛЛЕЛЬНО, ответы приходят по готовности (JSON-RPC
сопоставляет по id). Пример: два `sleep 1` параллельно = 1.0 с стеновых,
а не 2 с.

### M6: резидентное состояние (real-time, без холодного старта)

Сервер держит в RAM между запросами: тёплый Гиппокамп (SQLite/RaBitQ/HNSW/
эмбеддер — открыты один раз), LRU-кэш файлов для grep (инвалидация mtime+len)
и WebIndex. Каждый dispatch замеряется и (опционально) стримится в
зашифрованный журнал.

```bash
poler-engine --mcp --vault-log session.pvt          # + стриминг логов вызовов в Vault .pvt
POLER_VAULT_PASS="фраза" poler-engine --mcp --vault-log session.pvt  # пароль через env
poler-engine --mcp --mcp-ram-budget 64               # бюджет RAM-кэша, МиБ (по умолч. 48)
poler-engine --mcp-bench 500                          # бенчмарк резидентности: cold vs warm p50/p95/p99
```

- Формат журнала в `.pvt`: JSON-строки `{"ts","method","tool","us","ok"}` —
  файл **валиден на каждом коммите**, читается штатным `--memory-open/verify`.
- `--mcp-bench` — приёмка M6: тёплые p99 < 5 мс (exit 0/1); на release-сборке
  grep p50≈650 мкс / p99≤2 мс, knowledge p99≈250 мкс.

## Верификационные числа (золотой стандарт)

- Rust: **1177/1177** тестов (pnd-ffi; E2: +13 exec-семейство), default **1115/1115**; Zig: **21/21** exec + 33/33 крипто/ABI; golden-векторы PND: **54 626 bit-for-bit**.
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
