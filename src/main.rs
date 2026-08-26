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
    #[arg(long = "web-search", conflicts_with_all = ["web", "crawl"])]
    web_search: Option<String>,

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

    /// MCP-СЕРВЕР (Model Context Protocol): poler-engine как нативный
    /// инструмент LLM-агентов поверх stdio JSON-RPC.
    /// Инструменты: poler_web_search / poler_crawl / poler_fetch / poler_search.
    #[arg(long = "mcp", conflicts_with_all = ["web_search", "crawl", "web_stats", "impact"])]
    mcp: bool,

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

/// Вывод результатов веб-поиска в трёх форматах.
fn print_web_hits(hits: &[poler_engine::web::WebHit], query: &str, format: Format) {
    use std::io::Write;
    match format {
        Format::AiJson => {
            let out = serde_json::json!({
                "engine": "poler-engine",
                "mode": "web-search",
                "rank": "POLER WebRank v1 (0.55·BM25 + 0.15·PageRank + 0.20·title + 0.10·ε-density)",
                "query": query,
                "total": hits.len(),
                "results": hits,
            });
            println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
        }
        Format::Simple => {
            for h in hits {
                println!("{:.4}  {}  {}", h.score, h.url, h.title);
            }
        }
        Format::Md => {
            println!("# Веб-поиск: «{query}»\n");
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

fn main() -> ExitCode {
    let cli = Cli::parse();

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
        let hits = match ix.search(&query, cli.top.max(1)) {
            Ok(h) => h,
            Err(e) => {
                eprintln!("poler-engine: web-search: {e}");
                return ExitCode::from(2);
            }
        };
        if cli.verbose {
            eprintln!(
                "poler-engine web-search: «{query}» — {} результатов из {} страниц",
                hits.len(),
                ix.page_count()
            );
        }
        print_web_hits(&hits, &query, cli.format);
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
        let mut fetcher = match poler_engine::web::cdp_fetcher(cli.cdp_port, cli.web_wait_ms) {
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
            "poler-crawl: готово — {} загружено, {} проиндексировано, {} дубликатов, {} мс",
            stats.fetched, stats.indexed, stats.duplicates, stats.elapsed_ms
        );
        return ExitCode::SUCCESS;
    }

    let Some(path_arg) = cli.path.clone() else {
        eprintln!("poler-engine: укажите PATH, --web-search <QUERY> или --crawl с seed URL");
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
