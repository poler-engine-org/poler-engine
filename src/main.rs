//! CLI poler-engine: AI-Native Topographical, Resonant and Graph Search Engine.
//!
//! Режимы:
//! * поиск: `poler-engine <PATH> -q <QUERY> [--watch]`
//! * impact-анализ (AIDDE): `poler-engine <PATH> --impact <SYMBOL>`
//! * веб-поиск для AI: `poler-engine --web-search <QUERY>` (по веб-индексу)
//! * краулинг: `poler-engine <URL> --crawl [--crawl-depth N --crawl-max M]`
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
    #[arg(long = "web-lens", value_name = "BIND", num_args = 0..=1, default_missing_value = "127.0.0.1:8765", conflicts_with_all = ["web", "crawl", "web_search", "web_stats", "mcp", "mcp_http", "shell", "tui", "impact", "browser_index", "web_lens_install", "google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch"])]
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
    /// poler_gmail / poler_drive.
    #[arg(long = "mcp", conflicts_with_all = ["web_search", "crawl", "web_stats", "impact", "mcp_http", "mcp_token"])]
    mcp: bool,

    /// MCP-СЕРВЕР ПО HTTP (Streamable HTTP): тот же набор инструментов,
    /// что и --mcp, но для УДАЛЁННОГО агента — через туннель (например
    /// `cloudflared tunnel --url http://127.0.0.1:8765`). POST / или /mcp,
    /// заголовок Authorization: Bearer <токен>. Пароли/куки Google наружу
    /// не выходят: движок ходит в NotebookLM своим профилем.
    /// BIND = «127.0.0.1:8765» (по умолчанию) или просто порт «8765».
    #[arg(long = "mcp-http", value_name = "BIND", num_args = 0..=1, default_missing_value = "127.0.0.1:8765", conflicts_with_all = ["mcp", "shell", "tui", "web_search", "crawl", "web_stats", "impact", "google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch"])]
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
    #[arg(long, conflicts_with_all = ["shell", "tui", "mcp", "mcp_http", "web_search", "crawl", "web_stats", "impact", "grep", "chunk", "benchmark", "web_lens", "web_lens_install", "browser_index", "google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch", "auth_ui", "license", "license_import", "semantic_expand", "import_browser_session"])]
    gateway: bool,

    /// Интерактивный REPL (poler> search/nlm/crawl/sync...).
    /// База web-index.db и NlmSession открываются ленимо и переиспользуются
    /// между командами (история в ~/.cache/poler-engine/shell-history.txt).
    #[arg(long = "shell", conflicts_with_all = ["tui", "mcp", "web_search", "crawl", "web_stats", "impact", "google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch"])]
    shell: bool,

    /// TUI Dashboard на ratatui (3 панели: ноутбуки | ввод | результаты).
    /// Tab — смена фокуса, Esc — выход. Команды как в --shell.
    #[arg(long = "tui", conflicts_with_all = ["shell", "mcp", "web_search", "crawl", "web_stats", "impact", "google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch"])]
    tui: bool,

    // ---------- Google-интеграция без пароля (v0.12.0) ----------

    /// OAuth 2.0 loopback: согласие Google в ТВОЁМ браузере (пароль не
    /// попадает в poler-engine), движок получает только узкие readonly-токены.
    /// Нужен client_secret.json своего GCP-проекта (README, раздел v0.12.0).
    #[arg(long = "google-auth", conflicts_with_all = ["google_gmail", "google_drive", "google_status", "google_browse", "google_fetch"])]
    google_auth: bool,

    /// Gmail-поиск по своему ящику (нужен одноразовый --google-auth).
    /// QUERY — синтаксис Gmail: from:vasya has:attachment newer_than:7d …
    /// Без QUERY — недавняя почта.
    #[arg(long = "google-gmail", num_args = 0..=1, default_missing_value = "", conflicts_with_all = ["google_drive", "google_status", "google_browse", "google_fetch"])]
    google_gmail: Option<String>,

    /// Google Drive: файлы по имени (пустой QUERY — недавние).
    #[arg(long = "google-drive", num_args = 0..=1, default_missing_value = "", conflicts_with_all = ["google_status", "google_browse", "google_fetch"])]
    google_drive: Option<String>,

    /// Состояние Google-токенов: скоупы, срок действия, аккаунт.
    #[arg(long = "google-status", conflicts_with_all = ["google_browse", "google_fetch"])]
    google_status: bool,

    /// Открыть URL в браузере с ПЕРСИСТЕНТНЫМ профилем poler-engine
    /// (для сервисов без API — NotebookLM и т.п.: логин один раз своими руками).
    #[arg(long = "google-browse", value_name = "URL", conflicts_with_all = ["google_fetch"])]
    google_browse: Option<String>,

    /// Прочитать URL через персистентный профиль headless-ом:
    /// контент авторизованных сервисов после логина через --google-browse.
    #[arg(long = "google-fetch", value_name = "URL")]
    google_fetch: Option<String>,

    /// Дополнительные скоупы OAuth (через пробел), кроме gmail/drive readonly.
    #[arg(long = "google-scopes", value_name = "SCOPES")]
    google_scopes: Option<String>,

    /// v0.17.5 (security): ЯВНЫЙ перенос Google-сессии из основного
    /// браузера хоста (~/.config/chromium) в профиль движка. Спрашивает
    /// [y/N], пишет audit-запись. По умолчанию перенос куков ЗАПРЕЩЁН
    /// (раньше sync_host_chromium_profile тянул их молча).
    #[arg(long = "import-browser-session", conflicts_with_all = ["google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch", "auth_ui"])]
    import_browser_session: bool,

    /// v0.17.6: интерактивное окно авторизации Google (Auth Companion).
    /// Node.js-мост поднимает Chromium с ИЗОЛИРОВАННЫМ профилем движка
    /// (~/.cache/poler-engine/google-profile): логин и 2FA вводишь сам,
    /// движок получает только итоговые куки сессии (google_session.json,
    /// 0600) и закрывает окно за собой. Хост-браузер не трогается,
    /// все эндпоинты — строго 127.0.0.1.
    #[arg(long = "auth-ui", conflicts_with_all = ["google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch", "import_browser_session"])]
    auth_ui: bool,

    // ---------- License Gate (v0.18.0) ----------

    /// Статус лицензии: тир, владелец, срок, квота Community.
    /// Локальный поиск ВСЕГДА без лицензии; гейтятся только интеграции
    /// Gmail/Drive/NotebookLM.
    #[arg(long = "license", conflicts_with_all = ["google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch", "auth_ui", "import_browser_session", "shell", "tui", "mcp", "mcp_http"])]
    license: bool,

    /// Активация лицензии: PO1-ключ одной строкой ИЛИ путь к файлу с ним.
    /// Подпись ed25519 проверяется ДО сохранения; файл — 0600.
    #[arg(long = "license-import", value_name = "KEY|PATH", conflicts_with_all = ["google_auth", "google_gmail", "google_drive", "google_status", "google_browse", "google_fetch", "auth_ui", "import_browser_session", "shell", "tui", "mcp", "mcp_http"])]
    license_import: Option<String>,

    // ---------- NotebookLM через RPC-протокол NLMTools (v0.13.0) ----------

    /// Все ноутбуки NotebookLM с источниками (RPC wXbhsf из протокола
    /// NLMTools.com, сессия персистентного профиля — без пароля).
    #[arg(long = "nlm-notebooks", conflicts_with_all = ["nlm_source", "nlm_notes", "nlm_artifacts", "nlm_account", "nlm_chat", "nlm_media", "nlm_shot", "nlm_sync"])]
    nlm_notebooks: bool,

    /// Контент источника: текст и/или URL картинок слайдов (RPC hizoJc).
    /// NOTEBOOK SRC — id из --nlm-notebooks.
    #[arg(long = "nlm-source", value_names = ["NOTEBOOK", "SOURCE"], num_args = 2, conflicts_with_all = ["nlm_notes", "nlm_artifacts", "nlm_account", "nlm_chat", "nlm_media", "nlm_shot", "nlm_sync"])]
    nlm_source: Option<Vec<String>>,

    /// Заметки ноутбука (RPC cFji9, raw-JSON).
    #[arg(long = "nlm-notes", value_name = "NOTEBOOK", conflicts_with_all = ["nlm_artifacts", "nlm_account", "nlm_chat", "nlm_media", "nlm_shot", "nlm_sync"])]
    nlm_notes: Option<String>,

    /// Studio-объекты: аудио-обзоры, отчёты, квизы, миндмэпы (RPC gArtLc).
    #[arg(long = "nlm-artifacts", value_name = "NOTEBOOK", conflicts_with_all = ["nlm_account", "nlm_chat", "nlm_media", "nlm_shot", "nlm_sync"])]
    nlm_artifacts: Option<String>,

    /// Аккаунт сессии NotebookLM (RPC ZwVcOc) — проверка логина профиля.
    #[arg(long = "nlm-account", conflicts_with_all = ["nlm_chat", "nlm_media", "nlm_shot", "nlm_sync"])]
    nlm_account: bool,

    /// Спросить ноутбук: вопрос печатается в чат страницы, ответ
    /// читается после стабилизации (UI-автоматизация, без пароля).
    #[arg(long = "nlm-chat", value_names = ["NOTEBOOK", "QUESTION"], num_args = 2, conflicts_with_all = ["nlm_media", "nlm_shot", "nlm_sync"])]
    nlm_chat: Option<Vec<String>>,

    /// Скачать медиа-файл (картинка слайда и т.п.) авторизованным
    /// профилем: poler-engine --nlm-media URL → poler-media-N.<ext>.
    #[arg(long = "nlm-media", value_name = "URL", conflicts_with_all = ["nlm_shot", "nlm_sync"])]
    nlm_media: Option<String>,

    /// Скриншот страницы в профиле (PNG): медиа глазами юзера.
    #[arg(long = "nlm-shot", value_name = "URL")]
    nlm_shot: Option<String>,

    /// Синк NotebookLM в web-index: --nlm-sync [NOTEBOOK_ID] вливает
    /// заметки/источники/артефакты в общий индекс poler-engine —
    /// дальше они находятся через --web-search наравне с вебом.
    /// Без NOTEBOOK_ID — синк всех ноутбуков аккаунта.
    #[arg(long = "nlm-sync", value_name = "NOTEBOOK_ID", num_args = 0..=1, default_missing_value = "")]
    nlm_sync: Option<String>,

    /// Максимум результатов Gmail/Drive [default: 10].
    #[arg(long = "google-max", default_value_t = 10)]
    google_max: usize,

    /// Разрешить краулеру переход на другие хосты.
    #[arg(long = "cross-site", default_value_t = false)]
    cross_site: bool,

    /// Поисковый запрос: слово или фраза (в кавычках).
    #[arg(short, long)]
    query: Option<String>,

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

/// Вывод Gmail-выдачи в трёх форматах.
fn print_mail_hits(hits: &[poler_engine::google::api::MailHit], query: &str, format: Format) {
    use std::io::Write;
    match format {
        Format::AiJson => {
            let out = serde_json::json!({
                "engine": "poler-engine",
                "mode": "google-gmail",
                "auth": "OAuth 2.0 (gmail.readonly)",
                "query": query,
                "total": hits.len(),
                "results": hits,
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        Format::Simple => {
            for h in hits {
                let subj: String = h.subject.chars().take(60).collect();
                let from: String = h.from.chars().take(30).collect();
                println!("{}  {}  {}", h.date, from, subj);
            }
        }
        Format::Md => {
            println!("# Gmail: «{query}»\n");
            for (i, h) in hits.iter().enumerate() {
                let subj = if h.subject.is_empty() { "(без темы)" } else { &h.subject };
                println!("## {}. {}\n", i + 1, subj);
                println!("- От: {}", h.from);
                println!("- Дата: {}", h.date);
                println!("- ID: {} (поток {})\n", h.id, h.thread_id);
                let sn: String = h.snippet.chars().take(240).collect();
                println!("> {sn}\n");
            }
        }
    }
    let _ = std::io::stdout().flush();
}

/// Вывод Drive-выдачи в трёх форматах.
fn print_drive_hits(hits: &[poler_engine::google::api::DriveHit], query: &str, format: Format) {
    use std::io::Write;
    match format {
        Format::AiJson => {
            let out = serde_json::json!({
                "engine": "poler-engine",
                "mode": "google-drive",
                "auth": "OAuth 2.0 (drive.readonly)",
                "query": query,
                "total": hits.len(),
                "results": hits,
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        Format::Simple => {
            for h in hits {
                let name: String = h.name.chars().take(70).collect();
                println!("{}  {}  {}", h.modified_time, h.mime_type, name);
            }
        }
        Format::Md => {
            println!("# Google Drive: «{query}»\n");
            for (i, h) in hits.iter().enumerate() {
                println!("## {}. {}\n", i + 1, h.name);
                println!("- Тип: {}", h.mime_type);
                println!("- Изменён: {}", h.modified_time);
                if let Some(b) = h.size_bytes {
                    println!("- Размер: {:.1} КБ", b as f64 / 1024.0);
                }
                if let Some(l) = &h.web_view_link {
                    println!("- Ссылка: {l}");
                }
                println!("- ID: {}\n", h.id);
            }
        }
    }
    let _ = std::io::stdout().flush();
}

/// v0.18.0: человекочитаемый статус лицензии (`--license`).
/// v0.22.0: формат живёт в license::status_text() — единый источник
/// для CLI и команды `license` в Terminal Gateway.
fn print_license_status() -> ExitCode {
    print!("{}", poler_engine::license::status_text());
    ExitCode::SUCCESS
}

/// NotebookLM-режимы (v0.13.0): RPC-протокол NLMTools поверх
/// персистентного профиля. Сессия открывается один раз на вызов.
fn run_nlm(cli: &Cli) -> ExitCode {
    use poler_engine::google::nlm::{self, NlmSession};

    // v0.18.0: License Gate — единая точка для всех --nlm-* режимов.
    if !poler_engine::license::gate_or_print(poler_engine::license::FEATURE_NLM) {
        return ExitCode::from(2);
    }

    let fail = |e: String| {
        eprintln!("poler-engine nlm: {e}");
        ExitCode::from(2)
    };

    // ---- медиа и скриншот не требуют полноценной RPC-сессии ноутбука ----
    if let Some(url) = cli.nlm_shot.clone() {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return fail(format!("--nlm-shot ожидает URL, получено: {url}"));
        }
        let mut s = match NlmSession::open() {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        if let Err(e) = s.load_page_raw(&url) {
            return fail(e);
        }
        return match s.screenshot() {
            Ok(png) => match save_unique("poler-shot", "png", &png) {
                Ok(path) => {
                    println!("скриншот: {} ({} КБ)", path.display(), png.len() / 1024);
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            },
            Err(e) => fail(e),
        };
    }

    if let Some(url) = cli.nlm_media.clone() {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return fail(format!("--nlm-media ожидает URL, получено: {url}"));
        }
        let mut s = match NlmSession::open() {
            Ok(s) => s,
            Err(e) => return fail(e),
        };
        return match s.fetch_media(&url) {
            Ok((bytes, ct)) => {
                let ext = nlm::mime_ext(&ct);
                match save_unique("poler-media", ext, &bytes) {
                    Ok(path) => {
                        println!("медиа: {} ({} КБ, {})", path.display(), bytes.len() / 1024, ct);
                        ExitCode::SUCCESS
                    }
                    Err(e) => fail(e),
                }
            }
            Err(e) => fail(e),
        };
    }

    // ---- RPC-режимы ----
    let mut s = match NlmSession::open() {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    if cli.verbose {
        if let Some(email) = &s.email {
            eprintln!("poler-engine nlm: сессия {email}");
        }
    }

    if cli.nlm_notebooks {
        return match s.list_notebooks() {
            Ok(nbs) => {
                if cli.format == Format::AiJson {
                    let out = serde_json::json!({
                        "engine": "poler-engine",
                        "mode": "nlm-notebooks",
                        "auth": "persistent profile (no password)",
                        "protocol": "batchexecute (NLMTools)",
                        "total": nbs.len(),
                        "results": nbs,
                    });
                    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                } else {
                    println!("{}", nlm::format_notebooks(&nbs));
                }
                if nbs.is_empty() {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => fail(e),
        };
    }

    if let Some(args) = cli.nlm_source.clone() {
        let (nb, src) = (args[0].clone(), args[1].clone());
        return match s.load_source(&nb, &src) {
            Ok(sc) => {
                if cli.format == Format::AiJson {
                    let out = serde_json::json!({
                        "engine": "poler-engine",
                        "mode": "nlm-source",
                        "notebook": nb,
                        "source": sc,
                    });
                    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                } else {
                    println!("{}", nlm::format_source_content(&sc, &nb));
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        };
    }

    if let Some(nb) = cli.nlm_notes.clone() {
        return match s.notes(&nb) {
            Ok(v) => {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        };
    }

    if let Some(nb) = cli.nlm_artifacts.clone() {
        return match s.artifacts(&nb) {
            Ok(arts) => {
                if cli.format == Format::AiJson {
                    let out = serde_json::json!({
                        "engine": "poler-engine",
                        "mode": "nlm-artifacts",
                        "notebook": nb,
                        "total": arts.len(),
                        "results": arts,
                    });
                    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                } else {
                    println!("{}", nlm::format_artifacts(&arts));
                }
                if arts.is_empty() {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => fail(e),
        };
    }

    if cli.nlm_account {
        return match s.account() {
            Ok(v) => {
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        };
    }

    if let Some(args) = cli.nlm_chat.clone() {
        let (nb, q) = (args[0].clone(), args[1].clone());
        return match s.chat(&nb, &q) {
            Ok(answer) => {
                if cli.format == Format::AiJson {
                    let out = serde_json::json!({
                        "engine": "poler-engine",
                        "mode": "nlm-chat",
                        "notebook": nb,
                        "question": q,
                        "answer": answer,
                    });
                    println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                } else {
                    println!("{answer}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => fail(e),
        };
    }

    // ---------- v0.14: синк NotebookLM в web-index ----------
    if let Some(nb) = cli.nlm_sync.clone() {
        use poler_engine::google::nlm_ingest;
        use poler_engine::web::WebIndex;
        let db_path = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let mut ix = match WebIndex::open(&db_path) {
            Ok(ix) => ix,
            Err(e) => return fail(format!("web-index {db_path:?}: {e}")),
        };
        return if nb.is_empty() {
            // --nlm-sync без аргумента — все ноутбуки аккаунта
            eprintln!("poler-engine nlm: синк всех ноутбуков в {db_path:?} (до RPC на источник)…");
            match nlm_ingest::sync_all(&mut ix, &mut s) {
                Ok(stats) => {
                    if cli.format == Format::AiJson {
                        let out = serde_json::json!({
                            "engine": "poler-engine",
                            "mode": "nlm-sync",
                            "scope": "all",
                            "stats": stats,
                        });
                        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                    } else {
                        println!("{}", format_stats(&stats));
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => fail(e),
            }
        } else {
            // --nlm-sync NOTEBOOK_ID — один ноутбук
            let nbs = s.list_notebooks().unwrap_or_default();
            let target = nbs.iter().find(|x| x.id == nb).cloned();
            match target {
                Some(nb_meta) => {
                    let mut stats = nlm_ingest::IngestStats {
                        notebooks: 1,
                        ..Default::default()
                    };
                    if let Err(e) = nlm_ingest::ingest_notebook(&mut ix, &mut s, &nb_meta, &mut stats) {
                        stats.errors.push(format!("ingest_notebook {}: {e}", nb));
                    }
                    let _ = ix.recompute_pagerank(20);
                    if cli.format == Format::AiJson {
                        let out = serde_json::json!({
                            "engine": "poler-engine",
                            "mode": "nlm-sync",
                            "scope": "single",
                            "notebook": nb,
                            "stats": stats,
                        });
                        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
                    } else {
                        println!("{}", format_stats(&stats));
                    }
                    ExitCode::SUCCESS
                }
                None => fail(format!("ноутбук {nb} не найден в аккаунте")),
            }
        };
    }

    fail("nlm: не выбран режим (см. --help)".to_string())
}

/// Сохранить байты в неиспользуемый файл poler-<prefix>-N.<ext> в CWD.
/// Человекочитаемый отчёт о синке NLM в web-index.
fn format_stats(s: &poler_engine::google::nlm_ingest::IngestStats) -> String {
    let mut out = String::new();
    out.push_str(&format!("NLM sync: {} notebooks processed\n", s.notebooks));
    out.push_str(&format!("  passports:   {} reindexed, {} unchanged\n",
        s.notebooks_reindexed, s.notebooks_unchanged));
    out.push_str(&format!("  sources:      {} reindexed, {} unchanged\n",
        s.sources_reindexed, s.sources_unchanged));
    out.push_str(&format!("  notes:        {} reindexed, {} unchanged\n",
        s.notes_reindexed, s.notes_unchanged));
    out.push_str(&format!("  artifacts:    {} reindexed, {} unchanged\n",
        s.artifacts_reindexed, s.artifacts_unchanged));
    out.push_str(&format!("  TOTAL:        {} pages ({} new/changed, {} skipped by Percolator-lite)\n",
        s.total_pages(), s.total_reindexed(), s.total_unchanged()));
    if !s.errors.is_empty() {
        out.push_str(&format!("\n  errors ({}):\n", s.errors.len()));
        for e in &s.errors {
            out.push_str(&format!("    - {e}\n"));
        }
    }
    out.push_str("\nТеперь web-search пробивает NLM-корпус наравне с вебом.\n");
    out
}

fn save_unique(prefix: &str, ext: &str, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    for n in 1..10_000 {
        let p = std::path::PathBuf::from(format!("{prefix}-{n:02}.{ext}"));
        if !p.exists() {
            std::fs::write(&p, bytes).map_err(|e| format!("запись {}: {e}", p.display()))?;
            return Ok(p);
        }
    }
    Err("не найдено свободного имени для файла".to_string())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let code = run(cli);
    // v0.17.5: headless google-браузер, поднятый ЭТИМ процессом, не должен
    // переживать CLI и оставлять открытый CDP-порт без аутентификации.
    // Headed-браузер (--google-browse) не трогаем — его закрывает владелец.
    poler_engine::google::shutdown_owned_headless_browser();
    code
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

    // ---------- MCP-сервер: stdio JSON-RPC для LLM-агентов ----------
    if cli.mcp {
        let db = cli.web_db.clone().unwrap_or_else(poler_engine::web::default_db_path);
        let code = poler_engine::mcp::run(cli.cdp_port, cli.web_wait_ms, db);
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
        let code = poler_engine::mcp_http::run_http(&bind, &token, cli.cdp_port, cli.web_wait_ms, db);
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
        let code = poler_engine::mcp_http::run_http(&bind, &token, cli.cdp_port, cli.web_wait_ms, db);
        return ExitCode::from(code as u8);
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
        };
        // Корни поиска: позиционный PATH (может повторяться неявно —
        // несколько аргументов clap не поддерживает, но PATH может быть
        // каталогом) либо текущий каталог.
        let roots: Vec<std::path::PathBuf> = match cli.path.clone() {
            Some(p) => vec![p],
            None => vec![std::path::PathBuf::from(".")],
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
        return poler_engine::gateway::run_gateway(db_path);
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

    // ---------- v0.17.5: явный перенос сессии из основного браузера ----------
    if cli.import_browser_session {
        return match poler_engine::google::import_browser_session_interactive() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("poler-engine import-browser-session: {e}");
                ExitCode::from(2)
            }
        };
    }

    // ---------- v0.17.6: Auth Companion — изолированное окно логина ----------
    if cli.auth_ui {
        return match poler_engine::google::auth_ui::run_auth_ui() {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("poler-engine auth-ui: {e}");
                ExitCode::from(2)
            }
        };
    }

    // ---------- v0.18.0: License Gate — статус и активация ----------
    if cli.license {
        return print_license_status();
    }
    if let Some(k) = cli.license_import.clone() {
        return match poler_engine::license::import(&k) {
            Ok(lic) => {
                println!("✓ Лицензия активирована");
                println!("  Тир:       {}", lic.tier);
                println!("  Владелец:  {} <{}>", lic.name, lic.email);
                if lic.expires == 0 {
                    println!("  Срок:      бессрочно");
                } else {
                    println!(
                        "  Действует до: {} ({} дн.)",
                        poler_engine::license::civil_date(lic.expires),
                        lic.expires.saturating_sub(poler_engine::license::now_unix()) / 86400
                    );
                }
                if !lic.features.is_empty() {
                    println!("  Функции:   {}", lic.features.join(", "));
                }
                println!("  Сохранено: ~/.config/poler-engine/license.key (0600)");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("poler-engine license-import: {e}");
                ExitCode::from(2)
            }
        };
    }

    // ---------- Google-сервисы: OAuth без пароля (v0.12.0) ----------
    if cli.google_auth {
        let extra: Vec<String> = cli
            .google_scopes
            .clone()
            .map(|s| s.split_whitespace().map(String::from).collect())
            .unwrap_or_default();
        return match poler_engine::google::oauth::run_auth(&extra) {
            Ok(t) => {
                println!("\nГотово. Теперь доступны:");
                println!("  poler-engine --google-gmail \"from:me newer_than:7d\"");
                println!("  poler-engine --google-drive \"отчёт\"");
                println!("  poler-engine --google-status");
                println!("  poler-engine --nlm-notebooks  (NotebookLM — через профиль");
                println!("   браузера движка, НЕ отдельный OAuth-скоуп)");
                let _ = t;
                // v0.17.7: гарантированный выход после успешного OAuth.
                // Наблюдалось: при переиспользовании уже поднятого CDP-браузера
                // (порт 9223 занят другим процессом) CLI изредка не завершался
                // после печати результатов — вся работа сделана, токены
                // сохранены, но процесс жил минутами. hard-exit надёжен для CLI.
                // СВОЙ headless-браузер закрываем ЯВНО до exit: process::exit
                // пропускает shutdown в main() (регресс v0.17.5 недопустима —
                // CDP-порт без аутентификации не должен переживать CLI).
                poler_engine::google::shutdown_owned_headless_browser();
                use std::io::Write as _;
                let _ = std::io::stdout().flush();
                std::process::exit(0);
            }
            Err(e) => {
                eprintln!("poler-engine google-auth: {e}");
                ExitCode::from(2)
            }
        };
    }

    if let Some(url) = cli.google_browse.clone() {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            eprintln!("poler-engine: --google-browse ожидает URL, получено: {url}");
            return ExitCode::from(2);
        }
        return match poler_engine::google::browse(&url) {
            Ok(()) => {
                println!("Браузер poler-engine открыт (персистентный профиль):");
                println!("  {:?}", poler_engine::google::profile_dir());
                println!("URL: {url}");
                println!();
                println!("Залогинься СВОИМИ руками — пароль остаётся между тобой и Google.");
                println!("Сессия сохранится в профиль; дальше читай контент:");
                println!("  poler-engine --google-fetch {url}");
                println!("Окно браузера закрой сам, когда закончишь.");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("poler-engine google-browse: {e}");
                ExitCode::from(2)
            }
        };
    }

    if let Some(url) = cli.google_fetch.clone() {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            eprintln!("poler-engine: --google-fetch ожидает URL, получено: {url}");
            return ExitCode::from(2);
        }
        return match poler_engine::google::fetch_profiled(&url, cli.web_wait_ms) {
            Ok(page) => {
                if page.text.trim().is_empty() {
                    eprintln!(
                        "poler-engine google-fetch: пустой рендер (нет логина для этого сервиса? \
                         см. --google-browse <URL>): {url}"
                    );
                    ExitCode::from(1)
                } else {
                    let max_chars = 20_000;
                    let text = page.text.trim();
                    let shown: String = text.chars().take(max_chars).collect();
                    println!("# {url}\n");
                    println!("{shown}");
                    if text.chars().count() > max_chars {
                        eprintln!(
                            "… (обрезано до {max_chars} символов из {})",
                            text.chars().count()
                        );
                    }
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                eprintln!("poler-engine google-fetch: {e}");
                ExitCode::from(2)
            }
        };
    }

    if cli.google_status {
        // Одна строка о тире — прозрачность лицензии рядом с OAuth-статусом.
        let lst = poler_engine::license::status();
        match &lst.license {
            Some(lic) => eprintln!(
                "Лицензия: {} ({}) — до {}",
                lic.tier,
                lic.name,
                if lic.expires == 0 {
                    "бессрочно".to_string()
                } else {
                    poler_engine::license::civil_date(lic.expires)
                }
            ),
            None => eprintln!(
                "Лицензия: {} (локальный поиск без лимитов)",
                lst.tier.as_str()
            ),
        }
        return match poler_engine::google::api::status() {
            Ok(st) => {
                println!("{}", serde_json::to_string_pretty(&st).unwrap_or_default());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("poler-engine google-status: {e}");
                ExitCode::from(1)
            }
        };
    }

    if let Some(query) = cli.google_gmail.clone() {
        if !poler_engine::license::gate_or_print(poler_engine::license::FEATURE_GMAIL) {
            return ExitCode::from(2);
        }
        return match poler_engine::google::api::gmail_search(&query, cli.google_max.max(1)) {
            Ok(hits) => {
                if cli.verbose {
                    eprintln!(
                        "poler-engine gmail: «{query}» — {} писем",
                        hits.len()
                    );
                }
                print_mail_hits(&hits, &query, cli.format);
                if hits.is_empty() {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                eprintln!("poler-engine gmail: {e}");
                ExitCode::from(2)
            }
        };
    }

    if let Some(query) = cli.google_drive.clone() {
        if !poler_engine::license::gate_or_print(poler_engine::license::FEATURE_DRIVE) {
            return ExitCode::from(2);
        }
        return match poler_engine::google::api::drive_list(&query, cli.google_max.max(1)) {
            Ok(hits) => {
                if cli.verbose {
                    eprintln!(
                        "poler-engine drive: «{query}» — {} файлов",
                        hits.len()
                    );
                }
                print_drive_hits(&hits, &query, cli.format);
                if hits.is_empty() {
                    ExitCode::from(1)
                } else {
                    ExitCode::SUCCESS
                }
            }
            Err(e) => {
                eprintln!("poler-engine drive: {e}");
                ExitCode::from(2)
            }
        };
    }

    // ---------- NotebookLM: RPC-протокол NLMTools без пароля (v0.13.0) ----------
    if cli.nlm_notebooks
        || cli.nlm_source.is_some()
        || cli.nlm_notes.is_some()
        || cli.nlm_artifacts.is_some()
        || cli.nlm_account
        || cli.nlm_chat.is_some()
        || cli.nlm_media.is_some()
        || cli.nlm_shot.is_some()
        || cli.nlm_sync.is_some()
    {
        return run_nlm(&cli);
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
