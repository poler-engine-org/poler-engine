---
name: poler-engine
description: AI-Native суверенный поисково-аналитический движок (POLER): локальный резонансный поиск с ε-плотностью и K-hop графом, скан архивов без распаковки (zip/tar/tar.gz/tar.zst/gz/zst, пароли ZipCrypto/AES прямо в CLI), крипто-слой данных Vault .pvt (PND v8.2 CBC, MAC, KDF), Суверенный Гиппокамп знаний (299 источников), AIDDE impact-анализ, RAG-чанки, MCP-сервер для LLM-агентов, монорепозиторий Rust + Zig 0.14.0.
---

# POLER-Engine — суверенный гиппокамп и крипто-субстрат данных для LLM-агентов

Поисково-аналитический и крипто-структурный движок, спроектированный как **инструмент для ИИ**:
индексирует, находит, отдаёт, защищает память (.pvt Vault), читает архивы без распаковки,
связывает по метаданным. Не понимает контент, не генерирует текст, не принимает решений —
понимает ИИ, творит автор.

Репозиторий: https://github.com/poler-engine-org/poler-engine
Архитектура: `docs/UNIFIED_ARCHITECTURE.md` | Формат памяти: `docs/formats/VAULT_FORMAT.md` | CLI: `docs/CLI.md`

## Принципы (не нарушать)

1. **Инструмент, не ИИ** — индексировать/находить/отдавать/шифровать/связывать; не понимать, не генерировать, не решать.
2. **Суверенный стек (v2.0 / M4.6)** — никаких облачных API. Нуль внешних ключей. Локальные CPU/mmap структуры.
3. **Крипто-слой данных (CDL)** — PND v8.2 шифр, KDF 100k итераций, постраничный CBC (4096Б), цепочечный MAC, внешний SHA-256.
4. **Архивы не распаковываются** — записи контейнеров читаются напрямую в bounded-память виртуальными путями `архив::запись`; ни один байт не пишется на диск (zip-slip/бомбы исключены by design).
5. **Rust + Zig C-ABI** — микроядро `os/core/` (Zig 0.14.0) компилируется в `libpoler_core.a` с проверкой MVR (54 626 golden-векторов bit-for-bit).
6. **Банальность — сила** — быстрый детерминированный доступ к данным и памяти для ИИ.

## Сборка и тесты

```bash
cd poler-engine
# Сборка с C-ABI крипто-мостом
cargo build --release --features pnd-ffi
./target/release/poler-engine --version

# Полный прогон тестов
cargo test --features pnd-ffi
cargo test -p pqw --lib
(cd os/core && zig test poler_core.zig)
```

## Основные режимы CLI

```bash
# 1. Защищенная память Vault (.pvt CDL)
export POLER_VAULT_KEY="парольная_фраза"
poler-engine --memory-seal <ФАЙЛ> [--memory-out <OUT.pvt>] [--memory-content-id <ID>]
poler-engine --memory-open <ФАЙЛ.pvt> [--memory-out <OUT.bin>]
poler-engine --memory-verify <ФАЙЛ.pvt>     # Проверка внешнего SHA-256 без ввода ключа
poler-engine --memory-info <ФАЙЛ.pvt>       # Метаданные (страницы, заголовок, соль)

# 2. Суверенный Гиппокамп (база 299 источников, 3.65M токенов)
poler-engine --knowledge-ingest <ПУТЬ_К_АРХИВУ>   # Первичный инжест корпуса знаний
poler-engine --knowledge-search "<ЗАПРОС>"       # Поиск по базе знаний
poler-engine --knowledge-stats                   # Статистика и эпистемический статус

# 3. Точный grep-режим (слой 0): ВСЕ совпадения, гарантия полноты, exit-коды grep
poler-engine ~/corpus --grep "fn main" [--grep-regex] [--grep-i] [-A/-B N]
poler-engine ~/corpus --grep "TODO" --grep-json          # byte offsets для агента

# 4. Архивы без распаковки (v0.28.1): недра контейнеров — виртуальные файлы «архив::запись»
poler-engine ~/corpus --grep "Subquantum" --archives     # zip/tar/tar.gz/tar.zst/gz/zst сканируются in-place
poler-engine --archive-list export.zip                   # листинг записей БЕЗ пароля и без распаковки
poler-engine --archive-list export.zip --archive-json    # JSON для агента (имена/размеры/шифрование)
poler-engine "export.zip::docs/paper.md" --chunk         # RAG-чанки записи архива
poler-engine ~/corpus --grep "secret" --archives --archive-password "PASS"   # пароль прямо в CLI
env POLER_ARCHIVE_KEY="PASS" poler-engine ~/corpus --grep "secret" --archives  # пароль без истории shell
poler-engine ~/corpus --grep "x" --archives --archive-max-entry-mb 128        # лимит zip-бомб

# 5. RAG-чанки (слой B): секция→абзац→предложение, byte range якоря
poler-engine document.md --chunk [--chunk-size 384] [--chunk-json]

# 6. Резонансный POLER-поиск: ε-плотность, IIR-резонанс R(t), сцены, K-hop граф
poler-engine ~/corpus -q "Алексей" --format ai-json
poler-engine ~/corpus -q "Алексей" --resonance-mode psi|poler|field|hits

# 7. AIDDE impact-анализ: call graph + upstream/downstream паспорт символа
poler-engine ./src --impact "run_gateway" [--impact-depth 3]

# 8. Веб: краулинг в локальный индекс (robots.txt, SimHash, PageRank) и поиск
poler-engine https://example.com --crawl --crawl-depth 2 --crawl-max 25
poler-engine --web-search "запрос"                      # Semantic Bridge ru↔en

# 9. Интерфейсы и сервисы
poler-engine --mcp                                       # MCP-сервер (stdio JSON-RPC)
poler-engine --mcp-http 127.0.0.1:8765 --mcp-token X     # MCP через HTTP (Bearer)
poler-engine --shell                                     # REPL с Tab-completion
poler-engine --tui                                       # TUI (chat|notes|sources)
poler-engine --gateway                                   # Terminal Gateway (sandbox)
poler-engine --license                                   # статус EULA
```

## Архивы: правила для агента

- **Форматы:** zip (stored/deflate/zstd; шифрование ZipCrypto и AES-256), tar, tar.gz,
  tar.zst, gz (мульти-член — ротация логов), zst, zip-контейнеры (jar/war/epub/odt).
  bzip2/xz не поддерживаются (суверенный минимум зависимостей).
- **Пароль** резолвится по цепочке: `--archive-password` → env `POLER_ARCHIVE_KEY` →
  TTY-промпт (интерактивный терминал спросит сам). Неверный пароль — ошибка в stderr
  на запись, остальной корпус сканируется.
- **Листинг не требует пароля**: `--archive-list` читает центральный каталог raw —
  сначала осмотр (`--archive-json`), потом вскрытие.
- **Бомба-гард:** несжатая запись > 64 МиБ (настраивается `--archive-max-entry-mb`)
  пропускается с ошибкой в stats — RAM всегда bounded.
- **Селектор `архив::запись`** работает с `--chunk` (формат записи — по её расширению).
- Ничего никогда не распаковывается на диск: если нужен «файл» из архива для другой
  команды — используй селектор `::`, а не unzip.

## MCP-инструменты (для LLM-агентов)

`poler_web_search`, `poler_crawl`, `poler_fetch`, `poler_search`,
`poler_grep` (аргументы `archives: true`, `archive_password: "…"` — скан недр архивов),
`poler_chunk`, `poler_box_exec`, `poler_box_status`, `poler_knowledge`.
