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

    // ---------- MCP-сервер: stdio JSON-RPC для LLM-агентов ----------
    if cli.mcp {
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let code = poler_engine::mcp::run(cli.cdp_port, cli.web_wait_ms, db, cli.knowledge_db.clone());
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
        let code = poler_engine::mcp_http::run_http(&bind, &token, cli.cdp_port, cli.web_wait_ms, db, cli.knowledge_db.clone());
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
        let code = poler_engine::mcp_http::run_http(&bind, &token, cli.cdp_port, cli.web_wait_ms, db, cli.knowledge_db.clone());
        return ExitCode::from(code as u8);
    }

    // ---------- v0.28.1: Листинг архива без распаковки ----------
    if let Some(archive) = cli.archive_list.clone() {
        return ExitCode::from(run_archive_list(&cli, &archive) as u8);
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
