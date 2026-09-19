//! CLI poler-engine: AI-Native Topographical, Resonant and Graph Search Engine.
//!
//! Режимы:
//! * поиск: `poler-engine <PATH> -q <QUERY> [--watch]`
//! * impact-анализ (AIDDE): `poler-engine <PATH> --impact <SYMBOL>`
//! * веб-поиск для AI: `poler-engine --web-search <QUERY>` (по веб-индексу)
//! * краулинг: `poler-engine <URL> --crawl [--crawl-depth N --crawl-max M]`
//!
//! v2.0 (sovereign stack): Google/NotebookLM/Gmail/Drive/OAuth-интеграции
//! удалены. Движок полностью локален; внешний мир — только краулинг через
//! CDP и MCP-инструменты для агентов.
//!
//! Коды выхода (grep-совместимые): 0 — есть совпадения, 1 — совпадений нет,
//! 2 — ошибка.

use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use poler_engine::aidde::{impact_analysis, SymbolTable};
use poler_engine::{
    collect_files, render_markdown, render_simple, Engine, EngineConfig, PiiMode, ResonanceMode,
    CodeLang, DEFAULT_EXTENSIONS, SearchResult,
};

#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
enum Format {
    /// Самодостаточный AI-Ready JSON (Context Anchor).
    AiJson,
    /// Markdown с секциями для человека.
    Md,
    /// Одна строка на совпадение.
    Simple,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
enum PiiArg {
    /// PII-маскирование выключено.
    Off,
    /// Email/телефоны/IP/секреты заменяются маркерами (по умолчанию).
    Mask,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
enum ResonanceArg {
    /// IIR-резонанс по последовательности совпадений (R[n], K=1).
    Hits,
    /// Поле резонанса по всему документу, строго O(N).
    Field,
    /// POLER[Ψ]: уравнение внимания из POLER_Psi_v3.py.
    Psi,
    /// Канонический POLER-цикл из P3_Engine (p3_poler.zig):
    /// p −= η·Π_Λ(D·p + γ·J·p + ∇F), CORDIC-ренормализация.
    Poler,
}

#[derive(Copy, Clone, PartialEq, Eq, Debug, ValueEnum)]
enum KnowledgeEmbedderArg {
    /// Без векторного слоя (только BM25/WebRank) — быстрый инжест.
    None,
    /// Детерминированная хэш-проекция 512-d: полный гибридный конвейер
    /// (RaBitQ+HNSW) без модели — инструментальная проекция, НЕ семантика.
    Hash,
    /// Нативный энкодер из --model <path.pqw> (BGE-M3/XLM-R класс).
    Pqw,
}

#[derive(Parser, Debug)]
#[command(
    name = "poler-engine",
    version,
    author = "POLER Engineering Core",
    about = "AI-Native Topographical, Resonant and Graph Search Engine",
    long_about = "Поисково-аналитический движок для LLM-агентов: полный логический скоуп \
                  (сцена/функция целиком), информационная плотность ε, резонанс R(t), \
                  K-hop подграф связей и AIDDE impact-анализ вместо изолированных строк grep."
)]
struct Cli {
    /// Путь к файлу или корню репозитория.
    /// С --web: трактуется как URL (https://...) — рендер через Chromium CDP.
    /// С --crawl: seed-URL для обхода.
    path: Option<PathBuf>,

    /// Рендер веб-страницы через Chromium CDP перед поиском
    /// (патч интерпретирует PATH как URL).
    #[arg(long)]
    web: bool,

    /// Порт Chromium DevTools (с --web) [default: 9222].
    #[arg(long = "cdp-port", default_value_t = 9222)]
    cdp_port: u16,

    /// Пауза после load на дочерние XHR, мс (с --web) [default: 1200].
    #[arg(long = "web-wait-ms", default_value_t = 1200)]
    web_wait_ms: u64,

    /// ВЕБ-ПОИСК: поиск по локальному веб-индексу (краулер --crawl).
    /// v0.21: запрос автоматически расширяется Semantic Bridge (офлайн
    /// сенсор ru↔en) — рус запрос находит англ корпус и обратно.
    #[arg(long = "web-search", conflicts_with_all = ["web", "crawl"])]
    web_search: Option<String>,

    /// SEMANTIC BRIDGE (v0.21): показать кросс-языковое расширение запроса
    /// офлайн-сенсором (кандидаты ru↔en + WHY + веса BM25) — без поиска.
    #[arg(
        long = "semantic-expand",
        value_name = "QUERY",
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "browser_index", "web_lens", "web_lens_install", "grep", "chunk",
            "benchmark"
        ]
    )]
    semantic_expand: Option<String>,

    /// BENCHMARK (v0.21, Задача 4): автоматический бенчмарк-раннер —
    /// Exact Retrieval (POLER grep vs ripgrep/grep, полнота + parity),
    /// Explainable Lexical (BM25/WebRank + Semantic Bridge, golden top-1),
    /// Passage (чанкер vs naive splitter, целостность предложений),
    /// latency (мс) и RAM (VmHWM/VmRSS). Golden-регрессии — в cargo test.
    #[arg(
        long = "benchmark",
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "browser_index", "web_lens", "web_lens_install", "grep", "chunk",
            "semantic_expand"
        ]
    )]
    benchmark: bool,

    /// Сохранить JSON-отчёт бенчмарка в файл (с --benchmark).
    #[arg(long = "benchmark-json", value_name = "PATH", requires = "benchmark")]
    benchmark_json: Option<PathBuf>,

    /// Краулинг: PATH трактуется как seed-URL, страницы индексируются
    /// в веб-индекс (robots.txt, sitemap, SimHash-дедуп, PageRank).
    #[arg(long, requires = "path")]
    crawl: bool,

    /// Путь к базе веб-индекса [default: ~/.local/share/poler-engine/web-index.db].
    #[arg(long = "web-db")]
    web_db: Option<PathBuf>,

    /// Статистика веб-индекса (JSON в stdout).
    #[arg(long = "web-stats")]
    web_stats: bool,

    /// Глубина краулинга от seed [default: 2].
    #[arg(long = "crawl-depth", default_value_t = 2)]
    crawl_depth: usize,

    /// Максимум страниц за один обход [default: 25].
    #[arg(long = "crawl-max", default_value_t = 25)]
    crawl_max: usize,

    /// Минимальная пауза между запросами к одному хосту, мс [default: 1000].
    #[arg(long = "crawl-delay-ms", default_value_t = 1000)]
    crawl_delay_ms: u64,

    /// Пер-страничный таймаут загрузки в краулинге, мс [default: 45000].
    /// JS-тяжёлые сайты (бесконечные XHR/стриминг) не зависят дольше лимита.
    #[arg(long = "crawl-page-timeout-ms", default_value_t = 45_000)]
    crawl_page_timeout_ms: u64,

    /// ИНДЕКСАЦИЯ ОДНОЙ СТРАНИЦЫ: URL → рендер (Chromium CDP) → веб-индекс.
    /// Явная команда пользователя: robots.txt не блокирует (но фиксируется
    /// в notes). После — доступен --web-search по общему индексу.
    #[arg(long = "browser-index", value_name = "URL", conflicts_with_all = ["web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell", "tui", "impact"])]
    browser_index: Option<String>,

    /// БРАУЗЕРНЫЙ РЕЖИМ: материализует WebLens (расширение MV3), запускает
    /// оконный Chromium с уже установленным WebLens и держит MCP-сервер
    /// на 127.0.0.1:8765 (BIND как у --mcp-http, или только порт).
    #[arg(long = "web-lens", value_name = "BIND", num_args = 0..=1, default_missing_value = "127.0.0.1:8765", conflicts_with_all = ["web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell", "tui", "impact", "browser_index", "web_lens_install"])]
    web_lens: Option<String>,

    /// Установить WebLens в ЕЖЕДНЕВНЫЙ браузер: материализует файлы
    /// и печатает шаги «Load unpacked» (chrome://-страницы автоматизировать
    /// нельзя — это защита браузера; управляемый движком браузер ставит
    /// WebLens сам через --web-lens).
    #[arg(long = "web-lens-install", conflicts_with_all = ["web_lens", "mcp_http", "mcp", "shell", "tui", "crawl", "web_search", "browser_index"])]
    web_lens_install: bool,

    // ---------- Native Retrieval: grep-режим + RAG-чанки (v0.20.0) ----------

    /// ТОЧНЫЙ ПОИСК (grep-режим): ВСЕ совпадения PATTERN по файлам
    /// (PATH или текущий каталог), без индекса, гарантия полноты.
    /// Exit-коды как у grep: 0 — найдено, 1 — пусто, 2 — ошибка.
    #[arg(long = "grep", value_name = "PATTERN", conflicts_with_all = ["web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell", "tui", "impact", "browser_index", "web_lens", "web_lens_install", "chunk"])]
    grep: Option<String>,

    /// Регулярное выражение вместо фиксированной строки (grep -E).
    #[arg(long = "grep-regex", requires = "grep")]
    grep_regex: bool,

    /// Регистронезависимость, Unicode-fold (grep -i).
    #[arg(long = "grep-i", requires = "grep")]
    grep_ignore_case: bool,

    /// Строк контекста ПОСЛЕ совпадения (grep -A NUM).
    #[arg(long = "grep-after", requires = "grep", default_value_t = 0)]
    grep_after: usize,

    /// Строк контекста ДО совпадения (grep -B NUM).
    #[arg(long = "grep-before", requires = "grep", default_value_t = 0)]
    grep_before: usize,

    /// Только счётчик совпадений на файл (grep -c).
    #[arg(long = "grep-count", requires = "grep")]
    grep_count: bool,

    /// Только пути файлов с совпадениями (grep -l).
    #[arg(long = "grep-list", requires = "grep")]
    grep_list: bool,

    /// Только пути файлов БЕЗ совпадений (grep -L).
    #[arg(long = "grep-list-nonmatching", requires = "grep")]
    grep_list_nonmatching: bool,

    /// Останов после N совпавших строк на файл (grep -m NUM).
    #[arg(long = "grep-max-count", value_name = "NUM", requires = "grep")]
    grep_max_count: Option<usize>,

    /// Включить скрытые файлы (по умолчанию пропускаются, как ripgrep).
    #[arg(long = "grep-hidden", requires = "grep")]
    grep_hidden: bool,

    /// Машинно-читаемый JSON вместо текста — byte offsets, диапазоны
    /// вхождений, статистика (для ИИ-агента).
    #[arg(long = "grep-json", requires = "grep")]
    grep_json: bool,

    // ---------- v0.28.1: Архивы без распаковки ----------

    /// Скан архивов без распаковки (с --grep): записи zip/tar/tar.gz/
    /// tar.zst/gz/zst внутри PATH читаются напрямую из контейнера
    /// виртуальными файлами «архив::запись». Ничего не пишется на диск.
    #[arg(long = "archives", requires = "grep")]
    archives: bool,

    /// Пароль зашифрованных архивов (ZipCrypto/AES). Вводится прямо
    /// в CLI человеком или ИИ-агентом. Виден в истории shell — для
    /// секретности используйте POLER_ARCHIVE_KEY или TTY-промпт.
    #[arg(long = "archive-password", value_name = "PASS")]
    archive_password: Option<String>,

    /// Лимит несжатой записи архива, МиБ [default: 64] — защита от
    /// zip-бомб: запись свыше лимита пропускается с ошибкой в stats.
    #[arg(long = "archive-max-entry-mb", value_name = "NUM", default_value_t = 64)]
    archive_max_entry_mb: u64,

    /// ЛИСТИНГ АРХИВА: записи контейнера (имя/размеры/шифрование)
    /// без распаковки и без пароля — осмотр перед вскрытием.
    #[arg(long = "archive-list", value_name = "ARCHIVE", conflicts_with_all = ["grep", "chunk", "web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell", "tui", "impact", "browser_index", "web_lens", "web_lens_install"])]
    archive_list: Option<PathBuf>,

    /// JSON-вывод листинга архива (с --archive-list) — для агента.
    #[arg(long = "archive-json", requires = "archive_list")]
    archive_json: bool,

    /// RAG-ЧАНКИ: нарезать документ PATH на фрагменты с якорями
    /// (byte range, номера строк, breadcrumb заголовков) —
    /// passage-уровень для агента вместо чтения документа целиком.
    #[arg(long = "chunk", requires = "path", conflicts_with_all = ["web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell", "tui", "impact", "browser_index", "web_lens", "web_lens_install", "grep"])]
    chunk: bool,

    /// Целевой размер чанка в токенах POLER [default: 384].
    #[arg(long = "chunk-size", requires = "chunk", default_value_t = poler_engine::retrieval::DEFAULT_TARGET_TOKENS)]
    chunk_size: usize,

    /// Перекрытие соседних чанков в токенах [default: 48].
    #[arg(long = "chunk-overlap", requires = "chunk", default_value_t = poler_engine::retrieval::DEFAULT_OVERLAP_TOKENS)]
    chunk_overlap: usize,

    /// Машинно-читаемый JSON вместо текста (для ИИ-агента).
    #[arg(long = "chunk-json", requires = "chunk")]
    chunk_json: bool,

    /// MCP-СЕРВЕР (Model Context Protocol): poler-engine как нативный
    /// инструмент LLM-агентов поверх stdio JSON-RPC.
    /// Инструменты: poler_web_search / poler_crawl / poler_fetch / poler_search /
    /// poler_grep / poler_chunk / poler_box_exec / poler_box_status.
    #[arg(long = "mcp", conflicts_with_all = ["web_search", "crawl", "web_stats", "impact", "mcp_http", "mcp_token"])]
    mcp: bool,

    /// MCP-СЕРВЕР ПО HTTP (Streamable HTTP): тот же набор инструментов,
    /// что и --mcp, но для УДАЛЁННОГО агента — через туннель (например
    /// `cloudflared tunnel --url http://127.0.0.1:8765`). POST / или /mcp,
    /// заголовок Authorization: Bearer <токен>.
    /// BIND = «127.0.0.1:8765» (по умолчанию) или просто порт «8765».
    #[arg(long = "mcp-http", value_name = "BIND", num_args = 0..=1, default_missing_value = "127.0.0.1:8765", conflicts_with_all = ["mcp", "shell", "tui", "web_search", "crawl", "web_stats", "impact"])]
    mcp_http: Option<String>,

    /// Токен доступа для --mcp-http (или env POLER_MCP_TOKEN; без него
    /// генерируется при старте и печатается в stderr).
    #[arg(long = "mcp-token", value_name = "TOKEN", requires = "mcp_http")]
    mcp_token: Option<String>,

    /// M6: ШИФРОПОТОК ЖУРНАЛА операций MCP-сервера (--mcp/--mcp-http)
    /// в Vault .pvt — стриминг логов: каждое JSON-RPC-сообщение дописывается
    /// зашифованной строкой (ts, method, tool, латентность мкс, ok) без
    /// пере-печати файла. Читается как обычный --memory-open (на любом
    /// коммите файл валиден). Фраза: --vault-log-pass | env POLER_VAULT_PASS
    /// | интерактивный вопрос (как у --memory-seal).
    #[arg(long = "vault-log", value_name = "FILE.pvt")]
    vault_log: Option<std::path::PathBuf>,

    /// Парольная фраза для --vault-log (иначе env POLER_VAULT_PASS/stdin).
    #[arg(long = "vault-log-pass", value_name = "PASS", requires = "vault_log")]
    vault_log_pass: Option<String>,

    /// M6: RAM-бюджет резидентного файл-кэша grep (МиБ; 0 = безлимит).
    #[arg(long = "mcp-ram-budget", value_name = "MIB", default_value_t = 48)]
    mcp_ram_budget: usize,

    /// M6: БЕНЧМАРК РЕЗИДЕНТНОСТИ: N итераций poler_grep и
    /// query_poler_knowledge холодный-против-тёплого (p50/p95/p99),
    /// RAM кэша и проверка бюджета <5 мс. Без --path/--knowledge-db
    /// строит временный корпус (самодостаточная верификация M6).
    #[arg(long = "mcp-bench", value_name = "N", num_args = 0..=1, default_missing_value = "200", conflicts_with_all = ["shell", "tui", "mcp", "mcp_http", "web_search", "crawl", "web_stats", "impact", "grep", "chunk", "benchmark", "web_lens", "web_lens_install", "browser_index", "license", "semantic_expand"])]
    mcp_bench: Option<usize>,

    // ---------- poler-shell: интерактивный терминал v0.15.0 ----------

    /// TERMINAL GATEWAY (v0.22.0): единый терминальный шлюз — двойной
    /// контур исполнения (engine-native приоритет + sandboxed host proxy),
    /// конвейеры host↔engine, service/attach управление нижним слоем.
    /// Верхний уровень управления на Linux/macOS.
    #[arg(long, conflicts_with_all = ["shell", "tui", "mcp", "mcp_http", "web_search", "crawl", "web_stats", "impact", "grep", "chunk", "benchmark", "web_lens", "web_lens_install", "browser_index", "license", "semantic_expand"])]
    gateway: bool,

    /// v0.23.0 DANGER OVERRIDE: полностью отключить sandbox Terminal
    /// Gateway. Красный баннер при старте; блокировки и подтверждения
    /// НЕ применяются — вся ответственность за безопасность хоста
    /// ложится на оператора. Требует --gateway.
    #[arg(long = "dangerously-allow-all", requires = "gateway")]
    dangerously_allow_all: bool,

    /// Интерактивный REPL (poler> search/crawl/notes/sync vcs...).
    /// База web-index.db открывается лениво и переиспользуется
    /// между командами (история в ~/.cache/poler-engine/shell-history.txt).
    #[arg(long = "shell", conflicts_with_all = ["tui", "mcp", "web_search", "crawl", "web_stats", "impact"])]
    shell: bool,

    /// TUI Dashboard на ratatui (панели: chat + ввод | notes + sources).
    /// Tab — смена фокуса, Esc — выход. Команды как в --shell.
    #[arg(long = "tui", conflicts_with_all = ["shell", "mcp", "web_search", "crawl", "web_stats", "impact"])]
    tui: bool,

    // ---------- License / EULA (v2.0: статус модели, без гейта) ----------

    /// Статус лицензии и модель распространения (Source-Available EULA).
    /// Локальный поиск и все функции движка — всегда без ограничений;
    /// v2.0 не содержит ключей и гейтов.
    #[arg(long = "license", conflicts_with_all = ["shell", "tui", "mcp", "mcp_http"])]
    license: bool,
    /// Разрешить краулеру переход на другие хосты.
    #[arg(long = "cross-site", default_value_t = false)]
    cross_site: bool,

    /// Поисковый запрос: слово или фраза (в кавычках).
    #[arg(short, long)]
    query: Option<String>,

    // ---------- v2.0 Part E/F: суверенный ML-инференс (.pqw через pqc) ----------

    /// Путь к .pqw-модели (для --semantic dense / --llm local / --ner gliner).
    #[arg(long = "model", value_name = "PQW")]
    model: Option<PathBuf>,

    /// Семантический режим: dense = нативный энкодер из .pqw (через pqc,
    /// не ONNX). Требует --model и -q. С --semantic-corpus — живой поиск
    /// по каталогу/файлу (чанки → эмбеддинги → косинус → топ-N).
    #[arg(long = "semantic", value_name = "MODE")]
    semantic: Option<String>,

    /// Корпус для --semantic dense или --llm quantum (каталог или файл): чанкируются
    /// или насыщают русла квантовой циркуляции J.
    #[arg(long = "corpus", value_name = "PATH")]
    corpus: Option<PathBuf>,

    /// Корпус для --semantic dense (каталог или файл): чанкируются,
    /// эмбеддятся нативным энкодером и ранжируются косинусом.
    #[arg(long = "semantic-corpus", value_name = "PATH", requires = "semantic")]
    semantic_corpus: Option<PathBuf>,

    /// Сколько верхних результатов печатать в --semantic-corpus поиске.
    #[arg(long = "semantic-limit", default_value_t = 5)]
    semantic_limit: usize,

    /// Потолок чанков для --semantic-corpus (равномерная выборка по корпусу).
    #[arg(long = "semantic-max-chunks", default_value_t = 512)]
    semantic_max_chunks: usize,

    /// LLM-генерация (Part F): local = нативный GLM-декодер из .pqw.
    /// remote/auto — мост к серверному GLM (Фаза 12.9, заглушка).
    #[arg(long = "llm", value_name = "MODE")]
    llm: Option<String>,

    /// Извлечение сущностей: gliner = нативная span-голова из .pqw.
    #[arg(long = "ner", value_name = "MODEL")]
    ner: Option<String>,

    /// Метки сущностей для --ner gliner (через запятую; zero-shot).
    /// Для реальных чекпойнтов GLiNER обязательно: «--ner-labels людина,місто».
    #[arg(long = "ner-labels", value_name = "LIST")]
    ner_labels: Option<String>,

    /// Самопроверка суверенного стека: синтетические .pqw-модели →
    /// полный цикл инференса (энкодер + GLiNER + GLM + MoE + Sha256)
    /// без сети, без внешних библиотек.
    #[arg(long = "pqw-selftest")]
    pqw_selftest: bool,

    /// Обратный In-Place компилятор: живая демонстрация переписывания
    /// тритов в .t5q без декомпрессии (STDP + фазовый ротор + commit).
    /// Без аргумента — синтетический поток; с путём — ваш .t5q-файл.
    #[arg(long = "t5q-compile", value_name = "FILE", num_args = 0..=1, default_missing_value = "")]
    t5q_compile: Option<String>,

    /// JIT-контур: обученные веса → машинный код x86_64 (ваги вшиты в
    /// інструкції як immediate) → виконання → Hebb-пластичність →
    /// перекомпіляція. Без аргумента — синтетический слой 32×32.
    #[arg(long = "jit-loop", value_name = "FILE", num_args = 0..=1, default_missing_value = "")]
    jit_loop: Option<String>,

    // ---------- Суверенный Гиппокамп: библиотека POLER → нативный индекс (v0.29) ----------

    /// ИНЖЕСТ БИБЛИОТЕКИ ЗНАНИЙ: PATH = корень POLER_ALL_GENERATED_DOCS
    /// (слои 01…06 распознаются автоматически). Секции → чанки с якорями
    /// и эпистемической разметкой (MVR ×1.5 / первоисточник ×1.0 /
    /// нарратив ×0.7) → полнотекстовый индекс (BM25/WebRank + Semantic
    /// Bridge) + опционально векторный слой (RaBitQ 144 Б/вектор + HNSW).
    #[arg(
        long = "knowledge-ingest",
        value_name = "PATH",
        conflicts_with_all = [
            "web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell",
            "tui", "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_search", "knowledge_stats"
        ]
    )]
    knowledge_ingest: Option<PathBuf>,

    /// Векторный слой инжеста: none (по умолчанию) | hash | pqw.
    /// pqw требует --model <path.pqw> (суверенные веса).
    #[arg(
        long = "knowledge-embedder",
        value_enum,
        default_value_t = KnowledgeEmbedderArg::None,
        requires = "knowledge_ingest"
    )]
    knowledge_embedder: KnowledgeEmbedderArg,

    /// ПОИСК ПО БИБЛИОТЕКЕ ЗНАНИЙ: гибрид BM25/WebRank + векторы (если
    /// индекс построен с векторным слоем), эпистемическая градация и
    /// фильтр достоверности. Хиты несут файл/строки/байты + прямую цитату.
    #[arg(
        long = "knowledge-search",
        value_name = "QUERY",
        conflicts_with_all = [
            "web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell",
            "tui", "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_ingest", "knowledge_stats"
        ]
    )]
    knowledge_search: Option<String>,

    /// Минимальный эпистемический статус хитов (с --knowledge-search):
    /// mvr | source | narrative.
    #[arg(long = "min-provenance", value_name = "LEVEL", requires = "knowledge_search")]
    min_provenance: Option<String>,

    /// Путь к БД знаний [default: ~/.local/share/poler-engine/knowledge.db].
    #[arg(long = "knowledge-db", value_name = "PATH")]
    knowledge_db: Option<PathBuf>,

    /// Статистика индекса знаний (JSON в stdout).
    #[arg(
        long = "knowledge-stats",
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_ingest", "knowledge_search"
        ]
    )]
    knowledge_stats: bool,

    // ---------- Крипто-слой данных: POLER Vault (M4.5 CDL, фича pnd-ffi) ----------

    /// ЗАПЕЧАТАТЬ ДАННЫЕ «НАСМЕРТЬ» В ЗАШИФРОВАННЫЙ КОНТЕЙНЕР:
    /// PATH → PATH.pvt. Крипто-ядро PND v8.2 (CBC, 256-битный ключ из
    /// парольной фразы, ланцюговий MAC + внешний SHA-256). Контейнер
    /// синхронизируется через git/любой транспорт: без ключа — нечитаем,
    /// но проверяем на целостность (--memory-verify). Память O(1):
    /// файлы от 500 КБ до сотен ГБ.
    #[cfg(feature = "pnd-ffi")]
    #[arg(
        long = "memory-seal",
        value_name = "PATH",
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_ingest", "knowledge_search", "knowledge_stats",
            "memory_open", "memory_verify", "memory_info"
        ]
    )]
    memory_seal: Option<PathBuf>,

    /// ВСКРЫТЬ КОНТЕЙНЕР: VAULT.pvt → открытый текст (проверка MAC
    /// и транспортного SHA-256 обязательна; неверный ключ = отказ).
    #[cfg(feature = "pnd-ffi")]
    #[arg(
        long = "memory-open",
        value_name = "VAULT",
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_ingest", "knowledge_search", "knowledge_stats",
            "memory_seal", "memory_verify", "memory_info"
        ]
    )]
    memory_open: Option<PathBuf>,

    /// ПРОВЕРИТЬ КОНТЕЙНЕР БЕЗ КЛЮЧА: заголовок (FNV-1a64) + внешний
    /// SHA-256 шифротекста — битый git-sync/диск виден до расшифровки.
    #[cfg(feature = "pnd-ffi")]
    #[arg(
        long = "memory-verify",
        value_name = "VAULT",
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_ingest", "knowledge_search", "knowledge_stats",
            "memory_seal", "memory_open", "memory_info"
        ]
    )]
    memory_verify: Option<PathBuf>,

    /// МЕТАДАННЫЕ КОНТЕЙНЕРА БЕЗ КЛЮЧА: версия, размеры, страницы,
    /// итерации KDF, флаги (для человека и агента).
    #[cfg(feature = "pnd-ffi")]
    #[arg(
        long = "memory-info",
        value_name = "VAULT",
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_ingest", "knowledge_search", "knowledge_stats",
            "memory_seal", "memory_open", "memory_verify"
        ]
    )]
    memory_info: Option<PathBuf>,

    /// Выходной путь (с --memory-seal: PATH.pvt по умолчанию;
    /// с --memory-open: VAULT без .pvt / с суффиксом .out).
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "memory-out", value_name = "PATH")]
    memory_out: Option<PathBuf>,

    /// Имя переменной окружения с парольной фразой
    /// [default: POLER_VAULT_KEY]. Если переменной нет — одна строка
    /// читается из stdin (не попадает в историю shell).
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "memory-key-env", value_name = "VAR", default_value = "POLER_VAULT_KEY")]
    memory_key_env: String,

    /// Записать публичный контент-хеш открытого текста в заголовок
    /// (дедупликация/адресация между контейнерами; по умолчанию
    /// отключено — приватность: хеш позволяет сверять догадки о содержимом).
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "memory-content-id", requires = "memory_seal")]
    memory_content_id: bool,

    /// Итерации KDF при печати [default: 100000] (минимум 10000).
    #[cfg(feature = "pnd-ffi")]
    #[arg(
        long = "memory-kdf-iters",
        value_name = "N",
        default_value_t = poler_engine::crypto::kdf::DEFAULT_ITERATIONS,
        requires = "memory_seal"
    )]
    memory_kdf_iters: u32,

    /// AIDDE impact-анализ символа (call graph + upstream/downstream паспорт).
    #[arg(long)]
    impact: Option<String>,

    // ---------- E1/v0.31.0: полер-исполнитель команд (фича pnd-ffi) ----------

    /// ИДЕАЛЬНЫЙ ИСПОЛНИТЕЛЬ КОМАНД (ядро os/core/poler_exec.zig, raw-syscalls):
    /// запустить CMD с аргументами, жёстким таймаутом, лимитом захвата вывода
    /// (хвост) и гарантией отсутствия зомби. Вывод ребёнка — на наши stdout/stderr,
    /// код выхода — код ребёнка; таймаут — 124 (конвенция GNU timeout);
    /// не найдено — 127. Без шелл-парсинга — инъекции невозможны.
    /// ВАЖНО: все флаги (--exec-timeout-ms и др.) — ДО --exec; после --exec
    /// всё до конца строки — команда и её аргументы (включая -флаги команды).
    #[cfg(feature = "pnd-ffi")]
    #[arg(
        long = "exec",
        value_name = "CMD ARGS...",
        num_args = 1..,
        allow_hyphen_values = true,
        conflicts_with_all = [
            "web_search", "crawl", "web_stats", "mcp", "mcp_http", "shell", "tui",
            "impact", "grep", "chunk", "benchmark", "semantic", "license",
            "knowledge_ingest", "knowledge_search", "knowledge_stats",
            "memory_seal", "memory_open", "memory_verify", "memory_info"
        ]
    )]
    exec: Vec<String>,

    /// Жёсткий таймаут команды --exec, мс [default: 30000].
    /// По истечении: SIGTERM группе → grace → SIGKILL.
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-timeout-ms", value_name = "MS", default_value_t = 30_000)]
    exec_timeout_ms: u64,

    /// Grace между SIGTERM и SIGKILL при таймауте --exec, мс.
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-grace-ms", value_name = "MS", default_value_t = 100)]
    exec_grace_ms: u64,

    /// Лимит захвата вывода НА ПОТОК для --exec, байт [default: 256 КиБ].
    /// Удерживается ХВОСТ вывода (кольцевой буфер), превышение — флаг
    /// truncated и заметка в stderr; O(1) памяти при любом объёме вывода.
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-max-out", value_name = "BYTES", default_value_t = 262_144)]
    exec_max_out: usize,

    /// Данные в stdin команды --exec (строка целиком).
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-stdin", value_name = "DATA")]
    exec_stdin: Option<String>,

    /// E2: рабочий каталог команды --exec (chdir в бутстрапе ребёнка;
    /// провал → код 125). Родитель многопоточен — сам chdir не делает.
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-cwd", value_name = "DIR")]
    exec_cwd: Option<String>,

    /// E2: переменная окружения ребёнка KEY=VALUE (повторяемый флаг;
    /// без него наследуется окружение процесса).
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-env", value_name = "KEY=VALUE")]
    exec_env: Vec<String>,

    /// E2: запустить под псевдотерминалом 200x50 (sudo/fzf/htop, isatty);
    /// stdout и stderr сливаются в один поток (природа PTY).
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-pty", default_value_t = false)]
    exec_pty: bool,

    /// E2: режим захвата при переполнении --exec-max-out: tail — только
    /// хвост; head_tail — первые B/2 + маркер «dropped N» + последние B/2
    /// (стек-трейс в начале огромного лога больше не теряется).
    #[cfg(feature = "pnd-ffi")]
    #[arg(long = "exec-capture", value_name = "MODE", default_value = "tail")]
    exec_capture: String,

    /// Disk-backed таблица символов (SQLite) для AIDDE на гигантских
    /// кодовых базах: RAM ограничен пачками записи, BFS — индексами.
    #[arg(long = "impact-cache")]
    impact_cache: Option<PathBuf>,

    /// Переиспользовать существующую --impact-cache базу без
    /// перестройки (мгновенные повторные impact-запросы).
    #[arg(long = "impact-reuse", default_value_t = false)]
    impact_reuse: bool,

    /// Глубина BFS impact-анализа.
    #[arg(long = "impact-depth", default_value_t = 3)]
    impact_depth: usize,

    /// Коннектом FLYCSR1 (FlyWire v783): мозг мухи как матрица A —
    /// node/edge/khop/impact-запросы прямо из zstd-артефакта.
    #[arg(
        long = "connectome",
        value_name = "CSR_ZST",
        conflicts_with_all = [
            "grep", "chunk", "web", "crawl", "web_search", "web_stats", "mcp",
            "mcp_http", "shell", "tui", "impact", "browser_index", "web_lens",
            "web_lens_install", "archive_list"
        ]
    )]
    connectome: Option<PathBuf>,

    /// Таблица нейронов (flywire_v783_nodes.bin): имена root_id в выводе
    /// и поиск нейрона по root_id (не только по индексу).
    #[arg(long = "connectome-nodes", value_name = "NODES_BIN", requires = "connectome")]
    connectome_nodes: Option<PathBuf>,

    /// Паспорт нейрона: степени, синаптическая масса, топ-рёбра.
    #[arg(long = "connectome-node", value_name = "IDX_OR_ROOT_ID", requires = "connectome")]
    connectome_node: Option<String>,

    /// Ребро U:V — вес, медиатор, знак и ротор J = A − Aᵀ пары.
    #[arg(long = "connectome-edge", value_name = "U:V", requires = "connectome")]
    connectome_edge: Option<String>,

    /// K-hop BFS потока сигнала от нейрона (глубина: --k-hop).
    #[arg(long = "connectome-khop", value_name = "IDX_OR_ROOT_ID", requires = "connectome")]
    connectome_khop: Option<String>,

    /// Входящие связи нейрона (CSC): «кто управляет» — impact-слой AIDDE.
    #[arg(long = "connectome-impact", value_name = "IDX_OR_ROOT_ID", requires = "connectome")]
    connectome_impact: Option<String>,

    /// Полный список партнёров нейрона (по весу; направление: --connectome-dir).
    #[arg(long = "connectome-neighbors", value_name = "IDX_OR_ROOT_ID", requires = "connectome")]
    connectome_neighbors: Option<String>,

    /// Кратчайший путь сигнала FROM:TO (BFS с цепочкой прыжков).
    #[arg(long = "connectome-path", value_name = "FROM:TO", requires = "connectome")]
    connectome_path: Option<String>,

    /// Общие партнёры набора нейронов, csv: a,b,c (направление: --connectome-dir).
    #[arg(long = "connectome-common", value_name = "A,B,...", requires = "connectome")]
    connectome_common: Option<String>,

    /// Хабы: топ степеней + PageRank (сколько: --connectome-top).
    #[arg(long = "connectome-centrality", requires = "connectome")]
    connectome_centrality: bool,

    /// Глобальный топ пар по циркуляции J = A − Aᵀ (K пар, порог: --connectome-min-abs).
    #[arg(long = "connectome-rotor-top", value_name = "K", requires = "connectome")]
    connectome_rotor_top: Option<usize>,

    /// Мотивы вокруг нейрона: реципрокные, feedforward, feedback.
    #[arg(long = "connectome-motifs", value_name = "IDX_OR_ROOT_ID", requires = "connectome")]
    connectome_motifs: Option<String>,

    /// Симуляция распространения сигнала от нейронов, csv-семена.
    #[arg(long = "connectome-propagate", value_name = "SEEDS", requires = "connectome")]
    connectome_propagate: Option<String>,

    /// Направление для --connectome-neighbors (out|in) и --connectome-common (down|up).
    #[arg(long = "connectome-dir", value_name = "DIR", default_value = "out", requires = "connectome")]
    connectome_dir: String,

    /// Лимит списков (--connectome-neighbors/--connectome-common).
    #[arg(long = "connectome-limit", value_name = "N", default_value_t = 20, requires = "connectome")]
    connectome_limit: usize,

    /// Размер топов (--connectome-centrality, --connectome-propagate).
    #[arg(long = "connectome-top", value_name = "N", default_value_t = 10, requires = "connectome")]
    connectome_top: usize,

    /// Порог циркуляции для --connectome-rotor-top.
    #[arg(long = "connectome-min-abs", value_name = "J", default_value_t = 1, requires = "connectome")]
    connectome_min_abs: i32,

    /// Шагов симуляции --connectome-propagate.
    #[arg(long = "connectome-steps", value_name = "N", default_value_t = 4, requires = "connectome")]
    connectome_steps: usize,

    /// Гейн симуляции (γ: разгон возбуждения).
    #[arg(long = "connectome-gamma", value_name = "G", default_value_t = 0.05, requires = "connectome")]
    connectome_gamma: f64,

    /// Утечка симуляции (leak: память состояния).
    #[arg(long = "connectome-leak", value_name = "L", default_value_t = 0.8, requires = "connectome")]
    connectome_leak: f64,

    /// Фильтр знака K-hop: all | exc | inh (поток по возбуждающим/тормозным).
    #[arg(long = "connectome-sign", value_name = "FILTER", default_value = "all", requires = "connectome")]
    connectome_sign: String,

    /// JSON-вывод режима --connectome (для агентов).
    #[arg(long = "connectome-json", requires = "connectome")]
    connectome_json: bool,

    // ── L1/v0.34.0: Литературный Двигатель POLER[Ψ] ────────────────

    /// Анализ поля интенции: замысел → Ω(o) + архетипы (фазы ℘→O).
    #[arg(
        long = "literary-field",
        value_name = "TEXT",
        conflicts_with_all = [
            "literary_generate", "grep", "chunk", "web", "crawl", "web_search",
            "web_stats", "mcp", "mcp_http", "shell", "tui", "impact",
            "browser_index", "web_lens", "web_lens_install", "archive_list",
            "connectome"
        ]
    )]
    literary_field: Option<String>,

    /// Генерация нарратива: полный прогон до H^Ψ = 0 (+ текст-разметка).
    #[arg(
        long = "literary-generate",
        value_name = "TEXT",
        conflicts_with_all = [
            "grep", "chunk", "web", "crawl", "web_search", "web_stats", "mcp",
            "mcp_http", "shell", "tui", "impact", "browser_index", "web_lens",
            "web_lens_install", "archive_list", "connectome"
        ]
    )]
    literary_generate: Option<String>,

    /// Коннектом FLYCSR1 — мушиная калибровка двигателя (каста от семян:
    /// ротор J = A − Aᵀ закручивает нарратив, D = L·Lᵀ гасит пертурбации).
    #[arg(long = "literary-csr", value_name = "CSR_ZST")]
    literary_csr: Option<PathBuf>,

    /// Таблица root_id для --literary-csr (семена по root_id).
    #[arg(long = "literary-nodes", value_name = "NODES_BIN", requires = "literary_csr")]
    literary_nodes: Option<PathBuf>,

    /// Семена касты, csv: индексы или root_id [default: 0].
    #[arg(long = "literary-seeds", value_name = "A,B,...", default_value = "0")]
    literary_seeds: String,

    /// K-hop BFS касты [default: 2].
    #[arg(long = "literary-khop", value_name = "N", default_value_t = 2)]
    literary_khop: usize,

    /// Максимум нейронов в касте [default: 48, max 64].
    #[arg(long = "literary-max-cast", value_name = "N", default_value_t = 48)]
    literary_max_cast: usize,

    /// Осей фазового пространства [default: 64].
    #[arg(long = "literary-dims", value_name = "N", default_value_t = 64)]
    literary_dims: usize,

    /// Шагов генерации --literary-generate [default: 48].
    #[arg(long = "literary-steps", value_name = "N", default_value_t = 48)]
    literary_steps: usize,

    /// η — шаг интегратора [default: 0.1].
    #[arg(long = "literary-eta", value_name = "F", default_value_t = 0.1)]
    literary_eta: f64,

    /// η_r — резонансный шаг [default: 0.05].
    #[arg(long = "literary-eta-r", value_name = "F", default_value_t = 0.05)]
    literary_eta_r: f64,

    /// ρ — затухание темпорального эха R[n] [default: 0.9].
    #[arg(long = "literary-rho", value_name = "F", default_value_t = 0.9)]
    literary_rho: f64,

    /// κ — масштаб энергии значимости ε [default: 1.2].
    #[arg(long = "literary-kappa", value_name = "F", default_value_t = 1.2)]
    literary_kappa: f64,

    /// γ — баланс циркуляции J (мушиный ротор) [default: 1.0].
    #[arg(long = "literary-gamma", value_name = "F", default_value_t = 1.0)]
    literary_gamma: f64,

    /// λ — вес логической регуляризации [default: 0.01].
    #[arg(long = "literary-lambda", value_name = "F", default_value_t = 0.01)]
    literary_lambda: f64,

    /// No-Mul: Trit5-квантование латентного состояния p_t ({−1,0,+1},
    /// SIMD-скалярные произведения без f32-математики).
    #[arg(long = "literary-no-mul", default_value_t = false)]
    literary_no_mul: bool,

    /// JSON-вывод режима --literary-* (для агентов).
    #[arg(long = "literary-json", default_value_t = false)]
    literary_json: bool,

    // ── S1/v0.35.0: Синаптический Вихрь SSN ────────────────────────

    /// Прогон живого мозга: полный доказанный стек вихря, телеметрия.
    #[arg(long = "ssn-demo", default_value_t = false)]
    ssn_demo: bool,

    /// CSE-кодирование текста (с --ssn-encode-b — сходство пары).
    #[arg(long = "ssn-encode", value_name = "TEXT")]
    ssn_encode: Option<String>,

    /// Второй текст для сравнения с --ssn-encode.
    #[arg(long = "ssn-encode-b", value_name = "TEXT", requires = "ssn_encode")]
    ssn_encode_b: Option<String>,

    /// Сенсорная инъекция текста в живой мозг + прогон --ssn-steps.
    #[arg(long = "ssn-inject", value_name = "TEXT")]
    ssn_inject: Option<String>,

    /// Нейронов в мозге [default: 600].
    #[arg(long = "ssn-n", value_name = "N", default_value_t = 600)]
    ssn_n: usize,

    /// Виртуальных синаптических полей на нейрон [default: 16].
    #[arg(long = "ssn-fields", value_name = "N", default_value_t = 16)]
    ssn_fields: usize,

    /// Seed траектории мозга [default: 777].
    #[arg(long = "ssn-seed", value_name = "N", default_value_t = 777)]
    ssn_seed: u64,

    /// Размерность CSE-вектора сенсорики [default: 128].
    #[arg(long = "ssn-dims", value_name = "N", default_value_t = 128)]
    ssn_dims: usize,

    /// Шагов для --ssn-demo/--ssn-inject [default: 10000].
    #[arg(long = "ssn-steps", value_name = "N", default_value_t = 10_000)]
    ssn_steps: usize,

    /// Readout: топ-K активных нейронов [default: 10].
    #[arg(long = "ssn-readout", value_name = "K", default_value_t = 10)]
    ssn_readout: usize,

    /// JSON-вывод режима --ssn-* (для агентов).
    #[arg(long = "ssn-json", default_value_t = false)]
    ssn_json: bool,

    // ── S2/v0.36.0: Триединая Архитектура (муха + вихрь + кристалл) ──

    /// Витрина Триединства: муха + вихрь + кристалл → живая речь +
    /// моторные интенты + телеметрия всех трёх опор.
    #[arg(long = "triune-demo", default_value_t = false)]
    triune_demo: bool,

    /// Попросить Триединство говорить от промпта.
    #[arg(long = "triune-speak", value_name = "TEXT")]
    triune_speak: Option<String>,

    /// Токенов речи [default: 24].
    #[arg(long = "triune-tokens", value_name = "N", default_value_t = 24)]
    triune_tokens: usize,

    /// Seed Триединства [default: 777].
    #[arg(long = "triune-seed", value_name = "N", default_value_t = 777)]
    triune_seed: u64,

    /// Усиление ротора мухи γ (0 = муха спит) [default: 0.8].
    #[arg(long = "triune-gamma", value_name = "F", default_value_t = 0.8)]
    triune_gamma: f64,

    /// Настоящий мозг мухи: артефакт коннектома FLYCSR1 (.csr.zst).
    #[arg(long = "triune-connectome", value_name = "CSR_ZST")]
    triune_connectome: Option<PathBuf>,

    /// Семена касты для --triune-connectome (CSV индексов) [default: 1000,5000,9000].
    #[arg(long = "triune-seeds", value_name = "CSV", default_value = "1000,5000,9000")]
    triune_seeds: String,

    /// Внешний кристалл .t5c (по умолчанию — зашитый в бинарник).
    #[arg(long = "triune-crystal", value_name = "T5C")]
    triune_crystal: Option<PathBuf>,

    /// Сохранить кристалл ПОСЛЕ речи (синапсы, обученные в сессии) [v0.38.0].
    #[arg(long = "triune-out", value_name = "T5C")]
    triune_out: Option<PathBuf>,

    /// Выключить Хеббовскую пластичность во время речи [v0.38.0].
    #[arg(long = "triune-no-learn", default_value_t = false)]
    triune_no_learn: bool,

    /// JSON-вывод режима --triune-* (для агентов).
    #[arg(long = "triune-json", default_value_t = false)]
    triune_json: bool,

    /// Собрать кристалл знаний .t5c из корпуса.
    #[arg(long = "crystal-build", value_name = "CORPUS_TXT")]
    crystal_build: Option<PathBuf>,

    /// Потокова інгестия директорії з текстами/кодом в кристалл .t5c.
    #[arg(long = "crystal-ingest-dir", value_name = "DIR")]
    crystal_ingest_dir: Option<PathBuf>,

    /// Автогенерація розгорнутого x86_64 асемблерного алгоритму матриці архетипів (50 000+ рядків).
    #[arg(long = "gen-archetype-asm", value_name = "OUT_ASM_PATH")]
    gen_archetype_asm: Option<PathBuf>,

    /// Мінімальна кількість рядків асемблера для --gen-archetype-asm [default: 50000].
    #[arg(long = "gen-archetype-lines", value_name = "N", default_value_t = 50000)]
    gen_archetype_lines: usize,

    /// АВТОНОМНЫЙ ИНТЕРНЕТ-ИНЖЕКТОР ПАМЯТИ: краулить URL (глубина и
    /// лимиты наследуются из --crawl-*) и потоково выучить кристалл .t5c.
    #[arg(
        long = "learn-web",
        value_name = "URL",
        conflicts_with_all = [
            "crawl", "web_search", "web_stats", "browser_index", "learn_dir",
            "mcp", "mcp_http", "shell", "tui", "impact", "web_lens",
            "web_lens_install", "crystal_build", "crystal_ingest_dir",
        ]
    )]
    learn_web: Option<String>,

    /// Автономный инжектор памяти: рекурсивно выучить кристалл из папки
    /// (текстовые расширения, бинарники пропускаются).
    #[arg(
        long = "learn-dir",
        value_name = "DIR",
        conflicts_with_all = [
            "crawl", "web_search", "web_stats", "browser_index", "learn_web",
            "mcp", "mcp_http", "shell", "tui", "impact", "web_lens",
            "web_lens_install", "crystal_build", "crystal_ingest_dir",
        ]
    )]
    learn_dir: Option<PathBuf>,

    /// Размер чанка потокового чтения при обучении, байт [default: 65536].
    #[arg(long = "learn-chunk", value_name = "BYTES", default_value_t = 65_536)]
    learn_chunk: usize,

    /// Лимит уникальных слов в RAM при обучении (защита от OOM) [default: 262144].
    #[arg(long = "learn-word-cap", value_name = "N", default_value_t = 262_144)]
    learn_word_cap: usize,

    /// Лимит биграммных пар в RAM при обучении [default: 2097152].
    #[arg(long = "learn-bigram-cap", value_name = "N", default_value_t = 2_097_152)]
    learn_bigram_cap: usize,

    /// Словарь кристалла для --crystal-build / --crystal-ingest-dir /
    /// --learn-web / --learn-dir [default: 384; для обучения рекомендуем
    /// 4096..65536 — при 384 режимы learn автоматически поднимают до 4096].
    #[arg(long = "crystal-vocab", value_name = "N", default_value_t = 384)]
    crystal_vocab: usize,

    /// Выходной путь .t5c для --crystal-build / --learn-* [default: <корпус>.t5c / memory.t5c].
    #[arg(long = "crystal-out", value_name = "T5C")]
    crystal_out: Option<PathBuf>,

    /// Потоковое квантование весов на лету: сырец → .t5q (70B на диске 4 ГБ).
    #[arg(long = "stream-quant", value_name = "FILE|-")]
    stream_quant: Option<PathBuf>,

    /// Выходной путь .t5q [default: <вход>.t5q].
    #[arg(long = "stream-quant-out", value_name = "T5Q")]
    stream_quant_out: Option<PathBuf>,

    /// Значений на блок [default: 512].
    #[arg(long = "stream-quant-block", value_name = "N", default_value_t = 512)]
    stream_quant_block: usize,

    /// Доля топ-|w| тритов на блок (вакуум при <1) [default: 1.0].
    #[arg(long = "stream-quant-keep", value_name = "F", default_value_t = 1.0)]
    stream_quant_keep: f64,

    /// Вход — f16-LE (иначе f32-LE).
    #[arg(long = "stream-quant-f16", default_value_t = false)]
    stream_quant_f16: bool,

    /// Watcher-режим: инкрементальный рескан по mtime/size.
    #[arg(long)]
    watch: bool,

    /// Интервал watcher-опроса в секундах.
    #[arg(long = "interval-secs", default_value_t = 2)]
    interval: u64,

    /// Дифф-режим watcher: печатать только новые/появившиеся якоря.
    #[arg(long, default_value_t = false)]
    diff: bool,

    /// Число топ-результатов.
    #[arg(short = 't', long, default_value_t = 10)]
    top: usize,

    /// Коэффициент затухания IIR-резонанса φ ∈ [0.75, 0.90].
    #[arg(long, default_value_t = 0.85)]
    phi: f64,

    /// Масштабный коэффициент калибровки ε.
    #[arg(long, default_value_t = 1.0)]
    kappa: f64,

    /// Радиус токенного окна.
    #[arg(short = 'w', long, default_value_t = 40)]
    window: usize,

    /// Глубина K-hop обхода графа сущностей.
    #[arg(short = 'k', long = "k-hop", default_value_t = 2)]
    k_hop: usize,

    /// Временной фильтр графа (например: Т-23).
    #[arg(long)]
    metric: Option<String>,

    /// Формат вывода.
    #[arg(long, value_enum, default_value_t = Format::AiJson)]
    format: Format,

    /// Режим очистки PII.
    #[arg(long, value_enum, default_value_t = PiiArg::Mask)]
    pii: PiiArg,

    /// Режим накопления резонанса.
    #[arg(long = "resonance-mode", value_enum, default_value_t = ResonanceArg::Hits)]
    resonance: ResonanceArg,

    /// POLER[Ψ]: η — скорость обучения внимания [default: 0.05].
    #[arg(long = "psi-eta", default_value_t = 0.05)]
    psi_eta: f64,

    /// POLER[Ψ]: γ — вес резонансного члена ∇ε [default: 0.5].
    #[arg(long = "psi-gamma", default_value_t = 0.5)]
    psi_gamma: f64,

    /// POLER[Ψ]: ρ — затухание резонансной памяти [default: 0.9].
    #[arg(long = "psi-rho", default_value_t = 0.9)]
    psi_rho: f64,

    /// POLER[Ψ]: K — глубина резонансной памяти [default: 8].
    #[arg(long = "psi-depth", default_value_t = 8)]
    psi_depth: usize,

    /// POLER-цикл (P3_Engine): η — learning rate [default: 0.01].
    #[arg(long = "poler-eta", default_value_t = 0.01)]
    poler_eta: f64,

    /// POLER-цикл: γ — резонансная связь [default: 0.1].
    #[arg(long = "poler-gamma", default_value_t = 0.1)]
    poler_gamma: f64,

    /// POLER-цикл: mix — CORDIC-квантовая нормализация [default: 0.1].
    #[arg(long = "poler-mix", default_value_t = 0.1)]
    poler_mix: f64,

    /// POLER-цикл: d — диссипатор D=LLᵀ (энтропийный горел) [default: 0.02].
    #[arg(long = "poler-dissipator", default_value_t = 0.02)]
    poler_dissipator: f64,

    /// ε по статистикам файла вместо корпуса.
    #[arg(long, default_value_t = false)]
    local_stats: bool,

    /// Сканируемые расширения (через запятую).
    #[arg(long, default_value = DEFAULT_EXTENSIONS)]
    extensions: String,

    /// Пропускать файлы больше N мегабайт.
    #[arg(long = "max-file-size", default_value_t = 64)]
    max_file_mb: u64,

    /// Максимум байт enclosing_scope в якоре.
    #[arg(long = "max-scope", default_value_t = 16384)]
    max_scope: usize,

    /// Максимум K-hop отношений на якорь.
    #[arg(long = "max-relations", default_value_t = 64)]
    max_relations: usize,

    /// Бюджет рёбер графа сущностей.
    #[arg(long = "max-graph-triples", default_value_t = 200_000)]
    max_graph_triples: usize,

    /// Показывать скрытые файлы/директории (rg --hidden).
    #[arg(long, default_value_t = false)]
    hidden: bool,

    /// Экспорт графа сущностей в SQL-файл (схема super-z memory_graph).
    #[arg(long = "graph-export")]
    graph_export: Option<PathBuf>,

    /// Число потоков rayon (по умолчанию — все ядра).
    #[arg(long)]
    threads: Option<usize>,

    /// Подробная статистика прогона в stderr.
    #[arg(short = 'v', long)]
    verbose: bool,
}

fn print_result(res: &SearchResult, format: Format) {
    match format {
        Format::AiJson => {
            println!("{}", serde_json::to_string_pretty(res).unwrap_or_default());
        }
        Format::Md => print!("{}", render_markdown(res)),
        Format::Simple => print!("{}", render_simple(res)),
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// Человекочитаемый отчёт Benchmark Suite (v0.21, Задача 4).
fn print_bench_report(res: &poler_engine::bench::BenchResults) {
    print!("{}", poler_engine::bench::report_text(res));
}

/// Вывод результатов веб-поиска в трёх форматах.
fn print_web_hits(
    hits: &[poler_engine::web::WebHit],
    query: &str,
    format: Format,
    expansion: Option<&poler_engine::retrieval::QueryExpansion>,
) {
    use std::io::Write;
    match format {
        Format::AiJson => {
            let mut out = serde_json::json!({
                "engine": "poler-engine",
                "mode": "web-search",
                "rank": "POLER WebRank v1 (0.55·BM25 + 0.15·PageRank + 0.20·title + 0.10·ε-density)",
                "query": query,
                "total": hits.len(),
                "results": hits,
            });
            if let Some(exp) = expansion {
                if !exp.is_empty() {
                    out["semantic_bridge"] = serde_json::to_value(exp).unwrap_or_default();
                }
            }
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        Format::Simple => {
            // WHY — в stderr: stdout остаётся чистым для пайпа
            if let Some(exp) = expansion {
                if !exp.is_empty() {
                    eprintln!("{}", exp.why());
                }
            }
            for h in hits {
                println!("{:.4}  {}  {}", h.score, h.url, h.title);
            }
        }
        Format::Md => {
            println!("# Веб-поиск: «{query}»\n");
            if let Some(exp) = expansion {
                if !exp.is_empty() {
                    println!("## Semantic Bridge (WHY)\n");
                    for l in exp.why_lines() {
                        println!("- {l}");
                    }
                    println!();
                }
            }
            for (i, h) in hits.iter().enumerate() {
                println!("## {}. {}\n", i + 1, if h.title.is_empty() { &h.url } else { &h.title });
                println!("- URL: {}", h.url);
                let phrase_note = if h.phrase_occ > 0 {
                    format!(", фраз=·{}", h.phrase_occ)
                } else {
                    String::new()
                };
                println!("- Score: {:.4} (bm25={:.3}, pagerank={:.5}, title={:.2}, ε={:.5}{})", h.score, h.bm25, h.pagerank, h.title_frac, h.density, phrase_note);
                println!("- Язык: {}, токенов: {}\n", if h.lang.is_empty() { "-" } else { &h.lang }, h.doclen);
                println!("> {}\n", h.snippet);
            }
        }
    }
    let _ = std::io::stdout().flush();
}

/// v0.18.0 → v2.0: человекочитаемый статус лицензии (`--license`).
/// Формат живёт в license::status_text() — единый источник
/// для CLI и команды `license` в Terminal Gateway.
fn print_license_status() -> ExitCode {
    print!("{}", poler_engine::license::status_text());
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    // v0.24.0: hidden-вход PATH-shim медиации агентов — перехват ДО clap:
    // shim-обёртки (~/.poler-engine/shim/bash) вызывают именно его.
    // `poler-engine __gateway-shim <shell> [args…]` → судит payload тем же
    // sandbox-гейтом и либо exec реальный shell, либо отказ 126.
    let mut argv = std::env::args();
    let _prog = argv.next();
    if argv.next().as_deref() == Some("__gateway-shim") {
        let rest: Vec<String> = argv.collect();
        return ExitCode::from(poler_engine::gateway::shim::shim_main(&rest) as u8);
    }
    let cli = Cli::parse();
    run(cli)
}

fn run(cli: Cli) -> ExitCode {

    if let Some(threads) = cli.threads {
        if let Err(e) = rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .build_global()
        {
            eprintln!("poler-engine: не удалось настроить пул потоков: {e}");
            return ExitCode::from(2);
        }
    }

    // ---------- v2.0 Part E/F: суверенный ML-инференс через pqc ----------
    // Никакого ONNX Runtime: энкодер/NER/LLM читают .pqw mmap'ом и
    // считаются нативными SIMD-кернелами внутри этого бинарника.
    if cli.pqw_selftest {
        return ExitCode::from(poler_engine::pqc::selftest::run_selftest() as u8);
    }
    if let Some(target) = &cli.t5q_compile {
        return ExitCode::from(run_t5q_compile(target) as u8);
    }
    if let Some(target) = &cli.jit_loop {
        return ExitCode::from(run_jit_loop(target) as u8);
    }
    if let Some(mode) = cli.semantic.as_deref() {
        return ExitCode::from(run_semantic_native(mode, &cli) as u8);
    }
    if let Some(mode) = cli.llm.as_deref() {
        return ExitCode::from(run_llm(mode, &cli) as u8);
    }
    if let Some(what) = cli.ner.as_deref() {
        return ExitCode::from(run_ner(what, &cli) as u8);
    }

    // ---------- Суверенный Гиппокамп: библиотека знаний POLER (v0.29) ----------
    if cli.knowledge_stats {
        return ExitCode::from(run_knowledge_stats(&cli) as u8);
    }
    if let Some(root) = &cli.knowledge_ingest {
        return ExitCode::from(run_knowledge_ingest(&cli, root) as u8);
    }
    if let Some(query) = &cli.knowledge_search {
        return ExitCode::from(run_knowledge_search(&cli, query) as u8);
    }

    // ---------- Крипто-слой данных: POLER Vault (M4.5 CDL) ----------
    #[cfg(feature = "pnd-ffi")]
    {
        if let Some(input) = &cli.memory_seal {
            return ExitCode::from(run_memory_seal(&cli, input) as u8);
        }
        if let Some(vault) = &cli.memory_open {
            return ExitCode::from(run_memory_open(&cli, vault) as u8);
        }
        if let Some(vault) = &cli.memory_verify {
            return ExitCode::from(run_memory_verify(&cli, vault) as u8);
        }
        if let Some(vault) = &cli.memory_info {
            return ExitCode::from(run_memory_info(&cli, vault) as u8);
        }
    }

    // ---------- E1/v0.31.0: полер-исполнитель команд ----------
    #[cfg(feature = "pnd-ffi")]
    if !cli.exec.is_empty() {
        return ExitCode::from(run_exec(&cli) as u8);
    }

    // ---------- MCP-сервер: stdio JSON-RPC для LLM-агентов ----------
    if let Some(n) = cli.mcp_bench {
        return ExitCode::from(run_mcp_bench(&cli, n) as u8);
    }

    // ---------- M6: шифропоток журнала (--vault-log с --mcp/--mcp-http) ----------
    if cli.vault_log.is_some() && !cli.mcp && cli.mcp_http.is_none() {
        eprintln!("poler-vault-log: --vault-log работает вместе с --mcp или --mcp-http");
        return ExitCode::from(2);
    }

    if cli.mcp {
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let code = poler_engine::mcp::run(
            cli.cdp_port,
            cli.web_wait_ms,
            db,
            cli.knowledge_db.clone(),
            mcp_server_options(&cli),
        );
        return ExitCode::from(code as u8);
    }

    // ---------- v0.19.0: WebLens — браузерный режим движка ----------
    // Материализует расширение MV3 (вшито в бинарник), запускает оконный
    // Chromium с уже установленным WebLens (--load-extension — автоустановка)
    // и держит MCP-сервер на localhost: Ctrl+C останавливает демона,
    // окно браузера живёт своей жизнью.
    if let Some(bind) = cli.web_lens.clone() {
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let token = match poler_engine::web::weblens::weblens_token() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("poler-weblens: токен: {e}");
                return ExitCode::from(2);
            }
        };
        // «8765» → «127.0.0.1:8765» (тот же канон, что у --mcp-http)
        let bind_addr = match bind.parse::<u16>() {
            Ok(port) => format!("127.0.0.1:{port}"),
            Err(_) => bind.clone(),
        };
        let endpoint = format!("http://{bind_addr}/");
        let ext_dir = match poler_engine::web::weblens::materialize(&endpoint, &token) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("poler-weblens: материализация: {e}");
                return ExitCode::from(2);
            }
        };
        eprintln!("poler-weblens: расширение готово: {}", ext_dir.display());
        // оконный браузер с WebLens; неудача (нет дисплея/браузера) —
        // не фатально: демон продолжает serve, расширение можно поставить
        // в свой браузер (--web-lens-install)
        match poler_engine::web::weblens::spawn_windowed_browser(&ext_dir, "about:blank") {
            Ok(_child) => {
                eprintln!("poler-weblens: браузер запущен с WebLens — Alt+P открывает панель");
            }
            Err(e) => {
                eprintln!("poler-weblens: оконный браузер не запущен: {e}");
            }
        }
        let code = poler_engine::mcp_http::run_http(&bind, &token, cli.cdp_port, cli.web_wait_ms, db, cli.knowledge_db.clone(), mcp_server_options(&cli));
        return ExitCode::from(code as u8);
    }

    // ---------- --web-lens-install: WebLens в ЕЖЕДНЕВНЫЙ браузер ----------
    // chrome://-страницы автоматизировать нельзя (защита браузера) —
    // движок делает всё, что можно: файлы + конфиг + инструкция.
    if cli.web_lens_install {
        let token = match poler_engine::web::weblens::weblens_token() {
            Ok(t) => t,
            Err(e) => {
                eprintln!("poler-weblens: токен: {e}");
                return ExitCode::from(2);
            }
        };
        let endpoint = "http://127.0.0.1:8765/".to_string();
        let dir = match poler_engine::web::weblens::materialize(&endpoint, &token) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("poler-weblens: {e}");
                return ExitCode::from(2);
            }
        };
        println!("WebLens материализован: {}", dir.display());
        println!();
        println!("Установка в свой браузер (один раз, ~30 секунд):");
        println!("  1. Откройте chrome://extensions (или edge://extensions, brave://extensions)");
        println!("  2. Включите «Режим разработчика» (переключатель справа сверху)");
        println!("  3. «Загрузить распакованное расширение» → выберите каталог:");
        println!("     {}", dir.display());
        println!("  4. Alt+P (или иконка POLER на панели) — боковая панель поиска");
        println!();
        println!("Демон движка (панель работает, пока он жив):");
        println!("  poler-engine --web-lens            # рекомендуемый режим: свой браузер + демон");
        println!("  poler-engine --mcp-http 8765 --mcp-token <токен>   # только демон");
        println!();
        println!("Токен уже вписан в config.json расширения: {endpoint}");
        return ExitCode::SUCCESS;
    }

    // ---------- MCP-сервер по HTTP: удалённый агент через туннель ----------
    if let Some(bind) = cli.mcp_http.clone() {
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let token = cli
            .mcp_token
            .clone()
            .or_else(|| std::env::var("POLER_MCP_TOKEN").ok())
            .unwrap_or_else(poler_engine::mcp_http::generate_token);
        let code = poler_engine::mcp_http::run_http(&bind, &token, cli.cdp_port, cli.web_wait_ms, db, cli.knowledge_db.clone(), poler_engine::mcp::McpServerOptions::default());
        return ExitCode::from(code as u8);
    }

    // ---------- v0.28.1: Листинг архива без распаковки ----------
    if let Some(archive) = cli.archive_list.clone() {
        return ExitCode::from(run_archive_list(&cli, &archive) as u8);
    }

    // ---------- v0.30.0: Коннектом FLYCSR1 — мозг мухи как матрица A ----------
    if let Some(csr) = cli.connectome.clone() {
        return ExitCode::from(run_connectome(&cli, &csr) as u8);
    }

    // ---------- L1/v0.34.0: Литературный Двигатель POLER[Ψ] ----------
    if cli.literary_field.is_some() || cli.literary_generate.is_some() {
        return ExitCode::from(run_literary(&cli) as u8);
    }

    // ---------- S1/v0.35.0: Синаптический Вихрь SSN ----------
    if cli.ssn_demo || cli.ssn_encode.is_some() || cli.ssn_inject.is_some() {
        return ExitCode::from(run_ssn(&cli) as u8);
    }

    // ---------- S2/v0.36.0: Триединая Архитектура ----------
    if cli.triune_demo || cli.triune_speak.is_some() {
        return ExitCode::from(run_triune(&cli) as u8);
    }
    if cli.crystal_build.is_some() {
        return ExitCode::from(run_crystal_build(&cli) as u8);
    }
    if cli.crystal_ingest_dir.is_some() {
        return ExitCode::from(run_crystal_ingest_dir(&cli) as u8);
    }
    if cli.gen_archetype_asm.is_some() {
        return ExitCode::from(run_gen_archetype_asm(&cli) as u8);
    }
    if cli.learn_web.is_some() || cli.learn_dir.is_some() {
        return ExitCode::from(run_learn(&cli) as u8);
    }
    if cli.stream_quant.is_some() {
        return ExitCode::from(run_stream_quant(&cli) as u8);
    }

    // ---------- v0.20.0: Native Retrieval — grep-режим (слой 0) ----------
    if let Some(pattern) = cli.grep.clone() {
        use poler_engine::retrieval as nr;
        let output = if cli.grep_count {
            nr::GrepOutput::Count
        } else if cli.grep_list {
            nr::GrepOutput::ListMatching
        } else if cli.grep_list_nonmatching {
            nr::GrepOutput::ListNonMatching
        } else {
            nr::GrepOutput::Content
        };
        // Корни поиска: позиционный PATH (может повторяться неявно —
        // несколько аргументов clap не поддерживает, но PATH может быть
        // каталогом) либо текущий каталог.
        let roots: Vec<std::path::PathBuf> = match cli.path.clone() {
            Some(p) => vec![p],
            None => vec![std::path::PathBuf::from(".")],
        };
        // Архивы: пароль резолвится ДО параллельного прогона — промпт
        // на TTY нельзя звать из rayon-воркеров.
        let archive_password = if cli.archives {
            match resolve_archive_password(&cli, &roots, cli.grep_hidden) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("poler-archive: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            None
        };
        let config = nr::GrepConfig {
            pattern,
            mode: if cli.grep_regex { nr::GrepMode::Regex } else { nr::GrepMode::Literal },
            case_insensitive: cli.grep_ignore_case,
            before: cli.grep_before,
            after: cli.grep_after,
            max_count: cli.grep_max_count,
            output,
            include_hidden: cli.grep_hidden,
            respect_ignore: true,
            scan_archives: cli.archives,
            archive_password,
            archive_max_entry_bytes: cli.archive_max_entry_mb.saturating_mul(1024 * 1024),
        };
        match nr::grep_run(&roots, &config) {
            Ok(report) => {
                if cli.grep_json {
                    match serde_json::to_string_pretty(&report) {
                        Ok(json) => println!("{json}"),
                        Err(e) => {
                            eprintln!("poler-grep: сериализация JSON: {e}");
                            return ExitCode::from(2);
                        }
                    }
                } else {
                    print!("{}", nr::render_text(&report, nr::stdout_is_tty()));
                }
                for err in &report.stats.errors {
                    eprintln!("poler-grep: {err}");
                }
                // Ошибки обхода не меняют код (как у grep: trouble=2
                // только для фатальных). Совпадения решают 0/1.
                return ExitCode::from(report.exit_code() as u8);
            }
            Err(e) => {
                eprintln!("poler-grep: {e}");
                return ExitCode::from(2);
            }
        }
    }

    // ---------- v0.20.0: Native Retrieval — RAG-чанки (слой B) ----------
    if cli.chunk {
        use poler_engine::retrieval as nr;
        let path = cli.path.clone().unwrap_or_default();
        // v0.28.1: селектор «архив::запись» — чанки записи БЕЗ распаковки
        // (читается только указанная запись, bounded-буфер в памяти).
        if let Some((archive, entry)) =
            poler_engine::archive::split_virtual(&path.to_string_lossy())
        {
            if archive.exists() && poler_engine::archive::is_archive(&archive) {
                let limits = poler_engine::archive::ReadLimits {
                    max_entry_bytes: cli.archive_max_entry_mb.saturating_mul(1024 * 1024),
                };
                let roots = vec![archive.clone()];
                let password = match resolve_archive_password(&cli, &roots, false) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("poler-chunk: {e}");
                        return ExitCode::from(2);
                    }
                };
                let text = match poler_engine::archive::read_entry_text(
                    &archive,
                    &entry,
                    password.as_deref(),
                    &limits,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        eprintln!("poler-chunk: {e}");
                        return ExitCode::from(2);
                    }
                };
                let config = nr::ChunkConfig {
                    target_tokens: cli.chunk_size,
                    overlap_tokens: cli.chunk_overlap,
                    ..Default::default()
                };
                // Формат — по расширению записи (не архива).
                let format = nr::ChunkFormat::detect(std::path::Path::new(&entry));
                let report = nr::chunk_document(&text, format, &config);
                let display = poler_engine::archive::virtual_name(&archive, &entry);
                if cli.chunk_json {
                    match serde_json::to_string_pretty(&report) {
                        Ok(json) => println!("{json}"),
                        Err(e) => {
                            eprintln!("poler-chunk: сериализация JSON: {e}");
                            return ExitCode::from(2);
                        }
                    }
                } else {
                    print!("{}", nr::render_chunks_text(&report, &display));
                }
                return ExitCode::SUCCESS;
            }
            // «::» в имени, но левая часть — не архив: трактуем как
            // обычный путь (фолбэк, файл с «::» в имени — экзотика).
        }
        if !path.is_file() {
            eprintln!("poler-chunk: путь не файл (или не найден): {}", path.display());
            return ExitCode::from(2);
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("poler-chunk: прочитать {}: {e}", path.display());
                return ExitCode::from(2);
            }
        };
        let config = nr::ChunkConfig {
            target_tokens: cli.chunk_size,
            overlap_tokens: cli.chunk_overlap,
            ..Default::default()
        };
        let format = nr::ChunkFormat::detect(&path);
        let report = nr::chunk_document(&text, format, &config);
        if cli.chunk_json {
            match serde_json::to_string_pretty(&report) {
                Ok(json) => println!("{json}"),
                Err(e) => {
                    eprintln!("poler-chunk: сериализация JSON: {e}");
                    return ExitCode::from(2);
                }
            }
        } else {
            print!("{}", nr::render_chunks_text(&report, &path.display().to_string()));
        }
        return ExitCode::SUCCESS;
    }

    // ---------- Terminal Gateway: единый терминальный шлюз v0.22.0 ----------
    if cli.gateway {
        let db_path = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        return poler_engine::gateway::run_gateway(db_path, cli.dangerously_allow_all);
    }

    // ---------- poler-shell: интерактивный терминал v0.15.0 ----------
    if cli.shell {
        let db_path = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        return poler_engine::shell::run_shell(db_path);
    }
    if cli.tui {
        let db_path = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        return poler_engine::shell::run_tui(db_path);
    }

    // ---------- License / EULA: статус модели ----------
    if cli.license {
        return print_license_status();
    }

    // ---------- Веб-индекс: статистика ----------
    if cli.web_stats {
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        return match poler_engine::web::WebIndex::open(&db) {
            Ok(ix) => {
                let mut st = match ix.stats() {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("poler-engine: {e}");
                        return ExitCode::from(2);
                    }
                };
                st.db_bytes = std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0);
                println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("poler-engine: web-индекс {db:?}: {e}");
                ExitCode::from(2)
            }
        };
    }

    // ---------- Benchmark & Regression Suite (v0.21, Задача 4) ----------
    if cli.benchmark {
        let opts = poler_engine::bench::BenchOpts::default();
        match poler_engine::bench::run_suite(&opts) {
            Ok(res) => {
                print_bench_report(&res);
                if let Some(path) = &cli.benchmark_json {
                    match serde_json::to_string_pretty(&res) {
                        Ok(json) => match std::fs::write(path, json) {
                            Ok(()) => eprintln!("poler-engine: бенчмарк-отчёт → {}", path.display()),
                            Err(e) => {
                                eprintln!("poler-engine: запись {}: {e}", path.display());
                                return ExitCode::from(2);
                            }
                        },
                        Err(e) => {
                            eprintln!("poler-engine: сериализация отчёта: {e}");
                            return ExitCode::from(2);
                        }
                    }
                }
                // exit-код: golden-проверки полноты и моста обязаны быть зелёными
                return if res.exact.completeness_ok && res.lexical.golden_ok {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                };
            }
            Err(e) => {
                eprintln!("poler-engine benchmark: {e}");
                return ExitCode::from(2);
            }
        }
    }

    // ---------- Semantic Bridge: диагностика расширения запроса ----------
    if let Some(query) = cli.semantic_expand.clone() {
        if query.trim().is_empty() {
            eprintln!("poler-engine: пустой --semantic-expand");
            return ExitCode::from(2);
        }
        let bridge = poler_engine::retrieval::SemanticBridge::offline();
        let terms = poler_engine::web::stem::tokenize_stem(&query);
        let expansion = bridge.expand(&terms);
        println!("Semantic Bridge — офлайн-сенсор кросс-языковых запросов (v0.21)");
        println!("Запрос: «{query}»");
        println!("Термы (стем-форма): {}", terms.join(", "));
        println!();
        if expansion.is_empty() {
            println!("Расширений нет: сенсор не знает этих термов —");
            println!("ранжирование останется чистым WebRank без кандидатов.");
        } else {
            println!("Расширения ({}):", expansion.expansions.len());
            for line in expansion.why_lines() {
                println!("  {line}");
            }
            println!();
            println!("{}", expansion.why());
        }
        return ExitCode::SUCCESS;
    }

    // ---------- Веб-поиск: по локальному веб-индексу ----------
    if let Some(query) = cli.web_search.clone() {
        if query.trim().is_empty() {
            eprintln!("poler-engine: пустой --web-search");
            return ExitCode::from(2);
        }
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let mut ix = match poler_engine::web::WebIndex::open(&db) {
            Ok(ix) => ix,
            Err(e) => {
                eprintln!("poler-engine: web-индекс {db:?}: {e}");
                return ExitCode::from(2);
            }
        };
        let bridge = poler_engine::retrieval::SemanticBridge::offline();
        let (hits, expansion) = match ix.search_with_bridge(&query, cli.top.max(1), &bridge) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("poler-engine: web-search: {e}");
                return ExitCode::from(2);
            }
        };
        if cli.verbose {
            eprintln!(
                "poler-engine web-search: «{query}» — {} результатов из {} страниц{}",
                hits.len(),
                ix.page_count(),
                if expansion.is_empty() {
                    String::new()
                } else {
                    format!(", bridge: +{} терма-кандидата", expansion.extra_terms().len())
                }
            );
        }
        print_web_hits(&hits, &query, cli.format, Some(&expansion));
        return if hits.is_empty() {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        };
    }

    // ---------- Краулинг: seed URL → веб-индекс ----------
    if cli.crawl {
        let Some(seed_path) = cli.path.clone() else {
            eprintln!("poler-engine: --crawl требует seed URL как PATH");
            return ExitCode::from(2);
        };
        let seed = seed_path.to_string_lossy().to_string();
        if !seed.starts_with("http://") && !seed.starts_with("https://") {
            eprintln!("poler-engine: --crawl ожидает URL (http(s)://...), получено: {seed}");
            return ExitCode::from(2);
        }
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let mut ix = match poler_engine::web::WebIndex::open(&db) {
            Ok(ix) => ix,
            Err(e) => {
                eprintln!("poler-engine: web-индекс {db:?}: {e}");
                return ExitCode::from(2);
            }
        };
        let mut fetcher = match poler_engine::web::cdp_fetcher_with_timeout(
            cli.cdp_port,
            cli.web_wait_ms,
            cli.crawl_page_timeout_ms,
        ) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-engine: Chromium CDP (порт {}): {e}", cli.cdp_port);
                eprintln!("  автозапуск не удался: установите POLER_CHROME_BIN или запустите вручную:");
                eprintln!("  chrome --headless --remote-debugging-port={} --no-sandbox", cli.cdp_port);
                return ExitCode::from(2);
            }
        };
        let cfg = poler_engine::web::CrawlConfig {
            max_pages: cli.crawl_max.max(1),
            max_depth: cli.crawl_depth,
            delay_ms: cli.crawl_delay_ms,
            cross_site: cli.cross_site,
            wait_ms: cli.web_wait_ms,
            page_timeout_ms: cli.crawl_page_timeout_ms,
            respect_robots: true,
        };
        eprintln!("poler-crawl: seed {seed}, глубина ≤ {}, до {} страниц, база {db:?}", cfg.max_depth, cfg.max_pages);
        let stats = match poler_engine::web::crawl::crawl(&mut ix, &mut fetcher, &seed, &cfg, cli.verbose) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("poler-crawl: {e}");
                return ExitCode::from(2);
            }
        };
        println!("{}", serde_json::to_string_pretty(&stats).unwrap_or_default());
        eprintln!(
            "poler-crawl: готово — {} загружено, {} проиндексировано, {} без изменений, {} дубликатов, {} robots-запретов, {} ошибок, {} мс",
            stats.fetched,
            stats.indexed,
            stats.unchanged,
            stats.duplicates,
            stats.skipped_robots,
            stats.errors,
            stats.elapsed_ms
        );
        return ExitCode::SUCCESS;
    }

    // ---------- Индексация одной страницы: --browser-index <URL> ----------
    if let Some(url) = cli.browser_index.clone() {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            eprintln!("poler-engine: --browser-index ожидает URL (http(s)://...), получено: {url}");
            return ExitCode::from(2);
        }
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let mut ix = match poler_engine::web::WebIndex::open(&db) {
            Ok(ix) => ix,
            Err(e) => {
                eprintln!("poler-engine: web-индекс {db:?}: {e}");
                return ExitCode::from(2);
            }
        };
        let mut fetcher = match poler_engine::web::cdp_fetcher_with_timeout(
            cli.cdp_port,
            cli.web_wait_ms,
            cli.crawl_page_timeout_ms,
        ) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-engine: Chromium CDP (порт {}): {e}", cli.cdp_port);
                return ExitCode::from(2);
            }
        };
        // одиночная страница: глубина 0; явная команда пользователя —
        // robots не блокирует, но честно фиксируется в notes
        let cfg = poler_engine::web::CrawlConfig {
            max_pages: 1,
            max_depth: 0,
            delay_ms: cli.crawl_delay_ms,
            cross_site: false,
            wait_ms: cli.web_wait_ms,
            page_timeout_ms: cli.crawl_page_timeout_ms,
            respect_robots: false,
        };
        let stats = match poler_engine::web::crawl::crawl(&mut ix, &mut fetcher, &url, &cfg, false) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("poler-browser-index: {e}");
                return ExitCode::from(2);
            }
        };
        println!("{}", serde_json::to_string_pretty(&stats).unwrap_or_default());
        for n in stats.notes.iter().take(5) {
            eprintln!("  • {n}");
        }
        if stats.indexed == 1 {
            eprintln!("poler-browser-index: страница в индексе — poler-engine --web-search \"запрос\"");
        } else if stats.unchanged == 1 {
            eprintln!("poler-browser-index: уже в индексе, контент не менялся (Percolator-lite)");
        } else if stats.duplicates == 1 {
            eprintln!("poler-browser-index: near-дубликат уже известной страницы (SimHash)");
        } else {
            eprintln!(
                "poler-browser-index: страница НЕ попала в индекс (ошибок: {}) — см. notes выше",
                stats.errors
            );
            return ExitCode::from(1);
        }
        return ExitCode::SUCCESS;
    }

    let Some(path_arg) = cli.path.clone() else {
        // Интерактивный терминал (и stdin, и stdout — TTY) → сразу TUI-дашборд.
        // Если хоть один поток пайп/редирект — честная справка (скрипты, CI, docker).
        let interactive = std::io::IsTerminal::is_terminal(&std::io::stdin())
            && std::io::IsTerminal::is_terminal(&std::io::stdout());
        if interactive {
            let db_path = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
            return poler_engine::shell::run_tui(db_path);
        }
        eprintln!("poler-engine: укажите PATH, --tui, --web-search <QUERY> или --crawl с seed URL");
        return ExitCode::from(2);
    };

    // ---------- Web-Native режим: рендер через Chromium CDP ----------
    let scan_target: PathBuf = if cli.web {
        let url = path_arg.to_string_lossy().to_string();
        if !url.starts_with("http://") && !url.starts_with("https://") {
            eprintln!("poler-engine: --web ожидает URL (http(s)://...), получено: {url}");
            return ExitCode::from(2);
        }
        if let Err(e) = poler_engine::web::ensure_chromium(cli.cdp_port) {
            eprintln!("poler-engine web: {e}");
            return ExitCode::from(2);
        }
        match poler_engine::web::ingest_url(&url, cli.cdp_port, cli.web_wait_ms) {
            Ok(res) => {
                if cli.verbose {
                    eprintln!(
                        "poler-engine web: «{}» — {} байт текста, {} JSON API перехвачено",
                        res.title,
                        res.text_len,
                        res.json_files.len()
                    );
                }
                // директория кэша: сканируем текст страницы + все JSON
                poler_engine::web::web_cache_dir()
            }
            Err(e) => {
                eprintln!("poler-engine web: {e}");
                return ExitCode::from(2);
            }
        }
    } else {
        path_arg
    };

    if !cli.web && !scan_target.exists() {
        eprintln!("poler-engine: путь не найден: {}", scan_target.display());
        return ExitCode::from(2);
    }

    let config = EngineConfig {
        window_radius: cli.window,
        phi_decay: cli.phi,
        kappa: cli.kappa,
        top_n: cli.top,
        k_hop_depth: cli.k_hop,
        pii_mode: match cli.pii {
            PiiArg::Off => PiiMode::Off,
            PiiArg::Mask => PiiMode::Mask,
        },
        resonance_mode: match cli.resonance {
            ResonanceArg::Hits => ResonanceMode::Hits,
            ResonanceArg::Field => ResonanceMode::Field,
            ResonanceArg::Psi => ResonanceMode::Psi,
            ResonanceArg::Poler => ResonanceMode::Poler,
        },
        temporal_filter: cli.metric.clone(),
        local_stats: cli.local_stats,
        extensions: cli
            .extensions
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect(),
        max_file_bytes: cli.max_file_mb.saturating_mul(1024 * 1024),
        max_scope_bytes: cli.max_scope,
        max_relations: cli.max_relations,
        include_hidden: cli.hidden,
        graph_export: cli.graph_export.clone(),
        max_graph_triples: cli.max_graph_triples,
        psi_params: poler_engine::psi::PsiParams {
            eta: cli.psi_eta,
            gamma: cli.psi_gamma,
            rho: cli.psi_rho,
            memory_depth: cli.psi_depth,
        },
        poler_params: poler_engine::poler::PolerParams {
            eta: cli.poler_eta,
            gamma: cli.poler_gamma,
            mix: cli.poler_mix,
            dissipator: cli.poler_dissipator,
            ..poler_engine::poler::PolerParams::default()
        },
    };

    // ---------- Режим AIDDE: Impact Passport ----------
    if let Some(symbol) = &cli.impact {
        if cli.query.is_some() {
            eprintln!("poler-engine: --impact и --query взаимоисключающие");
            return ExitCode::from(2);
        }
        let files: Vec<PathBuf> = collect_files(&scan_target, &config)
            .into_iter()
            .filter(|p| poler_engine::detect_lang(p) != CodeLang::Plain)
            .collect();
        if cli.verbose {
            eprintln!("poler-engine AIDDE: кодовых файлов: {}", files.len());
        }
        // Два режима: ин-мемори (по умолчанию) или SQLite-хранилище
        // (--impact-cache path.db — для кодовых баз 65K+ файлов).
        if let Some(db_path) = &cli.impact_cache {
            // reuse-режим: существующая база не перестраивается
            let store = if cli.impact_reuse && db_path.exists() {
                match poler_engine::aidde::SymbolStore::open_existing(db_path) {
                    Ok((s, has_schema)) => {
                        if has_schema {
                            if cli.verbose {
                                eprintln!("poler-engine AIDDE: reuse базы {db_path:?}");
                            }
                            Some(s)
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            } else {
                None
            };
            let store = match store {
                Some(s) => s,
                None => {
                    let mut s = match poler_engine::aidde::SymbolStore::open(db_path) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("poler-engine: не удалось открыть базу {db_path:?}: {e}");
                            return ExitCode::from(2);
                        }
                    };
                    if let Err(e) = s.build(&files, config.max_file_bytes) {
                        eprintln!("poler-engine: ошибка построения таблицы: {e}");
                        return ExitCode::from(2);
                    }
                    s
                }
            };
            if cli.verbose {
                let (d, c) = store.stats();
                eprintln!("poler-engine AIDDE(sqlite): defs={d}, calls={c}");
            }
            match poler_engine::aidde::impact_analysis_sqlite(
                &store,
                symbol,
                cli.impact_depth,
                200,
            ) {
                Some(report) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("poler-engine: символ не найден: {symbol}");
                    ExitCode::from(1)
                }
            }
        } else {
            let table = SymbolTable::build(&files, config.max_file_bytes);
            match impact_analysis(&table, symbol, cli.impact_depth, 200) {
                Some(report) => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).unwrap_or_default()
                    );
                    ExitCode::SUCCESS
                }
                None => {
                    eprintln!("poler-engine: символ не найден: {symbol}");
                    ExitCode::from(1)
                }
            }
        }
    } else {
        // ---------- Режим поиска ----------
        let Some(query) = cli.query.clone() else {
            eprintln!("poler-engine: укажите --query <QUERY> или --impact <SYMBOL>");
            return ExitCode::from(2);
        };
        if query.trim().is_empty() {
            eprintln!("poler-engine: пустой запрос");
            return ExitCode::from(2);
        }

        if cli.watch {
            return watch_mode(cli, config, query, scan_target);
        }

        let mut engine = Engine::new(config, false);
        let (result, stats) = engine.scan(&scan_target, &query);
        if cli.verbose {
            eprintln!(
                "poler-engine: файлов просканировано={}, с совпадениями={}, токенов={}, \
                 хитов={}, узлов графа={}, рёбер={}, время={}мс",
                stats.files_scanned,
                stats.files_with_hits,
                stats.total_tokens,
                stats.total_hits,
                stats.graph_nodes,
                stats.graph_edges,
                stats.elapsed_ms
            );
        }
        print_result(&result, cli.format);
        if result.total_hits == 0 {
            ExitCode::from(1)
        } else {
            ExitCode::SUCCESS
        }
    }
}

/// Watcher-режим: первичный полный скан, затем инкрементальные rescans
/// по mtime/size; выход по Ctrl-C (SIGINT).
fn watch_mode(cli: Cli, config: EngineConfig, query: String, scan_target: PathBuf) -> ExitCode {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = stop.clone();
        let _ = ctrlc::set_handler(move || stop.store(true, Ordering::SeqCst));
    }

    let mut engine = Engine::new(config, true).with_diff(cli.diff);
    let (result, stats) = engine.scan(&scan_target, &query);
    if cli.verbose {
        eprintln!(
            "poler-engine watch: начальный скан — файлов={}, хитов={}, время={}мс",
            stats.files_scanned, stats.total_hits, stats.elapsed_ms
        );
    }
    print_result(&result, cli.format);

    let interval = cli.interval.max(1);
    while !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_secs(interval));
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let (event, result, stats) = engine.rescan(&scan_target, &query);
        if event.is_empty() {
            continue;
        }
        eprintln!(
            "poler-engine watch: добавлено={}, изменено={}, удалено={} (файлов={}, хитов={}, время={}мс)",
            event.added.len(),
            event.changed.len(),
            event.removed.len(),
            stats.files_scanned,
            stats.total_hits,
            stats.elapsed_ms
        );
        print_result(&result, cli.format);
    }
    eprintln!("poler-engine watch: остановлено");
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// v2.0 Part E/F: обработчики суверенного ML-инференса (pqc)
// ---------------------------------------------------------------------------

/// `--semantic dense --model X.pqw -q "…"`: нативный энкодер (BGE-M3-класс)
/// через pqc — mmap + Sha256 + weight-only int8/int4, без ONNX.
/// С `--semantic-corpus PATH` — живой семантический поиск: чанки корпуса
/// эмбеддятся тем же энкодером, ранжирование косинусом.
fn run_semantic_native(mode: &str, cli: &Cli) -> i32 {
    if mode != "dense" {
        eprintln!(
            "poler-engine: неизвестный --semantic режим {mode:?} (доступен: dense)"
        );
        return 2;
    }
    let Some(model_path) = &cli.model else {
        eprintln!(
            "poler-engine: --semantic dense требует --model <path.pqw>\n  \
             конвертер реальных весов: python3 scripts/convert_hf_to_pqw.py \
             --hf-dir <BGE-M3> --out models/bge-m3.pqw;\n  \
             проверить стек сейчас: poler-engine --pqw-selftest"
        );
        return 2;
    };
    let Some(query) = &cli.query else {
        eprintln!("poler-engine: --semantic dense требует -q <текст>");
        return 2;
    };
    use poler_engine::vectors::Embedder as _;
    let mut embedder = match poler_engine::vectors::pqw_bridge::PqwEmbedder::open(model_path) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };

    if let Some(corpus) = &cli.semantic_corpus {
        return run_semantic_corpus_search(
            &mut embedder,
            corpus,
            query,
            cli.semantic_limit,
            cli.semantic_max_chunks,
        );
    }

    let t0 = std::time::Instant::now();
    match embedder.embed_batch(&[query.as_str()]) {
        Ok(vs) => {
            let v = &vs[0];
            let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            let head: Vec<String> = v.iter().take(8).map(|x| format!("{x:.4}")).collect();
            let tok_info = match embedder.tokenizer() {
                Some(tk) => format!("unigram-токенизатор: {} кусков", tk.vocab_size()),
                None => "хэш-фолбэк (демо-модель без __tokenizer__)".to_string(),
            };
            println!(
                "pqw-native dense · dim={} · L2={norm:.4} · [{}] …",
                v.len(),
                head.join(" ")
            );
            println!(
                "модель: {} · {:.1} мс · {tok_info}",
                embedder.name(),
                t0.elapsed().as_secs_f64() * 1000.0
            );
            0
        }
        Err(e) => {
            eprintln!("poler-engine: {e}");
            2
        }
    }
}

/// Живой семантический поиск: корпус → чанки → эмбеддинги → косинус.
fn run_semantic_corpus_search(
    embedder: &mut poler_engine::vectors::pqw_bridge::PqwEmbedder,
    corpus: &std::path::Path,
    query: &str,
    limit: usize,
    max_chunks: usize,
) -> i32 {
    use poler_engine::vectors::Embedder as _;

    let t0 = std::time::Instant::now();
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if !collect_text_files(corpus, &mut files, 4096) {
        return 2;
    }
    if files.is_empty() {
        eprintln!("poler-engine: корпус пуст (текстовых файлов не найдено)");
        return 2;
    }

    // Чанки: абзацы склеиваются до ~400 символов, длинные режутся.
    let mut chunks: Vec<(String, String)> = Vec::new(); // (file, text)
    for f in &files {
        let Ok(text) = std::fs::read_to_string(f) else { continue };
        for piece in text.split("\n\n") {
            let p = piece.trim();
            if p.chars().count() < 40 {
                continue; // мусорные осколки пропускаем
            }
            if p.chars().count() <= 380 {
                chunks.push((f.display().to_string(), p.to_string()));
            } else {
                // жёсткая нарезка длинных абзацев (~150 токенов на чанк)
                let mut start = 0usize;
                let cs: Vec<char> = p.chars().collect();
                while start < cs.len() {
                    let end = (start + 340).min(cs.len());
                    let s: String = cs[start..end].iter().collect();
                    chunks.push((f.display().to_string(), s));
                    start = end;
                }
            }
        }
    }
    if chunks.is_empty() {
        eprintln!("poler-engine: в корпусе нет абзацев достаточной длины");
        return 2;
    }
    // Равномерная выборка до max_chunks (корпус может быть огромным).
    if chunks.len() > max_chunks {
        let step = chunks.len() as f64 / max_chunks as f64;
        let picked: Vec<(String, String)> = (0..max_chunks)
            .map(|i| chunks[(i as f64 * step) as usize].clone())
            .collect();
        eprintln!(
            "poler-engine: {} чанков → равномерная выборка {} (--semantic-max-chunks)",
            chunks.len(),
            max_chunks
        );
        chunks = picked;
    }

    eprintln!(
        "корпус: {} файлов → {} чанков · эмбеддинг…",
        files.len(),
        chunks.len()
    );
    // Параллельно по чанкам (rayon): токенизация + forward — оба &self,
    // mmap-веса разделяются всеми потоками.
    let emb: &poler_engine::vectors::pqw_bridge::PqwEmbedder = &*embedder;
    let cap = emb.model().view().header().max_pos as usize;
    let cap = cap.saturating_sub(if emb.model().view().header().xlmr_positions() {
        2
    } else {
        0
    });
    let vectors: Vec<Vec<f32>> = {
        use rayon::prelude::*;
        let done = std::sync::atomic::AtomicUsize::new(0);
        let total = chunks.len();
        let r = chunks
            .par_iter()
            .map(|(_, t)| {
                let n = done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 25 == 0 {
                    eprintln!("  эмбеддинг: {n}/{total}");
                }
                let mut ids = emb.token_ids(t);
                if ids.len() > cap {
                    ids.truncate(cap);
                }
                emb.model().embed(&ids)
            })
            .collect::<Result<Vec<_>, String>>();
        match r {
            Ok(v) => v,
            Err(e) => {
                eprintln!("poler-engine: {e}");
                return 2;
            }
        }
    };
    let qv = match embedder.embed_batch(&[query]) {
        Ok(v) => v.into_iter().next().unwrap(),
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };

    let mut ranked: Vec<(usize, f32)> = vectors
        .iter()
        .enumerate()
        .map(|(i, v)| (i, v.iter().zip(&qv).map(|(a, b)| a * b).sum::<f32>()))
        .collect();
    ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    let dt = t0.elapsed().as_secs_f64();
    println!(
        "\nсемантический поиск · «{query}» · {} чанков за {dt:.1} с ({:.1} чанков/с)",
        chunks.len(),
        chunks.len() as f64 / dt.max(1e-9)
    );
    for (rank, &(i, score)) in ranked.iter().take(limit.max(1)).enumerate() {
        let (file, text) = &chunks[i];
        let snippet: String = text.chars().take(140).collect();
        println!(
            "\n{}. [cos {:.4}] {}",
            rank + 1,
            score,
            file
        );
        println!("   {snippet}…");
    }
    0
}

// ---------------------------------------------------------------------------
// Суверенный Гиппокамп (v0.29): раннеры CLI
// ---------------------------------------------------------------------------

/// БД знаний: --knowledge-db | POLER_KNOWLEDGE_DB | ~/.local/share/…/knowledge.db.
fn knowledge_db_of(cli: &Cli) -> std::path::PathBuf {
    cli.knowledge_db
        .clone()
        .unwrap_or_else(poler_engine::sources::knowledge::default_db_path)
}

/// `--knowledge-ingest <PATH>`: библиотека POLER → нативный индекс.
fn run_knowledge_ingest(cli: &Cli, root: &std::path::Path) -> i32 {
    use poler_engine::sources::knowledge::{self, IngestOptions, KnowledgeEmbedder};

    let mode = match cli.knowledge_embedder {
        KnowledgeEmbedderArg::None => "none",
        KnowledgeEmbedderArg::Hash => "hash",
        KnowledgeEmbedderArg::Pqw => "pqw",
    };
    let mut embedder = match KnowledgeEmbedder::from_mode(mode, cli.model.as_deref()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };
    let db = knowledge_db_of(cli);
    eprintln!(
        "poler-knowledge: инжест {:?} → {:?} (эмбеддер: {mode})",
        root, db
    );
    match knowledge::ingest(root, &db, &mut embedder, &IngestOptions::default()) {
        Ok(rep) => {
            print!("{}", rep.render_text());
            eprintln!(
                "поиск: poler-engine --knowledge-search \"…\" [--min-provenance mvr] \
                 [--knowledge-db {:?}]",
                db
            );
            0
        }
        Err(e) => {
            eprintln!("poler-engine: {e}");
            2
        }
    }
}

/// `--knowledge-search <QUERY>`: гибридный поиск с эпистемической градацией.
fn run_knowledge_search(cli: &Cli, query: &str) -> i32 {
    use poler_engine::retrieval::Provenance;
    use poler_engine::sources::knowledge::{self, QueryOptions};

    let db = knowledge_db_of(cli);
    let min_prov = match cli.min_provenance.as_deref() {
        None => None,
        Some(s) => match Provenance::parse(s) {
            Some(p) => Some(p),
            None => {
                eprintln!(
                    "poler-engine: неизвестный --min-provenance {s:?} \
                     (доступно: mvr | source | narrative)"
                );
                return 2;
            }
        },
    };
    // Векторное русло: восстанавливаем эмбеддер по мета индекса; для .pqw
    // нужен --model — без него ищем чистым BM25 (честная деградация).
    let mut embedder = match knowledge::query_embedder(&db, cli.model.as_deref()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            None
        }
    };
    let opts = QueryOptions { top: cli.top.max(1), min_provenance: min_prov, ..Default::default() };
    match knowledge::query(&db, query, &opts, embedder.as_mut()) {
        Ok(out) => {
            print!("{}", knowledge::render_query_text(query, &out));
            if out.hits.is_empty() {
                1
            } else {
                0
            }
        }
        Err(e) => {
            eprintln!("poler-engine: {e}");
            2
        }
    }
}

/// `--knowledge-stats`: JSON-статистика индекса знаний.
fn run_knowledge_stats(cli: &Cli) -> i32 {
    let db = knowledge_db_of(cli);
    match poler_engine::sources::knowledge::stats(&db) {
        Ok(s) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&s).unwrap_or_else(|_| "{}".into())
            );
            0
        }
        Err(e) => {
            eprintln!("poler-engine: {e}");
            2
        }
    }
}

// ── Крипто-слой данных: POLER Vault (M4.5 CDL, фича pnd-ffi) ────────────────

// ---------------------------------------------------------------------------
// v0.28.1: Архивы без распаковки — CLI-хелперы
// ---------------------------------------------------------------------------

/// `--archive-list <ARCHIVE>`: листинг записей контейнера без распаковки
/// и без пароля (метаданные читаются raw из центрального каталога /
/// заголовков tar). `--archive-json` — машинно-читаемая форма для агента.
fn run_archive_list(cli: &Cli, archive: &std::path::Path) -> i32 {
    use poler_engine::archive;

    let info = match archive::open_info(archive) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("poler-archive: {e}");
            return 2;
        }
    };
    let files = info.entries.iter().filter(|e| !e.is_dir).count();
    let dirs = info.entries.len() - files;
    let encrypted = info.entries.iter().filter(|e| e.encrypted).count();
    if cli.archive_json {
        let listing = serde_json::json!({
            "archive": info.path.display().to_string(),
            "kind": info.kind.as_str(),
            "entries_total": info.entries.len(),
            "entries_files": files,
            "entries_dirs": dirs,
            "entries_encrypted": encrypted,
            "entries": info.entries.iter().map(|e| serde_json::json!({
                "name": e.name,
                "size": e.size,
                "compressed": e.compressed,
                "is_dir": e.is_dir,
                "encrypted": e.encrypted,
            })).collect::<Vec<_>>(),
        });
        match serde_json::to_string_pretty(&listing) {
            Ok(json) => println!("{json}"),
            Err(e) => {
                eprintln!("poler-archive: сериализация JSON: {e}");
                return 2;
            }
        }
    } else {
        println!("Архив:  {}", info.path.display());
        println!(
            "Тип:    {} · записей: {} (файлов: {}, каталогов: {}) · зашифрованных: {}",
            info.kind.as_str(),
            info.entries.len(),
            files,
            dirs,
            encrypted
        );
        if encrypted > 0 {
            println!(
                "Пароль: --archive-password <PASS> либо env POLER_ARCHIVE_KEY \
                 (выводится промптом на TTY)"
            );
        }
        println!("{:>12}  {:>12}  {:<4}  {}", "размер", "сжатие", "шифр", "имя");
        for e in &info.entries {
            println!(
                "{:>12}  {:>12}  {:<4}  {}{}",
                e.size,
                if e.compressed > 0 {
                    e.compressed.to_string()
                } else {
                    "-".to_string()
                },
                if e.encrypted { "да" } else { "-" },
                e.name,
                if e.is_dir { "/" } else { "" }
            );
        }
    }
    0
}

// ---------- v0.30.0: Коннектом FLYCSR1 — мозг мухи как матрица A ----------

/// Разделение разрядов: 54492922 → «54 492 922».
fn thou(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let bytes = s.as_bytes();
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && (bytes.len() - i) % 3 == 0 {
            out.push(' ');
        }
        out.push(*b as char);
    }
    out
}

/// Парсинг направления для режимов --connectome-neighbors/--connectome-common.
fn ct_dir(s: &str) -> Result<poler_engine::graph::flyops::Direction, String> {
    poler_engine::graph::flyops::Direction::parse(s)
}

/// Строка ребра для человека: «79529 (root ...) w=17 gaba (-1)».
fn fmt_con_edge(
    e: &poler_engine::graph::connectome::Edge,
    nodes: &Option<poler_engine::graph::connectome::ConnectomeNodes>,
) -> String {
    let who = match nodes {
        Some(ns) => match ns.root_id(e.target as usize) {
            Some(r) => format!("{} (root {})", e.target, r),
            None => e.target.to_string(),
        },
        None => e.target.to_string(),
    };
    let sign = match e.sign() {
        1 => "+1",
        -1 => "-1",
        _ => "0",
    };
    format!("{who} w={} {} ({sign})", e.weight, e.nt_name())
}

/// JSON-объект ребра (цель/источник + вес/медиатор/знак).
fn con_edge_json(
    e: &poler_engine::graph::connectome::Edge,
    nodes: &Option<poler_engine::graph::connectome::ConnectomeNodes>,
) -> serde_json::Value {
    serde_json::json!({
        "other": e.target,
        "other_root_id": nodes.as_ref().and_then(|n| n.root_id(e.target as usize)),
        "weight": e.weight,
        "nt": e.nt_name(),
        "sign": e.sign(),
        "signed_weight": e.signed_weight(),
    })
}

fn run_connectome(cli: &Cli, csr_path: &std::path::Path) -> i32 {
    use poler_engine::graph::connectome as ct;

    let t0 = std::time::Instant::now();
    let con = match ct::Connectome::load(csr_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("poler-connectome: {e}");
            return 2;
        }
    };
    let load_ms = t0.elapsed().as_millis();

    let nodes = match &cli.connectome_nodes {
        Some(p) => match ct::ConnectomeNodes::load(p) {
            Ok(n) => {
                if n.len() != con.n_nodes() {
                    eprintln!(
                        "poler-connectome: {} содержит {} root_id, а коннектом ждёт {} узлов",
                        p.display(),
                        n.len(),
                        con.n_nodes()
                    );
                    return 2;
                }
                Some(n)
            }
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        },
        None => None,
    };

    // Режимы взаимно исключают друг друга.
    let modes = [
        cli.connectome_node.is_some(),
        cli.connectome_edge.is_some(),
        cli.connectome_khop.is_some(),
        cli.connectome_impact.is_some(),
        cli.connectome_neighbors.is_some(),
        cli.connectome_path.is_some(),
        cli.connectome_common.is_some(),
        cli.connectome_centrality,
        cli.connectome_rotor_top.is_some(),
        cli.connectome_motifs.is_some(),
        cli.connectome_propagate.is_some(),
    ];
    if modes.iter().filter(|&&b| b).count() > 1 {
        eprintln!(
            "poler-connectome: укажите один режим (--connectome-node | --connectome-edge | \
             --connectome-khop | --connectome-impact | --connectome-neighbors | \
             --connectome-path | --connectome-common | --connectome-centrality | \
             --connectome-rotor-top | --connectome-motifs | --connectome-propagate); \
             без режима — сводка"
        );
        return 2;
    }

    // Резолв нейрона: индекс (< n_nodes) либо root_id (с --connectome-nodes).
    let resolve = |spec: &str| -> Result<usize, String> {
        if let Ok(idx) = spec.parse::<usize>() {
            if idx < con.n_nodes() {
                return Ok(idx);
            }
        }
        if let Some(ns) = &nodes {
            if let Ok(rid) = spec.parse::<u64>() {
                if let Some(i) = ns.idx_of(rid) {
                    return Ok(i);
                }
            }
        }
        Err(format!(
            "нейрон «{spec}» не найден (узлов: {}; формат: индекс 0..{} либо root_id с --connectome-nodes)",
            con.n_nodes(),
            con.n_nodes().saturating_sub(1)
        ))
    };

    let json = cli.connectome_json;

    // ---------- Режим: паспорт нейрона ----------
    if let Some(spec) = &cli.connectome_node {
        let idx = match resolve(spec) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let outs: Vec<ct::Edge> = con.out_edges(idx).unwrap().collect();
        let out_mass: u64 = outs.iter().map(|e| e.weight as u64).sum();
        let csc = con.build_in_edges();
        let ins: Vec<ct::Edge> = csc.in_edges(&con, idx).unwrap().collect();
        let in_mass: u64 = ins.iter().map(|e| e.weight as u64).sum();
        let mut top_out = outs.clone();
        top_out.sort_by_key(|e| std::cmp::Reverse(e.weight));
        top_out.truncate(5);
        let mut top_in = ins.clone();
        top_in.sort_by_key(|e| std::cmp::Reverse(e.weight));
        top_in.truncate(5);
        if json {
            let j = serde_json::json!({
                "mode": "node",
                "artifact": csr_path.display().to_string(),
                "core": con.is_core(),
                "neuron": idx,
                "root_id": nodes.as_ref().and_then(|n| n.root_id(idx)),
                "out_degree": outs.len(),
                "out_mass": out_mass,
                "in_degree": ins.len(),
                "in_mass": in_mass,
                "top_out": top_out.iter().map(|e| con_edge_json(e, &nodes)).collect::<Vec<_>>(),
                "top_in": top_in.iter().map(|e| con_edge_json(e, &nodes)).collect::<Vec<_>>(),
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!("Нейрон {}:", idx);
            println!(
                "  исходящих: {} (масса {}) · входящих: {} (масса {})",
                outs.len(),
                thou(out_mass),
                ins.len(),
                thou(in_mass)
            );
            if !top_out.is_empty() {
                let list = top_out
                    .iter()
                    .map(|e| fmt_con_edge(e, &nodes))
                    .collect::<Vec<_>>()
                    .join(" · ");
                println!("  топ исходящие: {list}");
            }
            if !top_in.is_empty() {
                let list = top_in
                    .iter()
                    .map(|e| fmt_con_edge(e, &nodes))
                    .collect::<Vec<_>>()
                    .join(" · ");
                println!("  топ входящие:  {list}");
            }
        }
        return 0;
    }

    // ---------- Режим: ребро U:V + ротор J = A − Aᵀ ----------
    if let Some(spec) = &cli.connectome_edge {
        let (us, vs) = match spec.split_once(':') {
            Some(pair) => pair,
            None => {
                eprintln!(
                    "poler-connectome: --connectome-edge ждёт формат U:V, получено «{spec}»"
                );
                return 2;
            }
        };
        let u = match resolve(us) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let v = match resolve(vs) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let fwd = con.edge(u, v);
        let bwd = con.edge(v, u);
        if fwd.is_none() && bwd.is_none() {
            if json {
                let j = serde_json::json!({
                    "mode": "edge", "artifact": csr_path.display().to_string(),
                    "u": u, "v": v, "found": false,
                    "forward": serde_json::Value::Null,
                    "backward": serde_json::Value::Null,
                    "rotor": 0,
                });
                println!("{}", serde_json::to_string_pretty(&j).unwrap_or_default());
            } else {
                eprintln!("poler-connectome: связи {u} -> {v} нет (в обоих направлениях)");
            }
            return 1;
        }
        let rotor = con.rotor(u, v);
        if json {
            let j = serde_json::json!({
                "mode": "edge",
                "artifact": csr_path.display().to_string(),
                "core": con.is_core(),
                "u": u,
                "u_root_id": nodes.as_ref().and_then(|n| n.root_id(u)),
                "v": v,
                "v_root_id": nodes.as_ref().and_then(|n| n.root_id(v)),
                "found": true,
                "forward": fwd.map(|e| con_edge_json(&e, &nodes)),
                "backward": bwd.map(|e| con_edge_json(&e, &nodes)),
                "rotor_Juu": rotor, // J[u][v] = A(u,v) − A(v,u); J[v][u] = −rotor
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            match fwd {
                Some(e) => println!("Ребро {u} -> {v}: {}", fmt_con_edge(&e, &nodes)),
                None => println!("Ребро {u} -> {v}: связи нет"),
            }
            match bwd {
                Some(e) => println!("Обратное {v} -> {u}: {}", fmt_con_edge(&e, &nodes)),
                None => println!("Обратное {v} -> {u}: связи нет"),
            }
            match (fwd, bwd) {
                (Some(_), Some(_)) => println!(
                    "J = A − Aᵀ: J[{u}][{v}] = {rotor:+} (реципрокная пара, циркуляция {rotor})"
                ),
                _ => println!(
                    "J = A − Aᵀ: J[{u}][{v}] = {rotor:+}, J[{v}][{u}] = {:-} — однонаправленный поток",
                    -rotor
                ),
            }
        }
        return 0;
    }

    // ---------- Режим: K-hop BFS потока сигнала ----------
    if let Some(spec) = &cli.connectome_khop {
        let idx = match resolve(spec) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let filter = match ct::SignFilter::parse(&cli.connectome_sign) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let r = con.k_hop(idx, cli.k_hop, filter);
        let frontiers = r
            .frontier_sizes
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if json {
            let j = serde_json::json!({
                "mode": "khop",
                "artifact": csr_path.display().to_string(),
                "core": con.is_core(),
                "start": idx,
                "start_root_id": nodes.as_ref().and_then(|n| n.root_id(idx)),
                "depth": cli.k_hop,
                "sign_filter": cli.connectome_sign,
                "frontier_sizes": r.frontier_sizes,
                "visited": r.visited,
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            let label = match filter {
                ct::SignFilter::All => "все связи".to_string(),
                ct::SignFilter::Excitatory => "только возбуждающие (+1)".to_string(),
                ct::SignFilter::Inhibitory => "только тормозные (-1)".to_string(),
            };
            println!(
                "K-hop от {idx} (глубина {}): фронты [{frontiers}], достигнуто {} нейронов (сигнал: {label})",
                cli.k_hop,
                r.visited
            );
        }
        return 0;
    }

    // ---------- Режим: impact — «кто управляет нейроном» (CSC) ----------
    if let Some(spec) = &cli.connectome_impact {
        let idx = match resolve(spec) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let t1 = std::time::Instant::now();
        let csc = con.build_in_edges();
        let csc_ms = t1.elapsed().as_millis();
        let outs: Vec<ct::Edge> = con.out_edges(idx).unwrap().collect();
        let ins: Vec<ct::Edge> = csc.in_edges(&con, idx).unwrap().collect();
        if ins.is_empty() && outs.is_empty() {
            if json {
                let j = serde_json::json!({
                    "mode": "impact", "artifact": csr_path.display().to_string(),
                    "neuron": idx, "in_degree": 0, "out_degree": 0, "found": false,
                });
                println!("{}", serde_json::to_string_pretty(&j).unwrap_or_default());
            } else {
                eprintln!("poler-connectome: нейрон {idx} изолирован (ни входящих, ни исходящих)");
            }
            return 1;
        }
        let in_mass: u64 = ins.iter().map(|e| e.weight as u64).sum();
        let inh_mass: u64 = ins
            .iter()
            .filter(|e| e.sign() == -1)
            .map(|e| e.weight as u64)
            .sum();
        let exc_mass: u64 = ins
            .iter()
            .filter(|e| e.sign() == 1)
            .map(|e| e.weight as u64)
            .sum();
        let mut top_in = ins.clone();
        top_in.sort_by_key(|e| std::cmp::Reverse(e.weight));
        top_in.truncate(10);
        if json {
            let j = serde_json::json!({
                "mode": "impact",
                "artifact": csr_path.display().to_string(),
                "core": con.is_core(),
                "neuron": idx,
                "root_id": nodes.as_ref().and_then(|n| n.root_id(idx)),
                "in_degree": ins.len(),
                "in_mass": in_mass,
                "in_exc_mass": exc_mass,
                "in_inh_mass": inh_mass,
                "out_degree": outs.len(),
                "csc_build_ms": csc_ms,
                "top_sources": top_in.iter().map(|e| con_edge_json(e, &nodes)).collect::<Vec<_>>(),
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!("Impact нейрона {idx} (кто управляет):");
            println!(
                "  входящих: {} (масса {}) · возбуждающая масса {} / тормозная {} · CSC за {} мс",
                ins.len(),
                thou(in_mass),
                thou(exc_mass),
                thou(inh_mass),
                csc_ms
            );
            println!("  исходящих: {} — куда управляет он сам", outs.len());
            for e in &top_in {
                println!("    {}", fmt_con_edge(e, &nodes));
            }
        }
        return 0;
    }

    // ---------- Режим: соседи (полный список по весу) ----------
    if let Some(spec) = &cli.connectome_neighbors {
        let idx = match resolve(spec) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let dir = match ct_dir(&cli.connectome_dir) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let filter = match ct::SignFilter::parse(&cli.connectome_sign) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let csc = con.build_in_edges();
        let list = match con.neighbors(idx, dir, filter, cli.connectome_limit, &csc) {
            Some(l) => l,
            None => {
                eprintln!("poler-connectome: нейрон {idx} вне диапазона");
                return 1;
            }
        };
        let dir_name = match dir {
            poler_engine::graph::flyops::Direction::Out => "исходящие (мишени)",
            poler_engine::graph::flyops::Direction::In => "входящие (источники)",
        };
        let mass: u64 = list.iter().map(|e| e.weight as u64).sum();
        if json {
            let j = serde_json::json!({
                "mode": "neighbors",
                "artifact": csr_path.display().to_string(),
                "neuron": idx,
                "root_id": nodes.as_ref().and_then(|n| n.root_id(idx)),
                "direction": cli.connectome_dir,
                "sign_filter": cli.connectome_sign,
                "limit": cli.connectome_limit,
                "count": list.len(),
                "mass": mass,
                "edges": list.iter().map(|e| con_edge_json(e, &nodes)).collect::<Vec<_>>(),
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!("Соседи нейрона {idx} — {dir_name} (фильтр {}):", cli.connectome_sign);
            println!("  партнёров: {} (масса {})", list.len(), thou(mass));
            for e in &list {
                println!("  {}", fmt_con_edge(e, &nodes));
            }
        }
        return 0;
    }

    // ---------- Режим: кратчайший путь FROM:TO ----------
    if let Some(spec) = &cli.connectome_path {
        let (fs, ts) = match spec.split_once(':') {
            Some(pair) => pair,
            None => {
                eprintln!(
                    "poler-connectome: --connectome-path ждёт формат FROM:TO, получено «{spec}»"
                );
                return 2;
            }
        };
        let from = match resolve(fs) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let to = match resolve(ts) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let filter = match ct::SignFilter::parse(&cli.connectome_sign) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let r = con.shortest_path(from, to, filter);
        if json {
            let j = serde_json::json!({
                "mode": "path",
                "artifact": csr_path.display().to_string(),
                "from": from,
                "from_root_id": nodes.as_ref().and_then(|n| n.root_id(from)),
                "to": to,
                "to_root_id": nodes.as_ref().and_then(|n| n.root_id(to)),
                "sign_filter": cli.connectome_sign,
                "found": r.found,
                "length": r.length(),
                "total_weight": r.total_weight(),
                "hops": r.hops.iter().map(|h| serde_json::json!({
                    "from": h.from,
                    "to": h.to,
                    "weight": h.edge.weight,
                    "nt": h.edge.nt_name(),
                    "sign": h.edge.sign(),
                    "signed_weight": h.edge.signed_weight(),
                })).collect::<Vec<_>>(),
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else if !r.found {
            eprintln!(
                "poler-connectome: путь {from} -> {to} не найден (фильтр {})",
                cli.connectome_sign
            );
            return 1;
        } else {
            println!(
                "Кратчайший путь {from} -> {to}: {} прыжков, масса {}",
                r.length(),
                thou(r.total_weight())
            );
            for (i, h) in r.hops.iter().enumerate() {
                println!(
                    "  {:>2}. {} -> {}: w={} {} ({:+})",
                    i + 1,
                    h.from,
                    h.to,
                    h.edge.weight,
                    h.edge.nt_name(),
                    h.edge.signed_weight()
                );
            }
        }
        return 0;
    }

    // ---------- Режим: общие партнёры набора ----------
    if let Some(spec) = &cli.connectome_common {
        let idxs: Vec<usize> = match spec
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| resolve(s))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => {
                eprintln!("poler-connectome: --connectome-common: пустой набор нейронов");
                return 2;
            }
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let dir = match ct_dir(&cli.connectome_dir) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let filter = match ct::SignFilter::parse(&cli.connectome_sign) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let csc = con.build_in_edges();
        let mut partners = match con.common_partners(&idxs, dir, filter, &csc) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let count = partners.len();
        partners.truncate(cli.connectome_limit);
        let dir_name = match dir {
            poler_engine::graph::flyops::Direction::Out => "общие мишени (вниз по потоку)",
            poler_engine::graph::flyops::Direction::In => "общие источники (вверх по потоку)",
        };
        if json {
            let j = serde_json::json!({
                "mode": "common",
                "artifact": csr_path.display().to_string(),
                "neurons": idxs,
                "direction": cli.connectome_dir,
                "sign_filter": cli.connectome_sign,
                "count": count,
                "partners": partners.iter().map(|p| serde_json::json!({
                    "node": p.node,
                    "root_id": nodes.as_ref().and_then(|n| n.root_id(p.node)),
                    "total_weight": p.total_weight,
                    "links": p.members.iter().map(|(q, e)| serde_json::json!({
                        "query": q,
                        "edge": con_edge_json(e, &nodes),
                    })).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!("Набор {:?} — {}:", idxs, dir_name);
            println!("  общих партнёров: {}", count);
            for p in partners.iter().take(cli.connectome_limit) {
                println!(
                    "  {} (масса {})",
                    match nodes.as_ref().and_then(|n| n.root_id(p.node)) {
                        Some(r) => format!("{} (root {})", p.node, r),
                        None => p.node.to_string(),
                    },
                    thou(p.total_weight)
                );
            }
        }
        return 0;
    }

    // ---------- Режим: центральность (хабы + PageRank) ----------
    if cli.connectome_centrality {
        let filter = match ct::SignFilter::parse(&cli.connectome_sign) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let csc = con.build_in_edges();
        let top_out = con.degree_ranking(
            poler_engine::graph::flyops::Direction::Out,
            cli.connectome_top,
            &csc,
        );
        let top_in = con.degree_ranking(
            poler_engine::graph::flyops::Direction::In,
            cli.connectome_top,
            &csc,
        );
        let t1 = std::time::Instant::now();
        let pr = con.pagerank(filter, 0.85, 30, 1e-9, cli.connectome_top);
        let pr_ms = t1.elapsed().as_millis();
        if json {
            let j = serde_json::json!({
                "mode": "centrality",
                "artifact": csr_path.display().to_string(),
                "sign_filter": cli.connectome_sign,
                "top": cli.connectome_top,
                "top_out_degree": top_out.iter().map(|&(u, d)| serde_json::json!({
                    "neuron": u,
                    "root_id": nodes.as_ref().and_then(|n| n.root_id(u)),
                    "out_degree": d,
                })).collect::<Vec<_>>(),
                "top_in_degree": top_in.iter().map(|&(v, d)| serde_json::json!({
                    "neuron": v,
                    "root_id": nodes.as_ref().and_then(|n| n.root_id(v)),
                    "in_degree": d,
                })).collect::<Vec<_>>(),
                "pagerank": {
                    "iterations": pr.iterations,
                    "converged": pr.converged,
                    "took_ms": pr_ms,
                    "top": pr.top.iter().map(|&(u, r)| serde_json::json!({
                        "neuron": u,
                        "root_id": nodes.as_ref().and_then(|n| n.root_id(u)),
                        "rank": r,
                    })).collect::<Vec<_>>(),
                },
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!("Центральность (топ {}):", cli.connectome_top);
            println!("  исходящие хабы (куда раздают):");
            for &(u, d) in &top_out {
                println!("    нейрон {u}: {d} рёбер");
            }
            println!("  входящие хабы (кого бомбардируют):");
            for &(v, d) in &top_in {
                println!("    нейрон {v}: {d} рёбер");
            }
            println!(
                "  PageRank ({} итераций, {}):",
                pr.iterations,
                if pr.converged { "сошёлся" } else { "потолок итераций" }
            );
            for &(u, r) in &pr.top {
                println!("    нейрон {u}: ранг {r:.9}");
            }
            println!("  PageRank занял {} мс", pr_ms);
        }
        return 0;
    }

    // ---------- Режим: глобальный топ циркуляции J ----------
    if let Some(k) = cli.connectome_rotor_top {
        let t1 = std::time::Instant::now();
        let pairs = con.rotor_top(k, cli.connectome_min_abs);
        let took_ms = t1.elapsed().as_millis();
        let count = pairs.len();
        if json {
            let j = serde_json::json!({
                "mode": "rotor_top",
                "artifact": csr_path.display().to_string(),
                "top": k,
                "min_abs": cli.connectome_min_abs,
                "count": count,
                "took_ms": took_ms,
                "pairs": pairs.iter().map(|p| serde_json::json!({
                    "u": p.u,
                    "u_root_id": nodes.as_ref().and_then(|n| n.root_id(p.u)),
                    "v": p.v,
                    "v_root_id": nodes.as_ref().and_then(|n| n.root_id(p.v)),
                    "J_uv": p.j,
                    "J_vu": -p.j,
                    "forward_signed": p.forward,
                    "backward_signed": p.backward,
                })).collect::<Vec<_>>(),
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!(
                "Топ-{} циркуляции J = A − Aᵀ (порог |J| ≥ {}, найдено {}, {} мс):",
                k, cli.connectome_min_abs, count, took_ms
            );
            for p in &pairs {
                println!(
                    "  J[{}][{}] = {:+5} — u доминирует (fwd {:?}, bwd {:?})",
                    p.u, p.v, p.j, p.forward, p.backward
                );
            }
        }
        return 0;
    }

    // ---------- Режим: мотивы вокруг нейрона ----------
    if let Some(spec) = &cli.connectome_motifs {
        let idx = match resolve(spec) {
            Ok(i) => i,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let csc = con.build_in_edges();
        let m = match con.motif_census(idx, &csc, 5) {
            Some(m) => m,
            None => {
                eprintln!("poler-connectome: нейрон {idx} вне диапазона");
                return 1;
            }
        };
        if json {
            let j = serde_json::json!({
                "mode": "motifs",
                "artifact": csr_path.display().to_string(),
                "neuron": idx,
                "root_id": nodes.as_ref().and_then(|n| n.root_id(idx)),
                "out_degree": m.out_degree,
                "in_degree": m.in_degree,
                "reciprocal_count": m.reciprocal.len(),
                "reciprocal": m.reciprocal.iter().map(|(fwd, bwd)| serde_json::json!({
                    "partner": fwd.target,
                    "partner_root_id": nodes.as_ref().and_then(|n| n.root_id(fwd.target as usize)),
                    "u_to_partner": {"weight": fwd.weight, "nt": fwd.nt_name(), "sign": fwd.sign()},
                    "partner_to_u": {"weight": bwd.weight, "nt": bwd.nt_name(), "sign": bwd.sign()},
                })).collect::<Vec<_>>(),
                "feedforward": m.feedforward,
                "feedback3": m.feedback3,
                "ff_examples": m.ff_examples,
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!("Мотивы вокруг нейрона {idx}:",);
            println!(
                "  степень: {} исходящих / {} входящих",
                m.out_degree, m.in_degree
            );
            println!("  реципрокных пар (u⇄v): {}", m.reciprocal.len());
            for (fwd, bwd) in m.reciprocal.iter().take(5) {
                println!(
                    "    ⇄ {}: туда w={} {} / обратно w={} {}",
                    fwd.target,
                    fwd.weight,
                    fwd.nt_name(),
                    bwd.weight,
                    bwd.nt_name()
                );
            }
            println!("  feedforward-треугольников (u→v→w + u→w): {}", thou(m.feedforward as u64));
            println!("  feedback-циклов (u→v→w→u): {}", thou(m.feedback3 as u64));
            if !m.ff_examples.is_empty() {
                let ex = m
                    .ff_examples
                    .iter()
                    .map(|(v, w)| format!("{v}→{w}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("  примеры: {ex}");
            }
        }
        return 0;
    }

    // ---------- Режим: симуляция распространения сигнала ----------
    if let Some(spec) = &cli.connectome_propagate {
        let seeds: Vec<usize> = match spec
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| resolve(s))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(v) if !v.is_empty() => v,
            Ok(_) => {
                eprintln!("poler-connectome: --connectome-propagate: пустой набор семян");
                return 2;
            }
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 1;
            }
        };
        let filter = match ct::SignFilter::parse(&cli.connectome_sign) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        let r = match con.propagate(
            &seeds,
            cli.connectome_steps,
            cli.connectome_gamma,
            cli.connectome_leak,
            filter,
            cli.connectome_top,
            0.01,
        ) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("poler-connectome: {e}");
                return 2;
            }
        };
        if json {
            let round6 = |x: f64| (x * 1e6).round() / 1e6;
            let j = serde_json::json!({
                "mode": "propagate",
                "artifact": csr_path.display().to_string(),
                "seeds": r.seeds,
                "params": {
                    "steps": cli.connectome_steps,
                    "gamma": cli.connectome_gamma,
                    "leak": cli.connectome_leak,
                    "sign_filter": cli.connectome_sign,
                    "theta": 0.01,
                },
                "timeline": r.steps.iter().map(|s| serde_json::json!({
                    "step": s.step,
                    "active": s.active,
                    "positive_mass": round6(s.positive_mass),
                    "negative_mass": round6(s.negative_mass),
                })).collect::<Vec<_>>(),
                "top": r.top.iter().map(|&(v, x)| serde_json::json!({
                    "neuron": v,
                    "root_id": nodes.as_ref().and_then(|n| n.root_id(v)),
                    "potential": round6(x),
                })).collect::<Vec<_>>(),
            });
            match serde_json::to_string_pretty(&j) {
                Ok(s) => println!("{s}"),
                Err(e) => {
                    eprintln!("poler-connectome: сериализация JSON: {e}");
                    return 2;
                }
            }
        } else {
            println!(
                "Симуляция от {:?}: {} шагов, γ = {}, leak = {} (фильтр {}):",
                r.seeds, cli.connectome_steps, cli.connectome_gamma, cli.connectome_leak,
                cli.connectome_sign
            );
            for s in &r.steps {
                println!(
                    "  шаг {}: активных {}, масса +{:.3} / {:.3}",
                    s.step, s.active, s.positive_mass, s.negative_mass
                );
            }
            println!("  топ возбуждённых:");
            for &(v, x) in r.top.iter().take(cli.connectome_top) {
                println!("    нейрон {v}: потенциал {x:+.6}");
            }
        }
        return 0;
    }

    // ---------- Сводка (режим по умолчанию) ----------
    let m = con.mass_by_nt();
    let exc = m[1].0 + m[2].0;
    let inh = m[0].0;
    let modm = m[3].0 + m[4].0 + m[5].0;
    let tot = con.total_mass();
    let pct = |x: u64| format!("{:.1}%", x as f64 / tot as f64 * 100.0);
    // топ нейронов по исходящей степени
    let mut top_deg: Vec<(u32, usize)> = (0..con.n_nodes())
        .map(|u| (con.out_degree(u).unwrap_or(0), u))
        .collect();
    top_deg.sort_by(|a, b| b.cmp(a));
    top_deg.truncate(5);
    if json {
        let j = serde_json::json!({
            "mode": "summary",
            "artifact": csr_path.display().to_string(),
            "format": "FLYCSR1",
            "core": con.is_core(),
            "core_min_synapses": if con.is_core() { serde_json::json!(ct::CORE_MIN_SYNAPSES) } else { serde_json::json!(null) },
            "n_nodes": con.n_nodes(),
            "n_edges": con.n_edges(),
            "total_mass": tot,
            "load_ms": load_ms,
            "nt": (0..6).map(|c| serde_json::json!({
                "name": ct::NT_NAMES[c],
                "edges": m[c].1,
                "mass": m[c].0,
                "sign": ct::nt_sign(c as u8),
            })).collect::<Vec<_>>(),
            "mass_balance": {
                "excitatory": exc, "inhibitory": inh, "modulatory": modm,
                "excitatory_pct": pct(exc), "inhibitory_pct": pct(inh), "modulatory_pct": pct(modm),
            },
            "top_out_degree": top_deg.iter().map(|&(d, u)| serde_json::json!({
                "neuron": u,
                "root_id": nodes.as_ref().and_then(|n| n.root_id(u)),
                "out_degree": d,
            })).collect::<Vec<_>>(),
        });
        match serde_json::to_string_pretty(&j) {
            Ok(s) => println!("{s}"),
            Err(e) => {
                eprintln!("poler-connectome: сериализация JSON: {e}");
                return 2;
            }
        }
    } else {
        println!("Коннектом: {}", csr_path.display());
        println!(
            "Формат: FLYCSR1 · {} · загрузка {} мс (zstd -> CSR в RAM)",
            if con.is_core() {
                format!("ядро (рёбра >= {} синапсов)", ct::CORE_MIN_SYNAPSES)
            } else {
                "полный граф".to_string()
            },
            load_ms
        );
        println!(
            "Нейронов: {} · рёбер: {} · синаптическая масса: {}",
            thou(con.n_nodes() as u64),
            thou(con.n_edges() as u64),
            thou(tot)
        );
        println!("{:<8} {:>12} {:>12}   знак", "медиатор", "рёбра", "масса");
        for c in 0..6 {
            let s = ct::nt_sign(c as u8);
            println!(
                "{:<8} {:>12} {:>12}   {}",
                ct::NT_NAMES[c],
                thou(m[c].1),
                thou(m[c].0),
                match s {
                    1 => "+1".to_string(),
                    -1 => "-1".to_string(),
                    _ => "0".to_string(),
                }
            );
        }
        println!(
            "Баланс массы: {} возб / {} торм / {} мод",
            pct(exc),
            pct(inh),
            pct(modm)
        );
        let tops = top_deg
            .iter()
            .map(|&(d, u)| match &nodes {
                Some(ns) => match ns.root_id(u) {
                    Some(r) => format!("{u} (root {r}) — {d}"),
                    None => format!("{u} — {d}"),
                },
                None => format!("{u} — {d}"),
            })
            .collect::<Vec<_>>()
            .join(" · ");
        println!("Топ по исходящей степени: {tops}");
        println!(
            "Запросы: --connectome-node | --connectome-edge U:V | --connectome-khop | \
             --connectome-impact (аргумент: индекс или root_id с --connectome-nodes)"
        );
    }
    0
}

/// Резолв пароля архивов ДО параллельного прогона: флаг
/// `--archive-password` (человек/агент вводит прямо в CLI) → env
/// `POLER_ARCHIVE_KEY` → TTY-промпт (только если среди найденных архивов
/// есть зашифрованные; промпт нельзя звать из rayon-воркеров).
fn resolve_archive_password(
    cli: &Cli,
    roots: &[std::path::PathBuf],
    include_hidden: bool,
) -> Result<Option<String>, String> {
    // 1. Явный флаг — высший приоритет.
    if let Some(p) = &cli.archive_password {
        return Ok(Some(p.clone()));
    }
    // 2. Окружение (не попадает в историю shell).
    if let Ok(p) = std::env::var("POLER_ARCHIVE_KEY") {
        if !p.trim().is_empty() {
            return Ok(Some(p));
        }
    }
    // 3. Пароль нужен только если хоть один архив зашифрован.
    let archives = poler_engine::retrieval::collect_archives(roots, include_hidden, true);
    let mut encrypted: Vec<std::path::PathBuf> = Vec::new();
    for a in &archives {
        match poler_engine::archive::open_info(a) {
            Ok(info) if info.encrypted() => encrypted.push(a.clone()),
            Ok(_) => {}
            // Листинг не удался — не блокируем поиск: сама запись
            // выдам ошибку в stats прогона.
            Err(_) => {}
        }
    }
    if encrypted.is_empty() {
        return Ok(None);
    }
    // 4. Интерактивный ввод (как у Vault: stdin, не история shell).
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        eprint!(
            "poler-archive: пароль для {} (stdin, не попадёт в историю shell): ",
            encrypted[0].display()
        );
        use std::io::Write as _;
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        std::io::stdin()
            .read_line(&mut line)
            .map_err(|e| format!("stdin: {e}"))?;
        let pw = line.trim_end_matches(['\r', '\n']).to_string();
        if pw.is_empty() {
            return Err(format!(
                "пароль пуст; архивы зашифрованы: {} (передайте --archive-password \
                 или POLER_ARCHIVE_KEY)",
                encrypted
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        return Ok(Some(pw));
    }
    Err(format!(
        "архив(ы) зашифрованы: {} — передайте --archive-password <PASS> \
         или env POLER_ARCHIVE_KEY (в интерактивном терминале движок \
         спросит пароль сам)",
        encrypted
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Парольная фраза: env (по умолчанию POLER_VAULT_KEY) → stdin.
/// Ключ никогда не пишется в историю shell: env или пайп.
#[cfg(feature = "pnd-ffi")]
fn vault_passphrase(key_env: &str) -> Result<String, String> {
    if let Ok(k) = std::env::var(key_env) {
        if !k.is_empty() {
            return Ok(k);
        }
    }
    eprint!("poler-vault: парольная фраза (stdin, не попадёт в историю shell): ");
    use std::io::BufRead;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("stdin: {e}"))?;
    let phrase = line.trim_end_matches(['\n', '\r']).to_string();
    if phrase.is_empty() {
        return Err("пустая парольная фраза — отказ (защита от случайной печати без ключа)".into());
    }
    Ok(phrase)
}

/// Выходной путь по умолчанию для печати: PATH + ".pvt".
#[cfg(feature = "pnd-ffi")]
fn vault_default_seal_output(input: &std::path::Path) -> std::path::PathBuf {
    let mut s = input.as_os_str().to_os_string();
    s.push(".pvt");
    std::path::PathBuf::from(s)
}

// ── M6: опции резидентного MCP + стриминг логов ────────────────────────────

/// Собрать опции резидентного сервера из CLI (--mcp-ram-budget, --vault-log).
/// Ошибка создания шифропотока — фатальна ДО старта сервера (не молчим).
fn mcp_server_options(cli: &Cli) -> poler_engine::mcp::McpServerOptions {
    // mut нужен только при pnd-ffi (стриминг --vault-log ниже);
    // без фичи конфигурация неизменяема после конструирования.
    #[cfg_attr(not(feature = "pnd-ffi"), allow(unused_mut))]
    let mut opts = poler_engine::mcp::McpServerOptions {
        ram_budget: cli.mcp_ram_budget.saturating_mul(1024 * 1024),
        ..Default::default()
    };
    #[cfg(feature = "pnd-ffi")]
    if let Some(vl) = &cli.vault_log {
        let phrase = match vault_log_passphrase(cli) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("poler-vault-log: {e}");
                std::process::exit(2);
            }
        };
        let appender = match poler_engine::crypto::vault::VaultAppender::create(
            vl,
            &phrase,
            &poler_engine::crypto::vault::SealOptions::default(),
        ) {
            Ok(a) => a,
            Err(e) => {
                eprintln!("poler-vault-log: {}: {e}", vl.display());
                std::process::exit(2);
            }
        };
        eprintln!(
            "poler-vault-log: журнал операций шифропотоком → {} (стрим, коммит на каждое событие)",
            vl.display()
        );
        opts.vault_log = Some(appender);
    }
    opts
}

/// Фраза шифропотока журнала: --vault-log-pass | env POLER_VAULT_PASS | stdin.
#[cfg(feature = "pnd-ffi")]
fn vault_log_passphrase(cli: &Cli) -> Result<String, String> {
    if let Some(p) = &cli.vault_log_pass {
        if !p.is_empty() {
            return Ok(p.clone());
        }
    }
    if let Ok(k) = std::env::var("POLER_VAULT_PASS") {
        if !k.is_empty() {
            return Ok(k);
        }
    }
    eprint!("poler-vault-log: парольная фраза журнала (stdin): ");
    use std::io::BufRead;
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("stdin: {e}"))?;
    let phrase = line.trim_end_matches(['\n', '\r']).to_string();
    if phrase.is_empty() {
        return Err("пустая парольная фраза — отказ".into());
    }
    Ok(phrase)
}

/// M6: бенчмарк резидентности — холодный против тёплого.
///
/// Строит (или берёт по --path/--knowledge-db) корпус, поднимает
/// in-process McpServer и гонит N итераций poler_grep +
/// query_poler_knowledge, замеряя латентность каждой dispatch.
/// Отчёт: cold (итерация 1) и warm p50/p95/p99; бюджет <5 мс —
/// критерий приёмки M6 (exit 0/1).
fn run_mcp_bench(cli: &Cli, iterations: usize) -> i32 {
    use poler_engine::mcp::McpServer;
    use serde_json::json;

    let tmp = std::env::temp_dir().join(format!("poler-mcp-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();

    // Корпус: --path пользователя или временный (60 файлов × ~8 КиБ).
    let corpus: std::path::PathBuf = cli
        .path
        .clone()
        .unwrap_or_else(|| {
            eprintln!("poler-bench: --path не задан — строю временный корпус 60×8 КиБ");
            let dir = tmp.join("corpus");
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..60 {
                let body = format!(
                    "раздел {i}: каноническое уравнение dp/dt и резонансный аттрактор H Psi\n\
                     строка шума {i} для объёма и правдоподобия корпуса\n"
                );
                std::fs::write(dir.join(format!("doc{i:03}.md")), body.repeat(64)).unwrap();
            }
            dir
        })
        .into();

    // Гиппокамп: --knowledge-db пользователя или временный инжест.
    let kdb: std::path::PathBuf = cli
        .knowledge_db
        .clone()
        .unwrap_or_else(|| {
            eprintln!("poler-bench: --knowledge-db не задан — строю временную библиотеку");
            let dir = tmp.join("lib");
            let spec = dir.join("01_SPECS");
            std::fs::create_dir_all(&spec).unwrap();
            std::fs::write(
                spec.join("PTS-BENCH.md"),
                "# PTS-BENCH\n\n## 4. ПОЛНЫЙ ТЕХНИЧЕСКИЙ ТЕКСТ\nрезонансный аттрактор канона: dp/dt = -eta Pi Lambda D p.\n",
            )
            .unwrap();
            let db = tmp.join("bench-knowledge.db");
            let mut emb = poler_engine::sources::knowledge::KnowledgeEmbedder::None;
            poler_engine::sources::knowledge::ingest(
                &dir,
                &db,
                &mut emb,
                &poler_engine::sources::knowledge::IngestOptions::default(),
            )
            .unwrap();
            db
        })
        .into();

    // path-guard: разрешаем корпус (временный или пользовательский).
    std::env::set_var("POLER_MCP_EXTRA_ROOTS", &corpus);

    let server = McpServer::new(9222, 10, tmp.join("no-web.db"))
        .with_knowledge_db(kdb)
        .with_ram_budget(cli.mcp_ram_budget.saturating_mul(1024 * 1024));

    let dispatch_timed = |msg: serde_json::Value| -> (u128, serde_json::Value) {
        let t0 = std::time::Instant::now();
        let r = server.dispatch(&msg).unwrap_or(serde_json::Value::Null);
        (t0.elapsed().as_micros(), r)
    };

    let grep_msg = |q: &str| {
        json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "poler_grep",
                       "arguments": {"pattern": q, "path": corpus.to_str().unwrap(), "output": "count"}}
        })
    };
    let know_msg = |q: &str| {
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "query_poler_knowledge", "arguments": {"query": q}}
        })
    };

    let print_report = |title: &str, samples: &[u128]| -> bool {
        let mut s = samples.to_vec();
        s.sort_unstable();
        let pick = |p: f64| -> u128 {
            if s.is_empty() { 0 } else { s[((s.len() as f64 - 1.0) * p).round() as usize] }
        };
        let p50 = pick(0.50);
        let p95 = pick(0.95);
        let p99 = pick(0.99);
        let ok = p99 < 5_000;
        println!(
            "{title}: n={} · cold={} мкс · warm p50={} p95={} p99={} мкс · бюджет <5000 мкс: {}",
            samples.len(),
            samples.first().copied().unwrap_or(0),
            p50, p95, p99,
            if ok { "ДА" } else { "НЕТ" }
        );
        ok
    };

    println!("poler-mcp-bench (M6 резидентность): итераций = {iterations}, бюджет RAM = {} МиБ", cli.mcp_ram_budget);

    // Серия grep: итерация 0 = холодная (чтение диска), остальные тёплые.
    let mut grep_samples = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let (us, r) = dispatch_timed(grep_msg(if i % 2 == 0 { "резонансный" } else { "dp/dt" }));
        let ok = serde_json::to_string(&r).unwrap().contains("poler-grep");
        if !ok {
            eprintln!("poler-bench: grep-вызов не прошёл: {r}");
            let _ = std::fs::remove_dir_all(&tmp);
            return 2;
        }
        grep_samples.push(us);
    }
    let grep_ok = print_report("poler_grep        ", &grep_samples);

    // Серия знаний: итерация 0 = холодная (открытие SQLite/векторов).
    let mut know_samples = Vec::with_capacity(iterations);
    for i in 0..iterations {
        let (us, r) = dispatch_timed(know_msg(if i % 2 == 0 { "резонансный аттрактор" } else { "dp/dt канон" }));
        let txt = serde_json::to_string(&r).unwrap();
        if !txt.contains("poler knowledge") && !txt.contains("не проиндексирована") {
            eprintln!("poler-bench: knowledge-вызов не прошёл: {r}");
            let _ = std::fs::remove_dir_all(&tmp);
            return 2;
        }
        know_samples.push(us);
    }
    let know_ok = print_report("knowledge (warm)  ", &know_samples);

    // RAM-кэш и тёплые хэндлы.
    let stats = serde_json::to_string_pretty(&server.resident_stats()).unwrap();
    println!("резидентное состояние: {stats}");

    let _ = std::fs::remove_dir_all(&tmp);
    if grep_ok && know_ok {
        println!("M6: ПРИЁМКА ПРОЙДЕНА — тёплые p99 в бюджете <5 мс");
        0
    } else {
        eprintln!("M6: ПРИЁМКА ПРОВАЛЕНА — тёплые p99 выше 5 мс");
        1
    }
}

/// Выходной путь по умолчанию для вскрытия: снять .pvt, иначе суффикс .out.
#[cfg(feature = "pnd-ffi")]
fn vault_default_open_output(vault: &std::path::Path) -> std::path::PathBuf {
    match vault.extension().and_then(|e| e.to_str()) {
        Some("pvt") => vault.with_extension(""),
        _ => {
            let mut s = vault.as_os_str().to_os_string();
            s.push(".out");
            std::path::PathBuf::from(s)
        }
    }
}

/// E1/v0.31.0 + E2/v0.32.0: диспетчер --exec. Вывод ребёнка — в наши потоки
/// как есть (байты, без перекодировки), код выхода — ребёнка; таймаут — 124
/// (конвенция GNU timeout), не найдено — 127, отказ права — 126, плохой
/// cwd — 125. E2: --exec-cwd/--exec-env/--exec-pty/--exec-capture.
#[cfg(feature = "pnd-ffi")]
fn run_exec(cli: &Cli) -> i32 {
    use poler_engine::exec::{self, CaptureMode, ExecSpec};
    use std::io::Write;

    let mut parts = cli.exec.iter();
    let program = parts.next().unwrap().clone();
    let args: Vec<String> = parts.cloned().collect();

    // --exec-env KEY=VALUE (повторяемый): явное окружение вместо наследования.
    let env = if cli.exec_env.is_empty() {
        None
    } else {
        let mut pairs = Vec::with_capacity(cli.exec_env.len());
        for kv in &cli.exec_env {
            match kv.split_once('=') {
                Some((k, v)) => pairs.push((k.to_string(), v.to_string())),
                None => {
                    eprintln!("poler-exec: --exec-env ожидает KEY=VALUE, получено {kv:?}");
                    return 2;
                }
            }
        }
        Some(pairs)
    };

    let capture = match CaptureMode::parse(&cli.exec_capture) {
        Some(c) => c,
        None => {
            eprintln!(
                "poler-exec: неизвестный --exec-capture {:?} (доступно: tail | head_tail)",
                cli.exec_capture
            );
            return 2;
        }
    };

    let spec = ExecSpec {
        program,
        args,
        env,
        timeout_ms: cli.exec_timeout_ms,
        grace_ms: cli.exec_grace_ms,
        max_out_bytes: cli.exec_max_out,
        stdin_data: cli.exec_stdin.as_ref().map(|s| s.as_bytes().to_vec()),
        cwd: cli.exec_cwd.clone(),
        pty: cli.exec_pty,
        capture,
        cancel: None,
    };

    match exec::run(&spec) {
        Ok(out) => {
            let mut so = std::io::stdout();
            let _ = so.write_all(&out.stdout);
            let _ = so.flush();
            let mut se = std::io::stderr();
            if !out.pty {
                // PTY: stderr уже слит в stdout — не дублируем
                let _ = se.write_all(&out.stderr);
            }
            if out.truncated {
                let _ = writeln!(
                    se,
                    "poler-exec: вывод превышал {} байт — удержан {}",
                    cli.exec_max_out,
                    match capture {
                        CaptureMode::Tail => "хвост".to_string(),
                        CaptureMode::HeadTail => "голова+маркер+хвост".to_string(),
                    }
                );
            }
            let _ = se.flush();
            if out.cancelled {
                eprintln!(
                    "poler-exec: запуск отменён — pid {} убит (TERM→KILL), {} мкс",
                    out.pid, out.duration_us
                );
                return 130; // 128+SIGINT-конвенция отмены
            }
            if out.timed_out {
                eprintln!(
                    "poler-exec: таймаут {} мс — pid {} убит (TERM→KILL), {} мкс",
                    cli.exec_timeout_ms, out.pid, out.duration_us
                );
                return 124;
            }
            if let Some(sig) = out.signal {
                eprintln!("poler-exec: процесс убит сигналом {sig}");
                return 128 + sig;
            }
            out.exit_code.unwrap_or(1)
        }
        Err(e) => {
            eprintln!("poler-exec: {e}");
            e.exit_code()
        }
    }
}

#[cfg(feature = "pnd-ffi")]
fn run_memory_seal(cli: &Cli, input: &std::path::Path) -> i32 {
    use poler_engine::crypto::vault::{self, SealOptions};

    let output = cli.memory_out.clone().unwrap_or_else(|| vault_default_seal_output(input));
    if output.exists() {
        eprintln!(
            "poler-vault: выход уже существует: {} (передайте --memory-out или удалите)",
            output.display()
        );
        return 2;
    }
    let passphrase = match vault_passphrase(&cli.memory_key_env) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("poler-vault: {e}");
            return 2;
        }
    };
    let opts = SealOptions {
        iterations: cli.memory_kdf_iters,
        content_id: cli.memory_content_id,
        salt: None,
    };
    match vault::seal(input, &output, &passphrase, &opts) {
        Ok(rep) => {
            println!("запечатано: {}", rep.output.display());
            println!(
                "  источник: {} ({} байт, {} стр. по 4096)",
                rep.input.display(),
                rep.original_len,
                rep.pages
            );
            println!("  контейнер: {} байт", rep.vault_len);
            println!("  время: {} мс ({:.1} МБ/с), KDF {} итераций", rep.duration_ms, rep.mb_per_s, opts.iterations);
            if let Some(cid) = rep.content_id {
                println!("  content-id: {}", poler_engine::crypto::hasher::PndHasher::hex(&cid));
            }
            println!("  проверка без ключа: poler-engine --memory-verify {}", rep.output.display());
            0
        }
        Err(e) => {
            eprintln!("poler-vault: {e}");
            2
        }
    }
}

#[cfg(feature = "pnd-ffi")]
fn run_memory_open(cli: &Cli, vault_path: &std::path::Path) -> i32 {
    use poler_engine::crypto::vault;

    let output = cli.memory_out.clone().unwrap_or_else(|| vault_default_open_output(vault_path));
    if output.exists() {
        eprintln!(
            "poler-vault: выход уже существует: {} (передайте --memory-out или удалите)",
            output.display()
        );
        return 2;
    }
    let passphrase = match vault_passphrase(&cli.memory_key_env) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("poler-vault: {e}");
            return 2;
        }
    };
    match vault::open(vault_path, &output, &passphrase) {
        Ok(rep) => {
            println!("вскрыто: {}", rep.output.display());
            println!(
                "  контейнер: {} ({} байт, {} стр.)",
                rep.input.display(),
                rep.original_len,
                rep.pages
            );
            println!(
                "  транспорт: OK (SHA-256), аутентичность: OK (MAC), {} мс ({:.1} МБ/с)",
                rep.duration_ms, rep.mb_per_s
            );
            0
        }
        Err(e) => {
            eprintln!("poler-vault: {e}");
            eprintln!("  подсказка: неверная фраза ИЛИ подмена — сравните с --memory-verify (без ключа)");
            2
        }
    }
}

#[cfg(feature = "pnd-ffi")]
fn run_memory_verify(_cli: &Cli, vault_path: &std::path::Path) -> i32 {
    use poler_engine::crypto::vault;

    match vault::verify(vault_path) {
        Ok(rep) => {
            println!("контейнер цел: {}", rep.path.display());
            println!("  заголовок: OK (FNV-1a64), транспорт: OK (SHA-256 шифротекста)");
            println!("  страниц: {}, размер: {} байт", rep.pages, rep.vault_len);
            0
        }
        Err(e) => {
            eprintln!("poler-vault: {e}");
            2
        }
    }
}

#[cfg(feature = "pnd-ffi")]
fn run_memory_info(_cli: &Cli, vault_path: &std::path::Path) -> i32 {
    use poler_engine::crypto::vault;

    match vault::info(vault_path) {
        Ok(h) => {
            println!("контейнер: {}", vault_path.display());
            println!("  формат: v{} (magic POLERVLT), flags: {:#x}", h.format_version, h.flags);
            println!(
                "  данные: {} байт в {} стр. по 4096 (контейнер: {} байт)",
                h.original_len,
                h.page_count,
                4096 + h.page_count * 4096
            );
            println!("  KDF: {} итераций, epsilon: {:#x}", h.kdf_iterations, h.epsilon);
            if h.flags & poler_engine::crypto::vault::FLAG_CONTENT_ID != 0 {
                println!("  content-id: {}", poler_engine::crypto::hasher::PndHasher::hex(&h.content_id));
            } else {
                println!("  content-id: (не сохранён — приватный режим)");
            }
            0
        }
        Err(e) => {
            eprintln!("poler-vault: {e}");
            2
        }
    }
}


/// Рекурсивный сбор текстовых файлов (без бинарных, ≤2 МБ).
fn collect_text_files(root: &std::path::Path, out: &mut Vec<std::path::PathBuf>, max: usize) -> bool {
    let meta = match std::fs::metadata(root) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("poler-engine: корпус недоступен: {e}");
            return false;
        }
    };
    if meta.is_file() {
        out.push(root.to_path_buf());
        return true;
    }
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>, max: usize) {
        if out.len() >= max {
            return;
        }
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            let name = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if name.starts_with('.') || name == "target" || name == "node_modules" {
                continue;
            }
            match std::fs::metadata(&p) {
                Ok(m) if m.is_dir() => walk(&p, out, max),
                Ok(m) if m.len() <= 2_000_000 && m.len() > 0 => {
                    // пропускаем бинарные: NUL-байт в первой тысяче
                    let mut head = [0u8; 1024];
                    if let Ok(mut fh) = std::fs::File::open(&p) {
                        use std::io::Read;
                        if let Ok(n) = fh.read(&mut head) {
                            if !head[..n].contains(&0) {
                                out.push(p);
                            }
                        }
                    }
                }
                _ => {}
            }
            if out.len() >= max {
                return;
            }
        }
    }
    walk(root, out, max);
    true
}

/// `--llm quantum [--model X.poler] -q "…"`: квантово-фазовый L5-разум
/// (русла циркуляции J, сфера Блоха, Born-лотерея по аттракторам).
fn run_quantum_llm(cli: &Cli) -> i32 {
    let Some(prompt) = &cli.query else {
        eprintln!("poler-engine: --llm quantum требует -q <промпт>");
        return 2;
    };
    let mut mind = if let Some(path) = &cli.model {
        match poler_engine::quantum::QuantumMind::open(path) {
            Ok(mut m) => {
                let corpus_opt = cli.corpus.as_ref().or(cli.semantic_corpus.as_ref());
                if let Some(corpus_path) = corpus_opt {
                    let mut files = Vec::new();
                    if collect_text_files(corpus_path, &mut files, 256) {
                        for f in files {
                            if let Ok(text) = std::fs::read_to_string(f) {
                                let _ = m.ingest(&text, 0);
                            }
                        }
                    }
                }
                let _ = m.ingest(prompt, 0);
                m
            }
            Err(e) => {
                eprintln!("poler-engine: {e}");
                return 2;
            }
        }
    } else {
        match poler_engine::quantum::QuantumMind::new(1024, 42) {
            Ok(mut m) => {
                let corpus_opt = cli.corpus.as_ref().or(cli.semantic_corpus.as_ref());
                if let Some(corpus_path) = corpus_opt {
                    let mut files = Vec::new();
                    if collect_text_files(corpus_path, &mut files, 256) {
                        for f in files {
                            if let Ok(text) = std::fs::read_to_string(f) {
                                let _ = m.ingest(&text, 0);
                            }
                        }
                    }
                }
                let _ = m.ingest(prompt, 0);
                m
            }
            Err(e) => {
                eprintln!("poler-engine: {e}");
                return 2;
            }
        }
    };

    use std::io::Write;
    print!("квантовый ответ: ");
    let _ = std::io::stdout().flush();
    let t0 = std::time::Instant::now();
    let res = mind.generate_stream(prompt, 64, |token| {
        print!("{token} ");
        let _ = std::io::stdout().flush();
        true
    });
    println!();
    match res {
        Ok(rep) => {
            let dt = t0.elapsed().as_secs_f64();
            println!(
                "\nquantum-L5 · {} шагов за {dt:.3} с = {:.1} шаг/с · Born-лотерея по руслам J (релевантность: {:.1}%)",
                rep.steps.len(),
                rep.steps.len() as f64 / dt.max(1e-9),
                rep.relevance * 100.0
            );
            0
        }
        Err(e) => {
            eprintln!("poler-engine quantum: {e}");
            2
        }
    }
}

/// `--jit-loop [FILE]`: замкнутий контур «ваги → машинний код →
/// виконання → пластичність → перекомпіляція».
///
/// Кожен вага шару вшивається в машинний код x86_64 як immediate
/// (`mov eax, <біти f32>`), код виконується прямо з R|X-сторінки,
/// після чого Hebb-правило переписує трити в .t5q і шар
/// перекомпілюється — модель навчается, залишаючись стисненою.
fn run_jit_loop(target: &str) -> i32 {
    use poler_engine::triune::compiler::PlasticityConfig;
    use poler_engine::triune::jit_loop::JitLoop;
    use poler_engine::triune::stream_quant::{stream_quantize, StreamQuantConfig};

    let path: std::path::PathBuf = if target.is_empty() {
        // Синтетичний шар: 4096 LCG-значень → .t5q у тимчасовій папці.
        let mut s = 0x91F100Du64;
        let mut data = Vec::with_capacity(4096 * 4);
        for _ in 0..4096 {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = ((s >> 33) as i32 as f64 / i32::MAX as f64) as f32;
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        if let Err(e) = stream_quantize(&data[..], &mut out, &StreamQuantConfig::default()) {
            eprintln!("poler-engine: синтетичний шар: {e}");
            return 2;
        }
        let dir = std::env::temp_dir().join("poler_jit_loop_demo");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("layer.t5q");
        if let Err(e) = std::fs::write(&p, &out) {
            eprintln!("poler-engine: тимчасовий файл: {e}");
            return 2;
        }
        p
    } else {
        target.into()
    };

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║  JIT LOOP · ВЕСЫ = МАШИННЫЙ КОД · ЗАМКНУТЫЙ КОНТУР       ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!("файл: {}", path.display());

    let mut lp = match JitLoop::open(&path, PlasticityConfig::default(), 0, 32, 32) {
        Ok(lp) => lp,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };
    println!(
        "слой: {}×{} · машинный код: {} Б · инструкций: {} · плотность: {:.1} Б/вес",
        lp.rows(),
        lp.cols(),
        lp.code_len(),
        lp.instruction_count(),
        lp.code_len() as f32 / (lp.rows() * lp.cols()) as f32
    );
    println!(
        "блоков в .t5q: {} · значений: {}",
        lp.view().block_count(),
        lp.view().values()
    );

    // Фрагмент листинга — ваги видно прямо в інструкціях.
    println!("\nлистинг (первые строки — ваги як immediate у потоці коду):");
    for line in lp.asm_listing().lines().take(14) {
        println!("  {line}");
    }

    // Три цикли контуру з різними фазами входу.
    for cycle in 1..=3u32 {
        let phase = cycle as f32 * 0.35;
        let x: Vec<f32> = (0..32)
            .map(|j| {
                let v = (j as f32 * 0.21 + phase).sin() * 0.9 + 0.05 * ((j % 3) as f32 - 1.0);
                v.clamp(-1.0, 1.0)
            })
            .collect();
        let rep = match lp.cycle(&x) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("poler-engine: цикл {cycle}: {e}");
                return 2;
            }
        };
        let y_norm = rep.y_before.iter().map(|v| v * v).sum::<f32>().sqrt();
        println!(
            "\n─ цикл {cycle} ─ вход ||x||≈{:.2}",
            x.iter().map(|v| v * v).sum::<f32>().sqrt()
        );
        println!(
            "  forward машинним кодом: ||y|| = {y_norm:.4} ({} выходов)",
            rep.y_before.len()
        );
        println!(
            "  Hebb: {} импульсов · in-place переписано тритов: {}",
            rep.impulses, rep.flips
        );
        println!(
            "  commit: вакуум {} → {} · {} мс · sha256 пересчитан",
            rep.zeros_before, rep.zeros_after, rep.commit_ms
        );
        println!(
            "  перекомпіляція: {} Б нового машинного кода ({} инструкций)",
            rep.code_bytes, lp.instruction_count()
        );
        println!(
            "  следующий forward на новых весах: ||Δy|| = {:.4} — сигнал обучения",
            rep.y_delta_norm()
        );
    }

    // Цілісність файлу після трьох циклів мутацій.
    match lp.view().verify() {
        Ok(()) => println!("\nsha256: OK — файл валиден после 3 циклов самомодификации"),
        Err(e) => {
            eprintln!("poler-engine: verify: {e}");
            return 2;
        }
    }
    if target.is_empty() {
        let _ = std::fs::remove_file(&path);
        println!("(демо-файл удалён; для работы с реальными весами: --jit-loop layer.t5q)");
    }
    0
}

/// `--t5q-compile [FILE]`: обратный In-Place компилятор — модель
/// переписывает собственные триты в .t5q без декомпрессии.
/// Без FILE — синтетическая демонстрация полного цикла.
fn run_t5q_compile(target: &str) -> i32 {
    use poler_engine::triune::compiler::{PlasticityCompiler, PlasticityConfig};
    use poler_engine::triune::stream_quant::{stream_quantize, StreamQuantConfig};

    let mut demo_path: Option<std::path::PathBuf> = None;
    let path: std::path::PathBuf = if target.is_empty() {
        // Синтетический поток: 4096 значений LCG → .t5q во временной папке.
        let mut s = 0xC0FFEEu64;
        let mut data = Vec::with_capacity(4096 * 4);
        for _ in 0..4096 {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let v = ((s >> 33) as i32 as f64 / i32::MAX as f64) as f32;
            data.extend_from_slice(&v.to_le_bytes());
        }
        let mut out = Vec::new();
        if let Err(e) = stream_quantize(&data[..], &mut out, &StreamQuantConfig::default()) {
            eprintln!("poler-engine: синтетический поток: {e}");
            return 2;
        }
        let dir = std::env::temp_dir().join("poler_t5q_compile_demo");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("demo.t5q");
        if let Err(e) = std::fs::write(&p, &out) {
            eprintln!("poler-engine: временный файл: {e}");
            return 2;
        }
        demo_path = Some(p.clone());
        p
    } else {
        target.into()
    };

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║   INVERSE IN-PLACE COMPILER · триты переписывают себя    ║");
    println!("╚══════════════════════════════════════════════════════════╝");
    println!("файл: {}", path.display());

    let mut compiler = match PlasticityCompiler::open(&path, PlasticityConfig::default()) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };
    let blocks = compiler.view().block_count();
    let values = compiler.view().values();
    println!("блоков: {blocks} · значений: {values} · вакуум: пересчитывается при commit");

    // 1. Хеббовские импульсы: пара активаций пре/пост.
    let pre = vec![0.9f32, -0.2, 0.05, 0.0, 0.7, 0.0, -0.6, 0.1];
    let post = vec![0.8f32, 0.0, -0.9, 0.05, 0.0, 0.6, 0.0, -0.05];
    let hebb = match compiler.hebbian_impulses(0, 8, 8, &pre, &post) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("poler-engine: хебб: {e}");
            return 2;
        }
    };
    let mut impulses = hebb;

    // 2. Фазовый ротор J = A − Aᵀ: доминирующий поток ↔ встречный канал.
    let mut a = vec![0.0f32; 64];
    a[1 * 8 + 0] = 0.9;
    a[0 * 8 + 1] = -0.4;
    match compiler.phase_rotor_impulses(0, &a, 8) {
        Ok(mut v) => impulses.append(&mut v),
        Err(e) => {
            eprintln!("poler-engine: фазовый ротор: {e}");
            return 2;
        }
    }
    println!("импульсов мутаций: {} (STDP + фазовый ротор)", impulses.len());

    // 3. In-place применение (без распаковки в FP16).
    match compiler.apply(&impulses) {
        Ok(changed) => println!("тритов переписано: {changed}"),
        Err(e) => {
            eprintln!("poler-engine: apply: {e}");
            return 2;
        }
    }

    // 4. Адаптация масштаба первого блока (STDP-пластичность масштаба).
    let _ = compiler.adapt_scale(0, 1.1);

    // 5. Атомарная фиксация: пересчёт вакуума + sha256 + msync.
    match compiler.commit() {
        Ok(stats) => {
            println!(
                "commit: {} флипов · {} масштабов · вакуум {} → {} · {} мс",
                stats.flips, stats.scale_updates, stats.zeros_before, stats.zeros_after, stats.ms
            );
        }
        Err(e) => {
            eprintln!("poler-engine: commit: {e}");
            return 2;
        }
    }

    // 6. Финальная валидация.
    if let Err(e) = compiler.view().verify() {
        eprintln!("poler-engine: файл невалиден после commit: {e}");
        return 2;
    }
    println!("sha256: OK — файл валиден, мутации зафиксированы на диске");

    if let Some(p) = demo_path {
        let dir = p.parent().unwrap().to_path_buf();
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_dir(&dir);
        println!("(демо-файл удалён; для работы с реальными весами: --t5q-compile model.t5q)");
    }
    0
}

/// `--llm local --model X.pqw -q "…"`: нативный GLM-декодер (RoPE + MQA +
/// SwiGLU + MoE) через pqc. remote/auto — мост Фазы 12.9 (заглушка).
fn run_llm(mode: &str, cli: &Cli) -> i32 {
    match mode {
        "quantum" => return run_quantum_llm(cli),
        "local" => {}
        "remote" | "auto" => {
            eprintln!(
                "poler-engine: --llm {mode} — серверный GLM-мост это Фаза 12.9 (не встроен);\n  \
                 сейчас доступны автономные режимы: --llm quantum, --llm local --model glm.pqw"
            );
            return 2;
        }
        _ => {
            eprintln!(
                "poler-engine: неизвестный --llm режим {mode:?} (доступны: quantum, local, remote, auto)"
            );
            return 2;
        }
    }
    let Some(model_path) = &cli.model else {
        eprintln!(
            "poler-engine: --llm local требует --model <glm.pqw>\n  \
             конвертер PyTorch GLM → .pqw (int4) — Фаза 12.7;\n  \
             проверить декодер сейчас: poler-engine --pqw-selftest"
        );
        return 2;
    };
    let Some(prompt) = &cli.query else {
        eprintln!("poler-engine: --llm local требует -q <промпт>");
        return 2;
    };
    let model = match poler_engine::llm::glm_engine::GlmModel::open(model_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };
    // _has_real_tok: признак «реальный токенизатор vs hash-фолбэк» —
    // диагностика; поведение генерации от него не ветвится.
    let (prompt_ids, _has_real_tok) = if let Some(tok) = model.tokenizer() {
        let formatted = if prompt.contains("[gMASK]") || prompt.contains("<|user|>") {
            prompt.clone()
        } else {
            format!("[gMASK]sop<|user|>\n{prompt}<|assistant|>")
        };
        let mut ids = tok.encode(&formatted);
        // If encode added bos (id=0 or 1) and eos (id=2), remove them if [gMASK]/sop are present
        if ids.first() == Some(&0) || ids.first() == Some(&1) {
            ids.remove(0);
        }
        if ids.last() == Some(&2) {
            ids.pop();
        }
        (ids, true)
    } else {
        (poler_engine::pqc::hash_token_ids(prompt, model.vocab() as u32), false)
    };
    if prompt_ids.is_empty() {
        eprintln!("poler-engine: пустой промпт — нечего генерировать");
        return 2;
    }
    let max_new = 128usize;
    let t0 = std::time::Instant::now();
    use std::io::Write;
    print!("ответ: ");
    let _ = std::io::stdout().flush();
    let out = match model.generate_stream(
        &prompt_ids,
        max_new,
        &poler_engine::llm::glm_engine::Sampling::Greedy,
        0,
        |_tok, piece| {
            print!("{piece}");
            let _ = std::io::stdout().flush();
            true
        },
    ) {
        Ok(o) => o,
        Err(e) => {
            eprintln!("\npoler-engine: {e}");
            return 2;
        }
    };
    println!();
    let dt = t0.elapsed().as_secs_f64();
    println!(
        "\nglm-local · {} токенов за {dt:.3} с = {:.1} ток/с · greedy",
        out.len(),
        out.len() as f64 / dt.max(1e-9)
    );
    0
}

/// `--ner gliner --model X.pqw -q "…"`: нативная span-голова GLiNER через pqc.
///
/// model_type=Gliner (реальные чекпойнты, mdeberta-спина): zero-shot метки
/// через `--ner-labels`, пословленная токенизация из секции __tokenizer__.
/// model_type=SpanNer (синтетика/selftest): метки фиксированы в __labels__.
fn run_ner(what: &str, cli: &Cli) -> i32 {
    if what != "gliner" {
        eprintln!("poler-engine: неизвестный --ner {what:?} (доступен: gliner)");
        return 2;
    }
    let Some(model_path) = &cli.model else {
        eprintln!(
            "poler-engine: --ner gliner требует --model <gliner.pqw>\n  \
             конвертер реальных весов: scripts/convert_gliner_to_pqw.py;\n  \
             проверить голову сейчас: poler-engine --pqw-selftest"
        );
        return 2;
    };
    let Some(text) = &cli.query else {
        eprintln!("poler-engine: --ner gliner требует -q <текст>");
        return 2;
    };
    // Маршрутизация по model_type из заголовка .pqw.
    let real = match poler_engine::pqc::pqw::QuantizedWeightsView::open(model_path) {
        Ok(v) => matches!(v.header().model_type, poler_engine::pqc::pqw::ModelType::Gliner),
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };
    if real {
        let labels: Vec<&str> = match &cli.ner_labels {
            Some(l) => l.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect(),
            None => {
                eprintln!(
                    "poler-engine: --ner gliner (реальный чекпойнт) требует \n  \
                     --ner-labels \"метка1,метка2,…\" (zero-shot список типов сущностей)"
                );
                return 2;
            }
        };
        let model = match poler_engine::ner::RealGlinerModel::open(model_path) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("poler-engine: {e}");
                return 2;
            }
        };
        let t0 = std::time::Instant::now();
        let entities = match model.predict(text, &labels, 0.5) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("poler-engine: {e}");
                return 2;
            }
        };
        println!(
            "gliner-native · {} сущностей · метки [{}] · {:.1} мс",
            entities.len(),
            labels.join(", "),
            t0.elapsed().as_secs_f64() * 1000.0
        );
        for e in entities.iter().take(20) {
            println!("  {}:{} [{:.2}] {}..{}", e.label, e.text, e.score, e.start, e.end);
        }
        if entities.len() > 20 {
            println!("  … и ещё {}", entities.len() - 20);
        }
        return 0;
    }
    let model = match poler_engine::ner::native_gliner::GlinerModel::open(model_path) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };
    use unicode_segmentation::UnicodeSegmentation;
    let words: Vec<String> = text.unicode_words().map(|w| w.to_string()).collect();
    if words.is_empty() {
        eprintln!("poler-engine: текст без UAX#29-слов");
        return 2;
    }
    let vocab = model.vocab() as u32;
    let ids: Vec<u32> = words
        .iter()
        .map(|w| {
            poler_engine::pqc::hash_token_ids(w, vocab)
                .first()
                .copied()
                .unwrap_or(0)
        })
        .collect();
    let word_refs: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
    let t0 = std::time::Instant::now();
    let entities = match model.extract(&word_refs, &ids, 0.5) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("poler-engine: {e}");
            return 2;
        }
    };
    println!(
        "gliner-native · {} сущностей · метки [{}] · {:.1} мс",
        entities.len(),
        model.labels().join(", "),
        t0.elapsed().as_secs_f64() * 1000.0
    );
    for e in entities.iter().take(20) {
        println!("  {}:{} [{:.2}] {}..{}", e.label, e.text, e.score, e.start, e.end);
    }
    if entities.len() > 20 {
        println!("  … и ещё {}", entities.len() - 20);
    }
    0
}

// ══════════════════════════════════════════════════════════════════
// L1/v0.34.0: Литературный Двигатель POLER[Ψ]
// ══════════════════════════════════════════════════════════════════

/// Режим CLI Литературного Двигателя.
// S1/v0.35.0: Синаптический Вихрь SSN — доказанное до реализации ядро
// (65→67 проверок Python-сьюта, 10 seed × 10k шагов Rust full-fidelity).
fn run_ssn(cli: &Cli) -> i32 {
    use poler_engine::ssn;
    use serde_json::json;

    let cfg = ssn::VortexConfig {
        n: cli.ssn_n,
        fields: cli.ssn_fields,
        ..Default::default()
    };

    // ── Режим 1: --ssn-encode [+ --ssn-encode-b] — сенсорная мера CSE ──
    if let Some(text) = cli.ssn_encode.clone() {
        let v = ssn::cse::encode(&text, cli.ssn_dims);
        match cli.ssn_encode_b.clone() {
            Some(other) => {
                let w = ssn::cse::encode(&other, cli.ssn_dims);
                let cos = ssn::cse::cos_sim(&v, &w);
                let sin = ssn::cse::sin_corrected(&v, &w);
                if cli.ssn_json {
                    println!(
                        "{}",
                        json!({
                            "mode": "ssn-encode-pair",
                            "a": text, "b": other, "dims": cli.ssn_dims,
                            "cos": (cos * 1e4).round() / 1e4,
                            "sin_corrected": (sin * 1e4).round() / 1e4,
                            "verdict": if cos > 0.5 { "похожи" } else if cos < 0.0 { "непохожи" } else { "нейтральны" },
                        })
                    );
                } else {
                    println!("CSE-сходство (D={}): cos = {:+.4}, sin-коррекция = {:+.4}", cli.ssn_dims, cos, sin);
                    println!("вердикт: {}", if cos > 0.5 { "похожи" } else if cos < 0.0 { "непохожи" } else { "нейтральны" });
                }
            }
            None => {
                if cli.ssn_json {
                    let head: Vec<f64> = v.iter().take(8).map(|x| (x * 1e4).round() / 1e4).collect();
                    println!(
                        "{}",
                        json!({
                            "mode": "ssn-encode", "dims": cli.ssn_dims,
                            "chars": text.chars().count(),
                            "norm": ssn::cse::norm(&v),
                            "head": head,
                        })
                    );
                } else {
                    println!("CSE-вектор (D={}, {} символов, норма {:.6}):", cli.ssn_dims, text.chars().count(), ssn::cse::norm(&v));
                    for (i, x) in v.iter().take(8).enumerate() {
                        println!("  v[{i:3}] = {x:+.6}");
                    }
                }
            }
        }
        return 0;
    }

    // ── Режим 2: --ssn-demo / --ssn-inject — живой мозг ──
    let t0 = std::time::Instant::now();
    let mut eng = ssn::SsnEngine::new(cfg, cli.ssn_seed, cli.ssn_dims);

    let injected = cli.ssn_inject.clone().map(|text| {
        let (chars, touched) = eng.inject_text(&text);
        (text, chars, touched)
    });

    let steps = cli.ssn_steps.max(1);
    let checkpoint = (steps / 10).max(1);
    let mut traj: Vec<(usize, f64)> = Vec::new();
    if !cli.ssn_json {
        println!(
            "Синаптический Вихрь SSN: N={} нейронов × {} полей = {} синапсов (виртуальная топология, 0 RAM на граф)",
            cli.ssn_n,
            cli.ssn_fields,
            cli.ssn_n * cli.ssn_fields
        );
        if let Some((text, chars, touched)) = &injected {
            println!("сенсорная инъекция: «{text}» — {chars} символов → {touched} нейронов");
        }
        println!("прогон {steps} шагов (доказанный режим: цель 5%, порог спайка 0.1):");
        println!("  шаг    акт  f_sys  b_тон    S     C   E/I  GABA  DA   5HT   NE");
    }
    for i in 0..steps {
        let act = eng.step();
        let done = i + 1;
        if done % checkpoint == 0 || done == steps {
            traj.push((done, act));
            if !cli.ssn_json {
                let t = eng.telemetry();
                println!(
                    "{:>6} {:>6.4} {:>6.4} {:>6.3} {:>5.2} {:>5.2} {:>5.2} {:>5.2} {:>4.2} {:>5.2} {:>5.2}",
                    done, t.activity, t.f_sys, t.b_tone, t.synchrony, t.criticality, t.ei, t.gaba, t.da, t.ht, t.ne
                );
            }
        }
    }

    let t = eng.telemetry();
    let readout = eng.readout(cli.ssn_readout);
    let elapsed = t0.elapsed();
    let sps = steps as f64 / elapsed.as_secs_f64();

    if cli.ssn_json {
        let traj_json: Vec<serde_json::Value> = traj
            .iter()
            .map(|(s, a)| json!({"step": s, "activity": (a * 1e4).round() / 1e4}))
            .collect();
        let readout_json: Vec<serde_json::Value> = readout
            .iter()
            .map(|(i, v)| json!({"neuron": i, "activation": (v * 1e4).round() / 1e4}))
            .collect();
        println!(
            "{}",
            json!({
                "mode": if injected.is_some() { "ssn-inject" } else { "ssn-demo" },
                "n": cli.ssn_n, "fields": cli.ssn_fields, "seed": cli.ssn_seed,
                "steps": steps,
                "injected_text": injected.as_ref().map(|(t, _, _)| t.clone()),
                "telemetry": {
                    "activity": (t.activity * 1e4).round() / 1e4,
                    "f_sys": (t.f_sys * 1e4).round() / 1e4,
                    "b_tone": (t.b_tone * 1e4).round() / 1e4,
                    "synchrony": (t.synchrony * 1e4).round() / 1e4,
                    "criticality": (t.criticality * 1e4).round() / 1e4,
                    "ei": (t.ei * 1e2).round() / 1e2,
                    "gaba": t.gaba, "glut": t.glut, "da": t.da, "ht_5ht": t.ht, "ne": t.ne,
                    "w_min": (t.w_min * 1e3).round() / 1e3, "w_max": (t.w_max * 1e3).round() / 1e3,
                    "w_at_clip": (t.w_at_clip * 1e4).round() / 1e4,
                    "active_count": t.active_count,
                },
                "readout": readout_json,
                "trajectory": traj_json,
                "elapsed_ms": elapsed.as_millis() as u64,
                "steps_per_sec": sps as u64,
            })
        );
    } else {
        println!("\nздоровье мозга: активность {:.1}% (цель 5%), S={:.2} (эпилепсия <0.8), C={:.2} (критичность), E/I={:.1} (норма ~4)",
            t.activity * 100.0, t.synchrony, t.criticality, t.ei);
        println!("веса: [{:+.3}, {:+.3}], у клипа {:.1}% | тон={:.3} | DA={:.2} 5HT={:.2} NE={:.2} | GABA={:.2} глут={:.2}",
            t.w_min, t.w_max, t.w_at_clip * 100.0, t.b_tone, t.da, t.ht, t.ne, t.gaba, t.glut);
        if !readout.is_empty() {
            print!("readout (топ-{}): ", readout.len());
            for (i, v) in &readout {
                print!("[{i}] {v:.3}  ");
            }
            println!();
        }
        println!("время: {} мс ({:.0} шагов/с)", elapsed.as_millis(), sps);
    }
    0
}

// ---------- S2/v0.36.0: Триединая Архитектура (муха + вихрь + кристалл) ----------

/// Загрузка кристалла: внешний .t5c, постоянная память или зашитый в бинарник.
fn triune_load_crystal(cli: &Cli) -> Result<poler_engine::triune::Crystal, String> {
    if let Some(path) = &cli.triune_crystal {
        let bytes = std::fs::read(path).map_err(|e| format!("чтение {path:?}: {e}"))?;
        return poler_engine::triune::Crystal::load(&bytes, poler_engine::triune::crystal::DEFAULT_DIMS);
    }
    // Автоматический поиск постоянной обученной памяти
    for default_path in &["mega_corpus.t5c", "permanent_memory.t5c"] {
        let p = std::path::Path::new(default_path);
        if p.exists() {
            if let Ok(bytes) = std::fs::read(p) {
                if let Ok(c) = poler_engine::triune::Crystal::load(&bytes, poler_engine::triune::crystal::DEFAULT_DIMS) {
                    return Ok(c);
                }
            }
        }
    }
    poler_engine::triune::Crystal::embedded()
}

/// Пульс мухи: настоящая каста из коннектома или виртуальная (seed).
fn triune_load_fly(cli: &Cli) -> Result<poler_engine::triune::FlyPulse, String> {
    match &cli.triune_connectome {
        Some(csr) => {
            let con = poler_engine::graph::connectome::Connectome::load(csr)
                .map_err(|e| format!("коннектом {csr:?}: {e}"))?;
            let seeds: Vec<usize> = cli
                .triune_seeds
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .collect();
            if seeds.is_empty() {
                return Err("--triune-seeds: укажите CSV индексов нейронов".into());
            }
            let cast = poler_engine::triune::flypulse::cast_from_connectome(&con, &seeds, 2)?;
            Ok(poler_engine::triune::FlyPulse::from_cast(&cast, cli.triune_gamma))
        }
        None => Ok(poler_engine::triune::FlyPulse::synthetic(cli.triune_seed, cli.triune_gamma)),
    }
}

/// Телеметрия вихря → JSON (общая для run_triune и MCP).
fn triune_telemetry_json(t: &poler_engine::ssn::VortexTelemetry) -> serde_json::Value {
    let r = |x: f64| (x * 1e6).round() / 1e6;
    serde_json::json!({
        "step": t.step, "activity": r(t.activity), "f_sys": r(t.f_sys),
        "b_tone": r(t.b_tone), "synchrony": r(t.synchrony),
        "criticality": r(t.criticality), "ei": r(t.ei),
        "gaba": r(t.gaba), "glut": r(t.glut),
        "da": r(t.da), "ht_5": r(t.ht), "ne": r(t.ne),
        "w_min": r(t.w_min), "w_max": r(t.w_max), "w_at_clip": r(t.w_at_clip),
        "active_count": t.active_count,
    })
}

/// Высказывание → JSON (агентам: полный провенанс каждого токена).
fn triune_utterance_json(u: &poler_engine::triune::Utterance) -> serde_json::Value {
    use poler_engine::triune::PulseOrigin;
    let r = |x: f64| (x * 1e4).round() / 1e4;
    let trace: Vec<serde_json::Value> = u
        .trace
        .iter()
        .map(|t| {
            serde_json::json!({
                "token": t.token, "idx": t.idx,
                "sem": r(t.sem), "gate": r(t.gate), "syn": t.syn,
                "fly": r(t.fly), "rep": r(t.rep), "score": r(t.score),
                "tau": r(t.tau), "activity": r(t.activity), "act": r(t.act),
            })
        })
        .collect();
    let intents: Vec<serde_json::Value> = u
        .intents
        .iter()
        .map(|i| {
            serde_json::json!({
                "verb": i.verb, "op": i.op.as_str(), "object": i.object,
                "proposal": i.proposal, "allowed": i.allowed, "reason": i.reason,
            })
        })
        .collect();
    let fly = match &u.fly_origin {
        PulseOrigin::Connectome { members, top_rotor } => serde_json::json!({
            "kind": "connectome", "members": members, "top_rotor": top_rotor
        }),
        PulseOrigin::Synthetic { seed } => serde_json::json!({
            "kind": "synthetic", "seed": seed
        }),
    };
    serde_json::json!({
        "text": u.text,
        "tokens": u.trace.len(),
        "crystal_vocab": u.crystal_vocab,
        "fly": fly,
        "fly_drift": u.fly_drift.iter().map(|v| r(*v as f64)).collect::<Vec<_>>(),
        "telemetry": triune_telemetry_json(&u.telemetry),
        "trace": trace,
        "motor_intents": intents,
    })
}

fn run_triune(cli: &Cli) -> i32 {
    use poler_engine::triune::{TriuneConfig, TriuneCore};
    use serde_json::json;

    let t0 = std::time::Instant::now();
    let crystal = match triune_load_crystal(cli) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("triune: {e}");
            return 2;
        }
    };
    let fly = match triune_load_fly(cli) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("triune: {e}");
            return 2;
        }
    };
    let mut cfg = TriuneConfig::default();
    if cli.triune_no_learn {
        cfg.neurons.learn = false;
    }
    let mut core = TriuneCore::new(crystal, cfg, fly, cli.triune_seed);

    let prompts: Vec<String> = if cli.triune_demo {
        vec![
            "живой мозг говорит".into(),
            "система слушает сенсорный вход".into(),
            "открой терминал и покажи статус".into(),
        ]
    } else {
        vec![cli.triune_speak.clone().unwrap_or_default()]
    };

    let mut utterances = Vec::new();
    for prompt in &prompts {
        let u = core.speak(prompt, cli.triune_tokens);
        utterances.push((prompt.clone(), u));
    }

    if cli.triune_json {
        let arr: Vec<serde_json::Value> = utterances
            .iter()
            .map(|(p, u)| {
                json!({"prompt": p, "seed": cli.triune_seed, "gamma": cli.triune_gamma,
                       "utterance": triune_utterance_json(u)})
            })
            .collect();
        println!("{}", json!({"mode": "triune", "elapsed_ms": t0.elapsed().as_millis(), "speech": arr}));
        return 0;
    }

    // Человекочитаемая витрина.
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║   ТРИЕДИНАЯ АРХИТЕКТУРА: муха + вихрь + кристалл → речь    ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    let fly_desc = match utterances.first().map(|(_, u)| &u.fly_origin) {
        Some(poler_engine::triune::PulseOrigin::Connectome { members, top_rotor }) => {
            format!("настоящая каста FLYCSR1: {members} нейронов, топ-ротор {top_rotor}")
        }
        Some(poler_engine::triune::PulseOrigin::Synthetic { seed }) => {
            format!("виртуальная муха (seed {seed}; подключи --triune-connectome для настоящей)")
        }
        None => "—".into(),
    };
    println!("муха:    {fly_desc}");
    println!("вихрь:   SSN v0.35.0, гомеостаз 5%, критичность на краю хаоса");
    println!("кристалл: {} токенов, Trit5 (5 тритов/байт), зашит в бинарник",
        utterances.first().map(|(_, u)| u.crystal_vocab).unwrap_or(0));
    println!("нейроны: v0.38.0, {} живых, ε={:.2}, S={:.2}, Хебб-обновлений: {}",
        utterances.last().map(|(_, u)| u.neurons.active_k).unwrap_or(0),
        utterances.last().map(|(_, u)| u.neurons.energy).unwrap_or(0.0),
        utterances.last().map(|(_, u)| u.neurons.entropy).unwrap_or(0.0),
        utterances.last().map(|(_, u)| u.neurons.hebb_updates).unwrap_or(0));
    for (prompt, u) in &utterances {
        println!("\n─ промпт: «{prompt}» ─────────────────────────────────");
        println!("речь: {text}", text = u.text);
        let t = &u.telemetry;
        println!("мозг: активность {:.1}%, S={:.2}, C={:.2}, DA={:.2} 5HT={:.2} NE={:.2}",
            t.activity * 100.0, t.synchrony, t.criticality, t.da, t.ht, t.ne);
        if let Some(first) = u.trace.first() {
            println!("первый токен: sem={:+.3} gate={:.3} syn={:+} fly={:+.3} act={:.3} τ={:.2}",
                first.sem, first.gate, first.syn, first.fly, first.act, first.tau);
        }
        if !u.intents.is_empty() {
            println!("моторный слой (S2):");
            for i in &u.intents {
                if i.allowed {
                    println!("  [допущено] {} → {}", i.verb, i.proposal);
                } else {
                    println!("  [отклонено] {} — {}", i.verb, i.reason);
                }
            }
        }
    }
    let (inj, spoken, _) = core.state_summary();
    println!("\nитог: {} фраз, {} токенов, {} сенсорных инъекций, {} мс",
        utterances.len(), spoken, inj, t0.elapsed().as_millis());
    // v0.38.0: сохранение синапсов, обученных в сессии (непрерывное обучение).
    if !cli.triune_no_learn {
        let save_target = if let Some(out) = &cli.triune_out {
            Some(out.clone())
        } else if let Some(c_path) = &cli.triune_crystal {
            Some(c_path.clone())
        } else if std::path::Path::new("mega_corpus.t5c").exists() {
            Some(std::path::PathBuf::from("mega_corpus.t5c"))
        } else if std::path::Path::new("permanent_memory.t5c").exists() {
            Some(std::path::PathBuf::from("permanent_memory.t5c"))
        } else {
            None
        };

        if let Some(target) = save_target {
            match core.crystal.save(&target) {
                Ok(()) => println!("память обновлена (онлайн-обучение): {} ({} токенов, {} синапсов)",
                    target.display(), core.crystal.vocab(), core.crystal.bigram_nonzeros),
                Err(e) => eprintln!("triune auto-save: {e}"),
            }
        }
    }
    0
}

fn run_crystal_build(cli: &Cli) -> i32 {
    use poler_engine::triune::Crystal;
    let corpus_path = cli.crystal_build.as_ref().unwrap();
    let corpus = match std::fs::read_to_string(corpus_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("crystal-build: чтение {corpus_path:?}: {e}");
            return 2;
        }
    };
    let crystal = match Crystal::build(
        &corpus,
        cli.crystal_vocab,
        poler_engine::triune::crystal::DEFAULT_DIMS,
        poler_engine::triune::crystal::DEFAULT_THETA_HI,
        poler_engine::triune::crystal::DEFAULT_THETA_LO,
    ) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("crystal-build: {e}");
            return 2;
        }
    };
    let out = cli.crystal_out.clone().unwrap_or_else(|| {
        let mut p = corpus_path.clone();
        p.set_extension("t5c");
        p
    });
    let bytes = crystal.to_bytes();
    if let Err(e) = crystal.save(&out) {
        eprintln!("crystal-build: {e}");
        return 2;
    }
    println!("кристалл собран: {}", out.display());
    println!("  словарь: {} токенов (порог включения θ_hi={})",
        crystal.vocab(), crystal.theta_hi());
    println!("  корпус: {} слов, {} символов", crystal.corpus_words, crystal.corpus_chars);
    println!("  биграммы: {} ненулевых тритов (Trit5, 5 тритов/байт)", crystal.bigram_nonzeros);
    println!("  файл: {} байт, sha256 защищён", bytes.len());
    0
}

fn run_crystal_ingest_dir(cli: &Cli) -> i32 {
    use poler_engine::triune::{IngestConfig, StreamCrystalBuilder};
    let dir = cli.crystal_ingest_dir.as_ref().unwrap();
    let mut cfg = IngestConfig::default();
    cfg.vocab = cli.crystal_vocab;
    let mut builder = StreamCrystalBuilder::new(cfg);
    println!("потоковая ингестия текстов из: {}", dir.display());
    match builder.feed_dir(dir) {
        Ok(count) => {
            println!("  прочитано файлов: {count}");
        }
        Err(e) => {
            eprintln!("crystal-ingest-dir: ошибка чтения директории {dir:?}: {e}");
            return 2;
        }
    }
    let (crystal, stats) = match builder.finalize() {
        Ok(res) => res,
        Err(e) => {
            eprintln!("crystal-ingest-dir: ошибка финализации: {e}");
            return 2;
        }
    };
    let out = cli.crystal_out.clone().unwrap_or_else(|| {
        let mut p = dir.clone();
        p.set_extension("t5c");
        p
    });
    if let Err(e) = crystal.save(&out) {
        eprintln!("crystal-ingest-dir: {e}");
        return 2;
    }
    println!("кристалл успешно скомпилирован: {}", out.display());
    println!("  словарь: {} токенов", stats.crystal_vocab);
    println!("  корпус: {} слов, {} символов", stats.total_words, stats.total_chars);
    println!("  размер: {} байт (Trit5, 5 тритов/байт, No-Mul)", stats.crystal_bytes);
    println!("  sha256: {}", stats.sha256_hex);
    0
}

fn run_gen_archetype_asm(cli: &Cli) -> i32 {
    use poler_engine::universal_archetype_asm::ArchetypeAsmGenerator;
    let out_path = cli.gen_archetype_asm.as_ref().unwrap();
    let min_lines = cli.gen_archetype_lines;
    println!("генерація розгорнутого x86_64 асемблерного алгоритму матриці архетипів...");
    println!("  цільовий файл: {}", out_path.display());
    println!("  мінімальна кількість рядків: {min_lines}");

    let gen = ArchetypeAsmGenerator::new();
    let t0 = std::time::Instant::now();
    match gen.write_unrolled_asm_to_file(out_path, min_lines) {
        Ok(count) => {
            let elapsed = t0.elapsed();
            println!("  успішно згенеровано рядків: {count}");
            println!("  час генерації: {:.2?}", elapsed);
            0
        }
        Err(e) => {
            eprintln!("gen-archetype-asm: помилка запису файлу: {e}");
            2
        }
    }
}

/// `--learn-web <URL>` / `--learn-dir <DIR>`: автономный инжектор памяти —
/// потоковое обучение Кристалла Знаний без удержания корпуса в RAM.
///
/// Веб-режим повторяет конвейер `--crawl` (robots, politeness, дедупликация
/// в SQLite-индекс), но каждая викачанная страница параллельно льётся в
/// [`StreamCrystalBuilder`] через декоратор [`IngestingFetcher`].
fn run_learn(cli: &Cli) -> i32 {
    use poler_engine::triune::{IngestConfig, IngestingFetcher, StreamCrystalBuilder};
    let t0 = std::time::Instant::now();

    // Словарь: 384 (дефолт флага) для обучения мал — поднимаем до 4096.
    let mut vocab = cli.crystal_vocab;
    if vocab == 384 {
        vocab = 4096;
        eprintln!(
            "learn: --crystal-vocab не задан, поднят 384 → 4096 \
             (для больших дампов укажите до 65536)"
        );
    }
    let vocab = vocab.clamp(32, 65_536);
    if vocab > 16_384 {
        let dense_mb = vocab as f64 * ((vocab as f64 + 4.0) / 5.0) / 1e6;
        eprintln!(
            "learn: ВНИМАНИЕ: плотная биграммная матрица {vocab}×{} ≈ {dense_mb:.0} МБ",
            (vocab + 4) / 5
        );
    }
    let cfg = IngestConfig {
        vocab,
        dims: poler_engine::triune::crystal::DEFAULT_DIMS,
        theta_hi: poler_engine::triune::crystal::DEFAULT_THETA_HI,
        theta_lo: poler_engine::triune::crystal::DEFAULT_THETA_LO,
        chunk_bytes: cli.learn_chunk.max(1024),
        word_cap: cli.learn_word_cap.max(1024),
        bigram_cap: cli.learn_bigram_cap.max(1024),
    };
    let mut builder = StreamCrystalBuilder::new(cfg);

    // ── Источник 1: локальная папка ──
    if let Some(dir) = &cli.learn_dir {
        eprintln!("learn-dir: потоковое чтение {}", dir.display());
        match builder.feed_dir(dir) {
            Ok(n) => eprintln!("learn-dir: прочитано файлов: {n}"),
            Err(e) => {
                eprintln!("learn-dir: {e}");
                return 2;
            }
        }
    }

    // ── Источник 2: веб-краул (страницы → кристалл на лету) ──
    if let Some(seed) = &cli.learn_web {
        if !seed.starts_with("http://") && !seed.starts_with("https://") {
            eprintln!("learn-web: ожидается URL (http(s)://...), получено: {seed}");
            return 2;
        }
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let mut ix = match poler_engine::web::WebIndex::open(&db) {
            Ok(ix) => ix,
            Err(e) => {
                eprintln!("learn-web: веб-индекс {db:?}: {e}");
                return 2;
            }
        };
        let fetcher = match poler_engine::web::cdp_fetcher_with_timeout(
            cli.cdp_port,
            cli.web_wait_ms,
            cli.crawl_page_timeout_ms,
        ) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("learn-web: Chromium CDP (порт {}): {e}", cli.cdp_port);
                eprintln!("  автозапуск не удался: установите POLER_CHROME_BIN или запустите вручную:");
                eprintln!("  chrome --headless --remote-debugging-port={} --no-sandbox", cli.cdp_port);
                return 2;
            }
        };
        let crawl_cfg = poler_engine::web::CrawlConfig {
            max_pages: cli.crawl_max.max(1),
            max_depth: cli.crawl_depth,
            delay_ms: cli.crawl_delay_ms,
            cross_site: cli.cross_site,
            wait_ms: cli.web_wait_ms,
            page_timeout_ms: cli.crawl_page_timeout_ms,
            respect_robots: true,
        };
        eprintln!(
            "learn-web: seed {seed}, глубина ≤ {}, до {} страниц, база {db:?}",
            crawl_cfg.max_depth, crawl_cfg.max_pages
        );
        let mut ing = IngestingFetcher::new(Box::new(fetcher), &mut builder);
        match poler_engine::web::crawl::crawl(&mut ix, &mut ing, seed, &crawl_cfg, cli.verbose) {
            Ok(s) => {
                eprintln!(
                    "learn-web: {} загружено, {} проиндексировано, {} без изменений, \
                     {} дубликатов, {} robots-запретов, {} ошибок",
                    s.fetched, s.indexed, s.unchanged, s.duplicates, s.skipped_robots, s.errors
                );
                eprintln!("learn-web: {} страниц скормлено в кристалл", ing.pages);
            }
            Err(e) => {
                eprintln!("learn-web: {e}");
                return 2;
            }
        }
    }

    // ── Сборка кристалла ──
    let (crystal, stats) = match builder.finalize() {
        Ok(res) => res,
        Err(e) => {
            eprintln!("learn: {e}");
            return 2;
        }
    };
    let out = cli
        .crystal_out
        .clone()
        .unwrap_or_else(|| std::path::PathBuf::from("memory.t5c"));
    if let Err(e) = crystal.save(&out) {
        eprintln!("learn: {e}");
        return 2;
    }
    println!("╔════════════════════════════════════════════════════════════╗");
    println!("║   АВТОНОМНЫЙ ИНТЕРНЕТ-ИНЖЕКТОР ПАМЯТИ: кристалл готов       ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!("артефакт:  {}", out.display());
    println!("словарь:   {} токенов (Trit5, 5 тритов/байт)", stats.crystal_vocab);
    println!("корпус:    {} слов, {} символов, {} источников", stats.total_words, stats.total_chars, stats.total_sources);
    println!("топология: {} ненулевых биграмм, sha256 {}", stats.bigram_nonzeros, &stats.sha256_hex[..16.min(64)]);
    println!(
        "RAM:       пик текстового буфера {} Б (лимит {} + 3), эвакуаций: {} слов / {} биграмм",
        stats.peak_text_buffer, cli.learn_chunk, stats.words_evicted, stats.bigrams_evicted
    );
    if stats.files_skipped_binary > 0 {
        println!("фильтр:    {} бинарных файлов пропущено", stats.files_skipped_binary);
    }
    println!("время:     {} мс", t0.elapsed().as_millis());
    println!();
    println!("говорить выученными словами:");
    println!("  poler-engine --triune-speak \"живой мозг\" --triune-crystal {}", out.display());
    0
}

fn run_stream_quant(cli: &Cli) -> i32 {
    use poler_engine::triune::{stream_quantize, verify_t5q, StreamQuantConfig};
    let input = cli.stream_quant.as_ref().unwrap();
    let out_path = cli.stream_quant_out.clone().unwrap_or_else(|| {
        let mut p = input.clone();
        p.set_extension("t5q");
        p
    });
    let cfg = StreamQuantConfig {
        block: cli.stream_quant_block,
        theta: 0.05,
        keep: cli.stream_quant_keep as f32,
        f16: cli.stream_quant_f16,
    };
    let t0 = std::time::Instant::now();
    let stats = {
        let reader: Box<dyn std::io::Read> = if input.as_os_str() == "-" {
            Box::new(std::io::stdin().lock())
        } else {
            let f = match std::fs::File::open(input) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("stream-quant: открытие {input:?}: {e}");
                    return 2;
                }
            };
            Box::new(f)
        };
        let mut out = match std::fs::File::create(&out_path) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("stream-quant: создание {out_path:?}: {e}");
                return 2;
            }
        };
        match stream_quantize(reader, &mut out, &cfg) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("stream-quant: {e}");
                return 2;
            }
        }
    };
    // Верификация записанного (честность формата).
    let written = std::fs::read(&out_path).unwrap_or_default();
    let ok = verify_t5q(&written).is_ok();
    println!("потоковое квантование: {} → {}", input.display(), out_path.display());
    println!("  значений: {} ({} блоков по {}), вход {} байт",
        stats.values, stats.blocks, cfg.block, stats.in_bytes);
    println!("  выход: {} байт = {:.3} бита/вес, сжатие {:.1}×",
        stats.out_bytes, stats.bits_per_value(),
        stats.in_bytes as f64 / stats.out_bytes.max(1) as f64);
    println!("  вакуум: {:.1}% нулевых тритов (keep = {})",
        stats.vacuum_frac() * 100.0, cfg.keep);
    println!("  пик RAM: {} байт (сырец на диск НЕ писался)", stats.peak_buffer);
    println!("  sha256: {} | время {} мс",
        if ok { "сходится" } else { "ОШИБКА" }, t0.elapsed().as_millis());
    if !ok {
        return 3;
    }
    // Математика масштабирования (честная): 70B FP16 = 140 ГБ →
    // 70e9 весов × bpv бит / 8 = N ГБ (1 ГБ = 1e9 Б).
    let bpv = stats.bits_per_value();
    let gb_70b = 70.0e9 * bpv / 8.0 / 1e9;
    println!("  масштаб 70B: 140 ГБ FP16 → {:.1} ГБ в этом режиме", gb_70b);
    0
}

fn run_literary(cli: &Cli) -> i32 {
    use poler_engine::literary as lit;

    // Гиперпараметры из флагов.
    let params = lit::LiteraryParams {
        eta: cli.literary_eta as f32,
        eta_r: cli.literary_eta_r as f32,
        rho: cli.literary_rho as f32,
        kappa: cli.literary_kappa as f32,
        gamma: cli.literary_gamma as f32,
        lambda_rl: cli.literary_lambda as f32,
        dims: cli.literary_dims,
        max_steps: cli.literary_steps,
        no_mul: cli.literary_no_mul,
        ..Default::default()
    };

    // Мушиная калибровка: коннектом + семена → каста.
    let cast = match &cli.literary_csr {
        Some(csr) => {
            let t0 = std::time::Instant::now();
            let con = match poler_engine::graph::connectome::Connectome::load(csr) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("poler-literary: {e}");
                    return 1;
                }
            };
            let nodes = cli
                .literary_nodes
                .as_ref()
                .map(|p| match poler_engine::graph::connectome::ConnectomeNodes::load(p) {
                    Ok(n) => n,
                    Err(e) => {
                        eprintln!("poler-literary: {e}");
                        std::process::exit(1);
                    }
                });
            // Резолв семян: индекс либо root_id.
            let mut seeds = Vec::new();
            for spec in cli.literary_seeds.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let resolved = spec
                    .parse::<usize>()
                    .ok()
                    .filter(|&i| i < con.n_nodes())
                    .or_else(|| {
                        nodes
                            .as_ref()
                            .and_then(|n| spec.parse::<u64>().ok().and_then(|r| n.idx_of(r)))
                    });
                match resolved {
                    Some(i) => seeds.push(i),
                    None => {
                        eprintln!("poler-literary: нейрон-семя «{spec}» не найден");
                        return 1;
                    }
                }
            }
            if seeds.is_empty() {
                eprintln!("poler-literary: семена пусты (--literary-seeds)");
                return 1;
            }
            match lit::flybridge::build_cast(&con, &seeds, cli.literary_khop, cli.literary_max_cast)
            {
                Ok(c) => {
                    eprintln!(
                        "poler-literary: каста мухи — {} нейронов (BFS достиг {}), {} внутренних \
                         рёбер, масса {}, за {} мс",
                        c.members.len(),
                        c.stats.reached,
                        c.stats.inner_edges,
                        c.stats.inner_abs_mass as u64,
                        t0.elapsed().as_millis()
                    );
                    Some(c)
                }
                Err(e) => {
                    eprintln!("poler-literary: {e}");
                    return 1;
                }
            }
        }
        None => None,
    };

    let mode = if cli.literary_field.is_some() {
        "field"
    } else {
        "generate"
    };
    let text = cli
        .literary_field
        .clone()
        .or_else(|| cli.literary_generate.clone())
        .expect("режим literary проверен выше");

    // ── Режим: анализ поля (℘→O) ────────────────────────────────────
    if mode == "field" {
        let eng = match lit::LiteraryEngine::new(params, &text, &[], cast) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("poler-literary: {e}");
                return 1;
            }
        };
        let top = lit::qualia::cosine_topology(&eng.field);
        if cli.literary_json {
            let mut report = serde_json::json!({
                "mode": "field",
                "dims": eng.params.dims,
                "tokens": eng.field.n_tokens,
                "terms": eng.field.n_terms,
                "intent_terms": eng.field.term_mass.iter().map(|(t, w)| serde_json::json!({
                    "term": t, "mass": (w * 1e4).round() / 1e4,
                })).collect::<Vec<_>>(),
                "archetypes": top.iter().take(6).map(|&(i, w)| serde_json::json!({
                    "name": lit::qualia::archetype_name(i),
                    "cosine": (w * 1e4).round() / 1e4,
                })).collect::<Vec<_>>(),
                "f_initial": (eng.free_energy() * 1e6).round() / 1e6,
            });
            if let Some(c) = &eng.cast {
                report["fly"] = serde_json::json!({
                    "members": c.members.len(),
                    "reached": c.stats.reached,
                    "inner_edges": c.stats.inner_edges,
                    "inner_abs_mass": c.stats.inner_abs_mass as i64,
                    "top_rotor": c.stats.top_rotor,
                });
            }
            println!(
                "{}",
                serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
            );
            return 0;
        }
        println!("ПОЛЕ ЗАМЫСЛА (℘→O)");
        println!(
            "  Токенов: {}, уникальных термов: {}, осей: {}",
            eng.field.n_tokens, eng.field.n_terms, eng.params.dims
        );
        println!("  Инвариант (топ термов):");
        for (t, w) in eng.field.term_mass.iter().take(8) {
            println!("    {t:<16} {w:.3}", w = *w as f64);
        }
        println!("  Архетипическая топология:");
        for &(i, w) in top.iter().take(6) {
            println!("    {:<22} {:>+7.4}", lit::qualia::archetype_name(i), w);
        }
        println!(
            "  F₀ = {:.4} (‖Ω(o)‖² = 1), причинный замок: p₀ − p₁ = 0",
            eng.free_energy()
        );
        if let Some(c) = &eng.cast {
            println!(
                "  МУХА: каста {} нейронов, {} внутренних рёбер, топ-ротор {:?}",
                c.members.len(),
                c.stats.inner_edges,
                c.stats.top_rotor
            );
        }
        return 0;
    }

    // ── Режим: генерация нарратива ─────────────────────────────────
    let mut eng = match lit::LiteraryEngine::new(params, &text, &[], cast) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("poler-literary: {e}");
            return 1;
        }
    };
    let t0 = std::time::Instant::now();
    let report = eng.generate();
    if cli.literary_json {
        let mut jr = serde_json::json!({
            "mode": "generate",
            "steps": report.steps,
            "converged": report.converged,
            "f_final": (report.final_f * 1e6).round() / 1e6,
            "h_psi_final": (report.final_h_psi * 1e6).round() / 1e6,
            "causality_clean": report.causality_clean,
            "refractions": report.refractions,
            "elapsed_ms": t0.elapsed().as_millis() as u64,
            "acts": report.acts.iter().map(|a| serde_json::json!({
                "steps": [a.steps.0, a.steps.1],
                "f": [(a.f_start * 1e4).round() / 1e4, (a.f_end * 1e4).round() / 1e4],
                "dominants": a.dominants.iter().map(|d| serde_json::json!({
                    "archetype": d.name, "weight": (d.weight * 1e4).round() / 1e4,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "dramatic_pairs": report.dramatic_pairs.iter().map(|p| serde_json::json!({
                "u": p.u, "v": p.v, "j": (p.j * 1e4).round() / 1e4,
                "u_archetype": p.u_archetype, "v_archetype": p.v_archetype,
            })).collect::<Vec<_>>(),
        });
        jr["text"] = serde_json::json!(report.text);
        println!(
            "{}",
            serde_json::to_string_pretty(&jr).unwrap_or_else(|_| "{}".into())
        );
        return 0;
    }
    println!("{}", report.text);
    println!(
        "  Время: {} мс ({} шагов)",
        t0.elapsed().as_millis(),
        report.steps
    );
    0
}
