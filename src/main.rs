//! CLI poler-engine: AI-Native Topographical, Resonant and Graph Search Engine.
//!
//! Коды выхода (grep-совместимые): 0 — есть совпадения, 1 — совпадений нет,
//! 2 — ошибка (путь не найден, пустой запрос).

use clap::{Parser, ValueEnum};
use std::path::PathBuf;
use std::process::ExitCode;

use poler_engine::{
    render_markdown, render_simple, scan_path_with_stats, EngineConfig, PiiMode, ResonanceMode,
    DEFAULT_EXTENSIONS,
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
    /// IIR-резонанс по последовательности совпадений.
    Hits,
    /// Поле резонанса по всему документу, строго O(N).
    Field,
}

#[derive(Parser, Debug)]
#[command(
    name = "poler-engine",
    version,
    author = "POLER Engineering Core",
    about = "AI-Native Topographical, Resonant and Graph Search Engine",
    long_about = "Поисково-аналитический движок для LLM-агентов: возвращает полный логический \
                  скоуп (сцена/функция целиком), информационную плотность ε, резонанс R(t) и \
                  K-hop подграф связей сущностей вместо изолированных строк grep."
)]
struct Cli {
    /// Путь к файлу или корню репозитория.
    path: PathBuf,

    /// Поисковый запрос: слово или фраза (в кавычках).
    #[arg(short, long)]
    query: String,

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

    /// ε по статистикам файла вместо корпуса.
    #[arg(long, default_value_t = false)]
    local_stats: bool,

    /// Сканируемые расширения (через запятую).
    #[arg(long, default_value = DEFAULT_EXTENSIONS)]
    extensions: String,

    /// Пропускать файлы больше N мегабайт.
    #[arg(long = "max-file-size", default_value_t = 32)]
    max_file_mb: u64,

    /// Максимум байт enclosing_scope в якоре.
    #[arg(long = "max-scope", default_value_t = 16384)]
    max_scope: usize,

    /// Максимум K-hop отношений на якорь.
    #[arg(long = "max-relations", default_value_t = 64)]
    max_relations: usize,

    /// Число потоков rayon (по умолчанию — все ядра).
    #[arg(long)]
    threads: Option<usize>,

    /// Подробная статистика прогона в stderr.
    #[arg(short = 'v', long)]
    verbose: bool,
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

    if !cli.path.exists() {
        eprintln!("poler-engine: путь не найден: {}", cli.path.display());
        return ExitCode::from(2);
    }
    if cli.query.trim().is_empty() {
        eprintln!("poler-engine: пустой запрос");
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
    };

    let (result, stats) = scan_path_with_stats(&cli.path, &cli.query, &config);

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

    match cli.format {
        Format::AiJson => {
            println!("{}", serde_json::to_string_pretty(&result).unwrap_or_default());
        }
        Format::Md => print!("{}", render_markdown(&result)),
        Format::Simple => print!("{}", render_simple(&result)),
    }

    if result.total_hits == 0 {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}
