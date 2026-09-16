# CLI-референс POLER Engine

> Источник истины — `src/main.rs` (clap derive): `--help` всегда актуален.
> Этот документ — сгруппированная аннотация всех режимов и флагов v2.0
> (~85 флагов). Карта всех документов — [INDEX.md](INDEX.md).

Общий скелет: `poler-engine [PATH] [РЕЖИМ] [ФЛАГИ]`. `PATH` — файл, каталог
или репозиторий; в веб-режимах — URL. Большинство флагов-режимов
конфликтуют друг с другом (clap сам разрулит).

---

## 1. Резонансный поиск (основной режим)

```bash
poler-engine ~/corpus -q "Алексей" --format ai-json
poler-engine ~/corpus -q "Нокс" --resonance-mode poler --kappa 0.9
```

| Флаг | Тип | Значение |
|---|---|---|
| `-q, --query` | текст | запрос (взвешенные термы) |
| `-t, --top` | usize | сколько результатов |
| `-w, --window` | usize | окно контекста в сценах |
| `--k-hop` | usize | глубина K-hop по графу сущностей |
| `--metric` | строка | метрика сортировки |
| `--format` | enum | `ai-json` \| `md` \| `simple` |
| `--pii` | enum | `off` \| `mask` — zero-copy очистка персональных данных |
| `--resonance-mode` | enum | `hits` \| `field` \| `psi` \| `poler` (THEORY.md §3.2) |
| `--phi` | f32 | показатель объёма в ε-плотности |
| `--kappa` | f32 | удержание IIR-резонанса R(t) |
| `--local-stats` | flag | считать статистики только по локальному корпусу |
| `--extensions` | список | фильтр расширений файлов |
| `--max-file-mb` | u64 | потолок размера файла |
| `--max-scope` / `--max-relations` / `--max-graph-triples` | usize | лимиты парсера/графа |
| `--hidden` | flag | включить скрытые файлы |

Параметры циклов: `--psi-eta/--psi-gamma/--psi-rho/--psi-depth` (Ψ-поле),
`--poler-eta/--poler-gamma/--poler-mix/--poler-dissipator` (POLER-цикл).

## 2. Watcher

```bash
poler-engine ~/corpus -q "деплой" --watch --interval-secs 30 --diff
```

`--watch` — инкрементальный перепрогон при изменениях (WatchState:
VocabArena + PostingsStore переиспользуются); `--interval-secs`; `--diff`
— только изменения с прошлого прогона.

## 3. Точный поиск — слой 0

```bash
poler-engine src --grep "fn main" -A 3 --grep-json
poler-engine . --grep "TODO" --grep-regex --grep-i --grep-count
```

| Флаг | Аналог grep | Смысл |
|---|---|---|
| `--grep PATTERN` | — | ВСЕ совпадения, гарантия полноты, без индекса |
| `--grep-regex` | -E | регулярное выражение |
| `--grep-i` | -i | регистронезависимость (Unicode-fold) |
| `--grep-after N` / `--grep-before N` | -A / -B | контекст |
| `--grep-count` | -c | счётчик на файл |
| `--grep-list` / `--grep-list-nonmatching` | -l / -L | пути с/без совпадений |
| `--grep-max-count N` | -m | потолок совпадений |
| `--grep-hidden` | — | включая скрытые |
| `--grep-json` | — | byte-offsets для агента |

Exit-коды как у grep: 0 — найдено, 1 — пусто, 2 — ошибка.

## 4. RAG-чанки — слой B

```bash
poler-engine document.md --chunk --chunk-size 384 --chunk-json
```

`--chunk` (секция→абзац→предложение, целостность предложений),
`--chunk-size`, `--chunk-overlap`, `--chunk-json` (byte-range якоря).

## 5. Суверенный ML (v2.0)

```bash
# самотест формата весов
poler-engine --pqw-selftest

# плотный семантический поиск: нативные эмбеддинги BGE-M3 из .pqw
poler-engine ~/corpus --semantic dense --model models/bge-m3.pqw -q "запрос"
poler-engine ~/corpus --semantic dense --model models/bge-m3.pqw \
    --semantic-corpus ~/lor --semantic-limit 100 --semantic-max-chunks 10000

# локальный LLM
poler-engine --llm local --model models/chatglm3-6b.pqw

# NER zero-shot
poler-engine ~/corpus --ner gliner --model models/gliner.pqw \
    --ner-labels "человек,организация,место"

# корпус для контекста LLM
--corpus PATH
```

Флаги: `--model` (путь .pqw), `--semantic` (режим), `--semantic-corpus`
(живой поиск: чанки → нативные эмбеддинги → косинус; rayon-параллельно),
`--semantic-limit`, `--semantic-max-chunks`, `--llm` (`local`), `--ner`
(`gliner`), `--ner-labels`, `--pqw-selftest`, `--corpus`.

Конвертация моделей — scripts/convert_*.py (см. INSTALL.md).

## 6. Impact-анализ (AIDDE)

```bash
poler-engine ./src --impact "run_gateway" --impact-depth 3 --impact-cache db
```

`--impact СИМВОЛ` — upstream/downstream паспорт (call graph),
`--impact-depth`, `--impact-cache` (кэш БД), `--impact-reuse`.

## 7. Веб

```bash
poler-engine https://example.com --web                # рендер CDP → поиск
poler-engine https://seed.com --crawl --crawl-depth 2 --crawl-max 25
poler-engine --web-search "запрос"                    # по локальному индексу
poler-engine --semantic-expand "запрос"               # диагностика моста ru↔en
poler-engine --web-stats                              # JSON-статистика индекса
poler-engine --browser-index https://page             # одна страница в индекс
poler-engine --web-lens 127.0.0.1:8765                # браузерный режим
poler-engine --web-lens-install                        # установка в ежедневный браузер
```

| Флаг | Смысл |
|---|---|
| `--web` | PATH = URL: рендер Chromium CDP перед поиском |
| `--cdp-port` (9222), `--web-wait-ms` (1200) | параметры CDP |
| `--crawl` | обход от seed: robots.txt, sitemap, SimHash-дедуп, PageRank |
| `--crawl-depth` (2), `--crawl-max` (25), `--crawl-delay-ms` (1000), `--crawl-page-timeout-ms` (45000) | политика обхода |
| `--web-db` | путь к web-index.db (по умолчанию ~/.local/share/poler-engine/) |
| `--cross-site` | межсайтовый обход |
| `--web-search` | поиск по индексу + авто-расширение Semantic Bridge |

## 8. Интерфейсы

```bash
poler-engine --shell                # REPL + Tab-completion
poler-engine --tui                  # TUI: chat | notes | sources
poler-engine --gateway              # Terminal Gateway (sandbox-контур)
poler-engine --gateway --dangerously-allow-all   # аварийный люк host-контура
poler-engine --mcp                  # MCP-сервер stdio (JSON-RPC)
poler-engine --mcp-http 127.0.0.1:8765 --mcp-token X   # MCP HTTP + Bearer
poler-engine --license              # статус EULA
```

MCP-инструменты: `poler_web_search`, `poler_crawl`, `poler_fetch`,
`poler_search`, `poler_grep`, `poler_chunk`, `poler_box_exec`,
`poler_box_status`.

## 9. Бенчмарк

```bash
poler-engine --benchmark --benchmark-json report.json
```

6 контуров: Teddy-предфильтр · grep+parity (vs ripgrep/grep) ·
BM25-golden · чанкер · vectors · compression/RSS + latency (мс) и RAM
(VmHWM/VmRSS).

## 10. Удалённое в v2.0 (не возвращать)

`--google-*`, `--nlm-*`, `--auth-ui`, `--import-browser-session`,
`--license-import` — Google/NLM-интеграции вырезаны (суверенный стек,
SKILL.md). `--web`/`--crawl`/`--web-search`/`--mcp*`/`--gateway` —
сохранены: CDP-краулинг и публичные REST — не облачная зависимость.
