# POLER-Engine

**AI-Native Topographical, Resonant and Graph Search Engine** — поисково-аналитический
движок на Rust, спроектированный для вытеснения `grep`/`ripgrep` и слепого векторного
RAG из архитектуры LLM-агентов.

```
poler-engine ~/book -q "нокс" --format ai-json | jq '.anchors[0].k_hop_relations'
```

---

## Проблема: почему grep и RAG больше не достаточны

| # | Проблема | Симптом | Механизм POLER |
|---|----------|---------|----------------|
| 1 | **Graph Blindness** | grep находит изолированную строку, модель не понимает, в каком скоупе она находится | Возвращается **полный логический скоуп** (функция/класс целиком, законченная сцена) + **K-hop подграф связей** |
| 2 | **Cosine Collapse / Negation Blindness** | «Система ОБЯЗАНА отключиться» ≈ «Система НЕ ДОЛЖНА отключаться» при cosine > 0.94 | **Exact Lexical Anchors**: фразовый поиск по токенам + маркеры отрицаний с весом 2.0 в ε |
| 3 | **Chunk Fragmentation** | Нарезка по 500 токенов рвёт причинно-следственные связи | **Semantic Boundary Chunking**: границы окон = заголовки сцен / границы функций |
| 4 | **BM25 / TF-IDF Fail** | Редкий токен считается «важным», а суть выражена базовыми словами | Формула информационной плотности **ε** на локальной энтропии |
| 5 | **Temporal Blindness** | Устаревший код смешивается с актуальным, эпохи T-24 и T-0 в одной куче | **Temporal Metric Tagging**: теги `Т-23` на сценах, узлах графа и фильтр `--metric` |

## Математический аппарат

### A. Информационная плотность ε(W_t)

```
ε(W_t) = κ · (1 + ln(1 + count(kw))) ·
         Σ_{w ∈ Unique(W_t) \ {kw}} (ln(N_total) − ln(freq(w)))² +
         Σ_{w ∈ W_t} Bonus_semantic(w)
```

* `N_total` — объём токенов корпуса (или файла при `--local-stats`);
* `freq(w)` — глобальная частота токена;
* `Bonus_semantic` — Ахо-Корасик словарь критических маркеров: отрицания (2.0),
  обязанность (1.5), критичность (1.5), угроза (1.2), код-маркеры (`unsafe`,
  `deprecated`, `panic!`), метрики;
* `κ` — калибровочный масштаб (`--kappa`).

### B. Линейный рекурсивный фильтр резонанса (IIR, O(N))

```
R_t = ε_t + φ · R_{t-1},    φ ∈ [0.75, 0.90]
```

Два режима (`--resonance-mode`):

* **hits** (по умолчанию) — IIR по последовательности ε совпадений в документе:
  повторные упоминания накапливают резонанс;
* **field** — ε вычисляется инкрементальным скользящим окном на **каждой**
  позиции документа (амортизированно O(1) на сдвиг), IIR прогоняется по всем
  позициям, сэмплируется в точках совпадений — строго O(N) один проход.

### C. K-hop подграф сущностей

```
SubGraph(E₀, k) = { (u, predicate, v) | dist(E₀, u) < k ∧ (u —predicate→ v) ∈ G }
```

BFS в **обе стороны** (outgoing + incoming), узлы сливаются case-insensitive,
каждый узел несёт temporal-слой. Тройки извлекаются из текста (SVO-эвристики:
`Нокс —вонзила_когти→ Солнечное сплетение`), из метаданных сцены
(`Нокс —появляется_в→ Глава 36`) и из кода (`process_data —вызывает→ compute`).

## Архитектура

```
files ──► [Проход A, rayon] mmap → PII-mask → токенизация → inverted index
              │                    (глобальные частоты токенов корпуса)
              ▼
        merge global stats (N_total, freq)
              ▼
        [Проход B, rayon] для hit-файлов:
              HitRecord (лёгкий): окно W_t → ε (+semantic bonus)
                                          → IIR R_t
                                          → локализация сцены (без клонов)
              Уникальные сцены: ScenePayload (сцена+тройки) один раз
              ▼
        EntityGraph (petgraph DiGraph) ── K-hop BFS от корневой сущности
              ▼
        temporal-фильтр → сортировка по R → top_n → материализация ContextAnchor
```

Ключевые решения производительности:

* **memmap2** — zero-copy чтение (валидный UTF-8 + нет PII → ноль копий файла);
* **PII-маскирование** возвращает `Cow::Borrowed` на чистом тексте; замаскированный
  текст малых hit-файлов кэшируется между проходами (нет повторного regex-скана);
* **HitRecord / ScenePayload** — тяжёлые payload (клон сцены, разбор троек)
  материализуются один раз на уникальную сцену и только для top-N якорей;
* **aho-corasick** — мультитокен-поиск семантических маркеров (LeftmostLongest);
* лексический сканер кода понимает строки, char-литералы (`'{'`!), raw-строки Rust
  (`r#"…"#`), комментарии и template-литералы JS с `${}` — скобки в них не считаются;
* release-профиль: `opt-level=3`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip`.

## Происхождение алгоритмов: анализ исходников базовых инструментов

Движок построен на техниках, извлечённых из исходных репозиториев
(см. `upstream/` в рабочей области):

| Источник | Файл/модуль | Техника | Внедрение в poler-engine |
|---|---|---|---|
| GNU grep 3.11 | `src/kwset.c` | Commentz-Walter: BM-сдвиги + AC-автомат, выбор исполнителя per-query | Литеральный SIMD-предфильтр `--fast`: aho-corasick по байтам **до** токенизации, файлы без ASCII-литерала отбраковываются (2.3× на разреженных корпусах) |
| ripgrep | `crates/ignore/src/walk.rs` | `WalkBuilder`: параллельный обход с .gitignore/.ignore/hidden | Обход каталогов — сам крейт `ignore` (код BurntSushi): `git_ignore`, `git_global`, `git_exclude`, `require_git(false)`, флаг `--hidden` |
| ripgrep | `regex-automata` prefilter | literal prefilter: regex не запускается без якорного байта | PiiCleaner: отсутствие `@`/цифр (проверка `memchr`) пропускает email/IP/phone/card regex |
| super-z-skills | `skills/_orchestrator/scripts/memory_graph.py` | SQLite-схема entities/relations с UNIQUE-констрейнтами | `--graph-export dump.sql`: дамп графа сущностей в этой же схеме, `sqlite3 graph.db < dump.sql` |
| GNU grep | `src/grep.c` | grep-совместимые коды выхода | 0/1/2 (совпадения/нет/ошибка) |

Отличие принципиальное: grep после нахождения строки останавливается —
poler-engine разворачивает каждое совпадение в полный аналитический
контекст (скоуп + ε + резонанс + K-hop подграф).

## Установка и сборка

```bash
cargo build --release          # бинарник: target/release/poler-engine
cargo test                     # 85 тестов (unit + integration)
cargo run --release --example bench
```

POSIX-совместимо: Linux/macOS/BSD, только чистый Rust без C-зависимостей.

## CLI

```
poler-engine [OPTIONS] --query <QUERY> <PATH>

-q, --query <QUERY>         слово или фраза (в кавычках: "не должна")
-t, --top <N>               топ-результатов [default: 10]
    --format <FORMAT>       ai-json | md | simple [default: ai-json]
    --phi <PHI>             затухание IIR-резонанса [default: 0.85]
    --kappa <K>             масштаб ε [default: 1.0]
-w, --window <RADIUS>       радиус токенного окна [default: 40]
-k, --k-hop <DEPTH>         глубина обхода графа [default: 2]
    --metric <TAG>          временной фильтр (например Т-23)
    --pii <MODE>            off | mask [default: mask]
    --resonance-mode <MODE> hits | field [default: hits]
    --local-stats           ε по статистикам файла вместо корпуса
    --extensions <LIST>     сканируемые расширения
    --max-file-size <MB>    [default: 32]
    --max-scope <BYTES>     потолок enclosing_scope [default: 16384]
    --max-relations <N>     потолок K-hop связей на якорь [default: 64]
    --fast                   литеральный SIMD-предфильтр до токенизации (GNU grep kwset-техника)
    --hidden                 показывать скрытые файлы (аналог rg --hidden)
    --graph-export <PATH>    дамп графа сущностей в SQL (схема super-z memory_graph)
    --threads <N>           потоки rayon [default: все ядра]
-v, --verbose               статистика прогона в stderr
```

Коды выхода (grep-совместимые): `0` — есть совпадения, `1` — нет, `2` — ошибка.

## Выходной контракт (Context Anchor)

```json
{
  "query": "нокс",
  "total_hits": 3,
  "anchors": [
    {
      "file": "/path/to/chapter_36.md",
      "token": "нокс",
      "epsilon": 2859.53,
      "resonance": 6185.5,
      "scene": {
        "chapter": "Глава 36. Инертный",
        "temporal_metric": "Метрика: Т-23",
        "location": "Локация: Разлом Каньона",
        "subjects": ["Субъекты: Мальчик (гибрид), Соболь (Нокс)"],
        "enclosing_scope": "# Глава 36. Инертный ..."
      },
      "k_hop_relations": [
        ["Нокс", "вонзила_когти", "Солнечное сплетение"],
        ["Шунт", "сбрасывает_тепло", "1300°C"]
      ]
    }
  ]
}
```

`total_hits` — все совпадения до усечения по `--top`; `k_hop_relations` —
подграф связей корневой сущности (первый токен запроса).

## Производительность

Синтетический корпус: 400 файлов × 25 абзацев, 3.14 МБ, 264 400 токенов,
10 400 совпадений, 2 потока (vCPU):

| Метрика | Значение |
|---|---|
| Полный прогон (hits-режим) | **~500 мс** |
| Field-режим (строго O(N)) | ~480 мс |
| Пропускная способность | ~6.2 МБ/с с полным аналитическим конвейером |
| Файлов/с | ~790 |
| Граф | 462 узла, 1621 ребро |

Для сравнения: ripgrep находит строки в ~100 раз быстрее, но не возвращает
скоупов, метрик, троек и K-hop — это цена полной аналитики на каждое совпадение.

## Структура проекта

```
poler-engine/
├── Cargo.toml                  # clap, rayon, memmap2, petgraph, serde, regex, aho-corasick, walkdir
└── src/
    ├── main.rs                 # CLI: --format [ai-json|md|simple], grep-совместимые коды выхода
    ├── lib.rs                  # двухпроходный параллельный пайплайн, EngineConfig, ScanStats
    ├── tokenizer/
    │   ├── pii.rs              # zero-copy (Cow) маскирование PII
    │   └── inverted_index.rs   # индекс всех токенов, включая отрицания
    ├── parser/
    │   ├── ast_code.rs         # лекс-сканер: brace/indent enclosing scope (Rust/C/JS/Python)
    │   ├── markdown_scenes.rs  # сцены: главы, метрики, локации, субъекты
    │   └── triples.rs          # SVO-тройки + call graph + co-occurrence
    ├── resonance/
    │   ├── epsilon.rs          # ε + Ахо-Корасик маркеры + скользящее окно O(1)
    │   └── iir_filter.rs       # R_t = ε_t + φ·R_{t-1}
    ├── graph/
    │   └── entity_graph.rs     # DiGraph (petgraph), K-hop BFS, temporal-слои
    └── output/
        └── context_anchor.rs   # AI-Ready JSON + рендеры md/simple
```

## Известные ограничения (честно)

* SVO-извлечение троек — эвристическое (морфология русского языка без
  полного парсера); ориентировано на воспроизводимость и полноту связей, не
  на лингвистическую точность;
* `--fast` меняет семантику статистики: ε считается по matched-подмножеству
  файлов, а не по всему корпусу (у grep статистики нет вовсе);
* кириллические запросы не проходят литеральный предфильтр (регистр меняет
  UTF-8 байты) — используется полный путь;
* в field-режиме семантический бонус не начисляется (он определён на уровне
  совпадений);
* память: индексы hit-файлов и их кэшированный текст (до 1 МБ на файл)
  удерживаются до конца прогона; для сверхбольших репозиториев используйте
  `--local-stats` и послабление `--max-file-size`.

## Тестирование

91 тест: 70 unit (математика ε/IIR, сканер скобок, raw-строки, PII,
разбиение предложений, K-hop, temporal-фильтр) + 21 интеграционный
(воспроизведение контракта спецификации на фикстуре главы 36, call graph,
PII-маскирование, детерминизм, сортировка, режимы резонанса).

```bash
cargo test
cargo clippy --all-targets   # 0 предупреждений
```

## Лицензия

MIT OR Apache-2.0.
