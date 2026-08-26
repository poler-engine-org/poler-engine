# FUTURE_ROADMAP — «Превзойти Google»

> **Статус: ЗАПИСАНО НА БУДУЩЕЕ. Срок реализации НЕ ОПРЕДЕЛЁН.**
> Дата фиксации цели: 2026-08-25 (сессия poler-engine v0.10.0).
> Формулировка владельца: *«цель превзойти гугл, максимальное обилие сервисов
> помимо ютуба»*. Это не задача следующего релиза — это направление движения
> проекта на годы. Отсюда берём приоритеты, когда появляется свободный ресурс.

---

## 1. Декларация цели

poler-engine развивается не как «ещё один grep», а как **суверенный стек
поиска и сервисов**, не зависящий от чужих API, квот и ToS. Конечная цель —
по богатству сервисов превзойти Google-экосистему (Search, YouTube,
Рекомендации, Карты контента), оставаясь при этом запускаемым на обычном
домашнем ПК под Linux. Каждый релиз движка — кирпич в этом направлении:

| Уже есть в poler-engine (v0.11.0) | Соответствие «большому Google» |
|---|---|
| --web-search, WebRank (BM25+PageRank) | Google Search (ядро поиска) |
| --crawl, robots.txt, sitemap, SimHash | Googlebot (обход) |
| Фразовый поиск с позиционным индексом | Точные цитаты `"..."` в Google |
| MCP-сервер (4 инструмента) | «Поиск как API» для любого LLM-агента |
| ε/R/сцены, K-hop графы, Cross-Universe | Knowledge Graph + «со-прос-граф» |
| Стемминг укр/рос | Морфология (в упрощённом виде) |

## 2. Донорские технологии YouTube-архитектуры (украдено и записано)

Разбор «как YouTube тянет 2.5 млрд пользователей без GPU» — референс для
будущих витков. Всё ниже — открытые исходники, которые можно пиздить в
движок по мере надобности (политика проекта: «пиздим технологии и пообольше,
потом дорабатываем если хреновые»):

1. **TensorFlow Recommenders (TFRS)** — github.com/tensorflow/recommenders
   Двухбашенная архитектура (Two-Tower DNN: башня пользователя + башня
   контента) для мгновенного отбора кандидатов из миллиардов. Аналог в
   poler-engine: Pass 1 (быстрый scatter по postings) / Pass 2 (глубокий
   WebRank по лидерам) — расширить до «башен» эмбеддингов на CPU.
2. **ScaNN (Scalable Nearest Neighbors)** —
   github.com/google-research/google-research/tree/master/scann
   Чистый C++ с AVX-512 SIMD: поиск среди сотен миллионов векторов за
   1–2 мс НА CPU, 15–30 МБ RAM, 0% GPU. Это путь к векторному слою
   poler-engine БЕЗ видеокарт: квантование int8 + SIMD-дистанции.
3. **MediaPipe** — github.com/google-mediapipe
   Потоковый анализ видео/кадров на слабых машинах. Кандидат на
   «мультимедийный» виток движка (кадры → сцены → тройки).
4. **Whitepapers (фундамент, читать перед реализацией):**
   * «Deep Neural Networks for YouTube Recommendations» — разделение
     Candidate Generation / Deep Ranking (у poler-engine та же философия
     двухпроходности);
   * «Google's Custom Video (Argos) ASIC» — VCU-чип: компрессия/анализ
     потоков в 20–33 раза эффективнее GPU. На ПК недостижимо — но протокол
     «анализ на лету при декодировании» переносим и в программный слой.
5. **TPU-подход (int8/bfloat16 матричные умножители)** — на CPU
   эмулируется квантованием + SIMD: см. ScaNN. Обучать с нуля на ПК
   нельзя, забирать готовые сжатые веса (int8/GGUF/ONNX) — можно.

## 3. Честные физические границы домашнего ПК (не игнорировать!)

Законы кремния не обмануть — план обязан учитывать 4 предела:

1. **RAM-bandwidth**: сервер 1000–3000 ГБ/с (HBM3/8-канал DDR5) против
   40–70 ГБ/с на 2-канальном десктопе → индексы обязаны быть компактными
   (varint-delta, WITHOUT ROWID, SimHash u64 вместо текста).
2. **Дисковый IOPS**: сотни тысяч мелких файлов убивают даже NVMe →
   всё в один SQLite/mmap-слой (так и сделано: web-index.db).
3. **Обучение vs инференс**: обучение тяжёлых моделей на CPU — месяцы.
   На ПК — только инференс готовых сжатых весов. Обучение — облако.
4. **Объём RAM 16–32 ГБ**: держать терабайты сырого текста нельзя →
   стриминг, B-tree, бинарные кучи, mmap-окна.

Вывод: инфраструктура poler-engine (SQLite-индекс, varint-кодеки,
двухпроходность, mmap) уже спроектирована под эти пределы — продолжать
в том же духе.

## 4. Что можно делать СЕЙЧАС vs что отложено

| Горизонт | Виток | Донор |
|---|---|---|
| сейчас | фразовый поиск, стемминг, MCP, автономный краул | Lucene/Snowball/MCP |
| скоро | lexical-векторный гибрид на CPU (ScaNN-подход: int8+SIMD), рекомендации «похожие страницы» (Two-Tower на postings-статистиках) | ScaNN, TFRS |
| потом | мультимедиа-конвейер (кадры видео → сцены), распределённый обход на нескольких машинах | MediaPipe, Mercator |
| не на ПК | обучение моделей с нуля, кастомные ASIC | — (облако/аренда) |

## 5. Правила движения к цели

1. Каждый релиз закрывает ровно ОДНУ «хреновую» часть до конца, с живым
   полевым тестом и регрессионным тестом в suite.
2. Никаких зависимостей «на вырост» — только то, что работает в этот релиз.
3. Большие донорские идеи (ScaNN/Two-Tower/MediaPipe) втягиваются только
   когда им есть чем управляться в текущем корпусе.
4. Границы честности фиксируются в README (пример: аутентификационные
   стены notebook.google.com — это граница, а не сбой).


---

## 6. v0.15+: poler-shell (TUI/REPL) и Unified VCS & Data Mesh

> Зафиксировано 2026-08-26 (post-v0.14.0). Инициатива владельца: обернуть
> движок в терминал для человеческого удобства + нативная интеграция со
> всеми VCS/датасет-платформами (GitHub/GitLab/Gitea/Git LFS/DVC/Hugging
> Face Hub/Oxen/ParamLake/HugeSCM/Lit) — Unified Code & Data Mesh.

### 6.1. poler-shell — интерактивный терминал (v0.15.0)

Когда у движка 15+ режимов (поиск, AIDDE, веб-краулинг, NotebookLM, Google
Drive, фразы, графы, синк), человеку неудобно каждый раз вбивать длинные
флаги `--format md --nlm-chat --top 5` или вспоминать UUID ноутбуков.

**Два уровня интерфейса:**

| Уровень | Библиотека | Что даёт | Аудитория |
|---|---|---|---|
| **TUI Dashboard** | `ratatui` + `crossterm` | Левая панель — живой список 87 ноутбуков NLM + локальные репозитории (с эмодзи 🌌 Касіопея, 📈 Бухгалтерия доверия). Правая верхняя — поле ввода поиска/чата с автодополнением Tab. Правая нижняя — выдача с подсветкой синтаксиса, значениями ε/R и K-hop деревом как интерактивным деревом. | Человек-владелец |
| **REPL `poler>`** | `rustyline` | Быстрый командный режим без перезапуска процесса: `poler> search "Касіопея Astra-Nic"` / `poler> nlm ask "Параметры Планковской геодезической"` / `poler> crawl https://rust-lang.org` / `poler> impact dissect_packet --depth 3`. Tab-Completion по командам+флагам, история стрелочками ↑/↓. | Скриптовый человек+скрипт-обёртки |

**Преимущества**: постоянно открытая `WebIndex` + `NlmSession` → As-You-Type
Search за 1 ms при вводе; UUID ноутбуков не нужно помнить — выбор из списка
стрелочками; контекст команд сохраняется в рамках сессии.

**Архитектурные последствия**: текущий `main.rs` — «запустили, отдали,
вышли». v0.15 введёт `Shell` стейт-машину поверх существующих
`poler_engine::*` функций (без переделки ядра — `ratatui` только UI слой).

### 6.2. Unified VCS & Data Mesh (v0.16.0+)

Превращение poler-engine из локального инструмента в **Универсальную Сеть
Кода и Данных** — единый пульт, нативно работающий с любыми репозиториями:

```
              ┌─────────────────────────────────────────┐
              │   POLER-ENGINE UNIFIED DATA MESH        │
              └────────────────────┬────────────────────┘
                                   │
   ┌─────────────────┬─────────────┴─────────────┬─────────────────┐
   ▼                 ▼                           ▼                 ▼
┌──────────┐  ┌──────────────┐          ┌──────────────┐    ┌──────────────┐
│КОД VCS   │  │ДАННЫЕ        │          │ИИ/МОДЕЛИ     │    │ЗНАНИЯ/ОБЛАКО │
│• gix     │  │• Git LFS     │          │• HF Hub      │    │• NotebookLM  │
│• GitHub  │  │• DVC         │          │  (Models/DS) │    │• Google Drive│
│• GitLab  │  │• Oxen.ai     │          │• ParamLake   │    │• Gmail       │
│• Gitea   │  │• HugeSCM(Ant)│          │• ONNX/GGUF   │    │• Web Crawler │
│• Lit(Rust│  │              │          │              │    │              │
│ VCS)     │  │              │          │              │    │              │
└──────────┘  └──────────────┘          └──────────────┘    └──────────────┘
```

**Адаптер-модель** (по образцу `google/` модуля v0.12–v0.13): каждый VCS —
отдельный `src/vcs/<name>.rs` с тривиальным трейтом `VcsAdapter`:

```rust
trait VcsAdapter {
    fn list_repos(&self) -> Result<Vec<RepoId>, String>;
    fn list_commits(&self, repo: &RepoId) -> Result<Vec<Commit>, String>;
    fn list_issues(&self, repo: &RepoId) -> Result<Vec<Issue>, String>;
    fn fetch_blob(&self, repo: &RepoId, oid: &str) -> Result<Vec<u8>, String>;
    fn url_scheme(&self) -> &str;  // "gh://", "gl://", "lfs://", "hf://", "ox://"
}
```

Каждый VCS-объект (коммит, issue, PR, файл, датасет, модель) становится
страницей в `web-index.db` по своей URL-схеме: `gh://user/repo/commit/<sha>`,
`hf://datasets/<owner>/<name>`, `lfs://<repo>/<path>`, `ox://<repo>/<rev>/<path>`.

**Сквозной запрос**:
```bash
poler-engine search "термогеодезическая функция" --all
# → hit 1: nlm://notebook/704f.../note/note-1            (NotebookLM)
# → hit 2: gh://user/repo/commit/a1b2c3d4                (GitHub commit)
# → hit 3: hf://models/owner/model-x                     (Hugging Face model card)
# → hit 4: https://rust-lang.org/...                      (проползенный веб)
```

**Работа с гигантскими монорепами без скачивания** (HugeSCM / LFS): движок
парсит AST и строит AIDDE Call Graph по удалённым репозиториям **через
API**, не забивая локальный диск сотнями гигабайт. Git LFS `.gitattributes`
+ pointer-файлы парсятся на лету для resolve `lfs://` URL без full fetch.

**Донорские технологии** (что заимствуем и улучшаем):

| Донор | Что берём | Куда легло |
|---|---|---|
| `gh` CLI (GitHub) | REST+GraphQL API (commits/issues/PR/codeowners), Actions artefacts | `src/vcs/github.rs` |
| `glab` CLI (GitLab) | REST API v4, merge requests, pipelines | `src/vcs/gitlab.rs` |
| `tea` CLI (Gitea) | REST API (forgejo-compatible) | `src/vcs/gitea.rs` |
| `gix` crate | Pure-Rust git: коммиты/trees/blobs без git-CLI | `src/vcs/local.rs` |
| `git-lfs` pointer protocol | `version https://git-lfs/...` + oid:size: | `src/vcs/lfs.rs` |
| `dvc` `.dvc` files | Outs files + remote storage (S3/Azure/SSH) | `src/vcs/dvc.rs` |
| `huggingface_hub` API | `/api/models`, `/api/datasets`, model cards | `src/vcs/hf.rs` |
| `oxen` CLI / SDK | Oxen.ai remote data versioning | `src/vcs/oxen.rs` |
| `hugescm` (Ant Group) | China-scale monorepo, server-side resolve | `src/vcs/hugescm.rs` |
| `lit` (Rust VCS) | Pure-Rust SCM alternative | `src/vcs/lit.rs` |

### 6.3. Приоритеты в этом направлении

1. **v0.15.0 — poler-shell**: TUI+REPL поверх существующих режимов.
   Зависимости: `ratatui`, `crossterm`, `rustyline` (всё mature, ноль
   новых рисков). Никаких изменений в ядре движка — только UI слой. ✅ shipped
2. **v0.15.1 — полер-шелл финализация**: Tab-completion через rustyline Helper
   + нативные `crawl`/`impact` в REPL. ✅ shipped
3. **v0.16.0 — Unified VCS & Data Mesh**: нативные адаптеры GitHub/GitLab/Gitea
   (REST через `ureq`) + Pure-Rust git через `gix` crate. VCS-страницы в
   web-index.db (`gh://`, `gl://`, `gt://`, `gix://`). Команды `poler> gh/gl/gt/gix`
   + `sync vcs`. ✅ shipped (2026-08-26)
4. **v0.17.0 — gix clone + LFS pointer resolve**: feature-флаги
   `blocking-network-client` для синхронного `gix clone` (без `git` CLI);
   Git LFS pointer-файлы `version https://git-lfs/...` — Pure-Rust клиент
   (detect + batch API + fetch в `.git/lfs/objects/`). Плюс TUI Redesign:
   4-панельный дашборд, мышь, Notes/Sources CRUD, Help 2.0. ✅ shipped
   (v0.17.0, 486 тестов; бинарник 12 МБ)
5. **v0.17.1–v0.17.3 — Companion Bridge**: официальный Pre-GA NotebookLM
   Enterprise API (Discovery Engine `v1alpha`) как auxiliary I/O gateway
   рядом с batchexecute. M1 skeleton ✅ / M2 реальные вызовы (9 операций,
   Bearer cloud-platform) ✅ / M3 HybridProvider routing + fallback ✅ /
   M4 TUI Enter-handler ✅ — всё в v0.17.3 (521 тест). M5 CLI-подкоманды
   (`nlm upload`, `nlm aoview`, `nlm batch-add`) → v0.18.0.
6. **v0.18.0 — Hugging Face Hub**: model cards + datasets API → влитие в
   web-index.db как `hf://` URL-схема + M5 Companion Bridge CLI + фикс
   скролла TUI.
7. **v0.19.0 — DVC + Oxen**: data-versioning pointer files, remote storage
   resolve.
8. **v0.20.0+ — HugeSCM/Lit/ParamLake**: новые адаптеры для China-scale
   монореп и AI-model versioning.

### 6.4. Архитектурное правило для v0.16+

**Ноль новых зависимостей в `web-index.db` схеме** — VCS-страницы используют
те же `WebDoc` + `links` + `content_hash` + `positions` + PageRank, что
веб и NLM. URL-схема — единственное отличие (`gh://` вместо `https://`,
`hf://` вместо `nlm://`). Все адаптеры — source-генераторы страниц, ядро
поиска не трогается.

Это сохраняет invariant v0.14.0: один `--web-search` пробивает ВСЕ юниверсы
(NLM + веб + локальный код + GitHub + HuggingFace + …) с единой PageRank
топологией.

## 7. Горизонт v0.18+: Zero-Storage Streaming Archives (SA1–SA7)

Зафиксировано в v0.17.3 (`docs/future-streaming-archives.md`, ~16 КБ):
петабайтные архивы интернета (Common Crawl `.tar.zst`, Hugging Face `.zip`)
читаются **потоком через HTTP Range без скачивания на диск** — десятки МБ RAM
на корпуса размером с интернет. Не замена кроулингу v0.9.0, а второй вход
в тот же web-index.db.

### 7.1. Математика (переиспользует существующие модули)

| Механизм | Модуль-донор | Что добавляем |
|---|---|---|
| Топологическая адресация zip central-directory | — | O(δ) ≈ 64 КБ для 50 ГБ архива |
| ε(W_k) информационная плотность | `resonance/epsilon.rs` | окно по потоку, а не по файлу |
| IIR резонанс R[n] | `resonance/iir_filter.rs` | готов, O(1) памяти |
| SimHash-дедуп | `web/simhash.rs` | + Bloom-фильтр (m=2²⁰, k=7) |
| POLER[Ψ] importance sampling | `psi.rs`, `poler.rs` | P(d→batch) ∝ exp(λ₁·ψ + λ₂·H − λ₃·Redundancy) |
| Потоковый конвейер | `streaming.rs` | zero-copy FileTokens поверх HTTP Range |

### 7.2. Milestones

- **SA1** — HTTP Range-транспорт + zip central-directory reader (end-of-archive
  seek, δ-зона);
- **SA2** — tar.zst streaming (последовательный, без адресации);
- **SA3** — streaming ε + IIR + SimHash/Bloom поверх потока;
- **SA4** — POLER[Ψ] importance sampling → обучающие батчи (4 выхода:
  LLM-тренинг / RAG / TUI discovery / параллельный поиск);
- **SA5** — CLI `stream-archive index|list|extract` + `stream-search`;
- **SA6** — rayon par_iter по N архивам параллельно;
- **SA7** — инвариант: web-index.db не меняется, `sa://` URL-схема.

### 7.3. Правила

- Никаких TLS-стеков в Rust-коде — HTTP через существующий транспортный
  слой (CDP/ureq);
- PII-маскирование (`tokenizer/pii.rs`) включено до попадания в батчи;
- Честность перед лимитами: если сервер не поддерживает Range — отказ, а не
  full download.
