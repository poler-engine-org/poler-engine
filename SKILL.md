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

Приоритеты v2.0: Foundation (whatlang/stemmers/unicode-segmentation/madvise/Teddy) →
Compression (FSST/zstd/lz4) → Vector Layer (BGE-M3/usearch/RaBitQ) → SPLADE →
IIR-Resonance Fusion (уникальная инновация) → Code Intelligence (tree-sitter/Salsa) →
KG (GLiNER/Leiden) → Streaming Archives → Agentic/MCP v2 → Differential Dataflow.
