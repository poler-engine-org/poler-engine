//! CLI `pqc`: Born-сэмплирование поверх контейнера `.poler` / `.pqw` —
//! полностью автономный бинарник (mmap zero-copy, без Python и Qiskit).
//!
//! ```text
//! pqc run <file> [--shots N] [--seed S] [--top K] [--purify N]
//!            [--engine auto|sv|product] [--max-sv-qubits N] [--verify]
//!            [--marginals]
//! pqc demo [--n N] [--shots M] [--seed S]
//! ```

use std::path::Path;
use std::process::exit;

use pqc::{Ansatz, LoadOptions, Rng, DEFAULT_MAX_SV_QUBITS, MAX_QUBITS};
use pqw::{PqwReader, PqwWriter};

const USAGE: &str = "\
POLER Quantum Core — statevector + Born sampling над .poler/.pqw

USAGE:
    pqc run <file> [options]
    pqc demo [--n N] [--shots M] [--seed S]

OPTIONS:
    --shots <N>               Born-выстрелов (default 1024)
    --seed <S>                семя xoshiro256++ (default 42)
    --top <K>                 топ-K исходов в отчёте (default 8)
    --purify <N>              шаги McWeeny перед кодированием (default 0)
    --engine <auto|sv|product>  выбор движка (default auto)
    --max-sv-qubits <N>       порог statevector-движка (default 20)
    --verify                  проверить SHA-256 payload перед запуском
    --marginals               вывести все маргиналы (по умолчанию первые 20)";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("run") => cmd_run(&args[1..]),
        Some("demo") => cmd_demo(&args[1..]),
        _ => {
            eprintln!("{USAGE}");
            2
        }
    };
    exit(code);
}

// --- Источник байтов: zero-copy mmap на unix ---

enum Source {
    #[cfg(unix)]
    Map(pqw::Mmap),
    #[allow(dead_code)] // не используется на unix, но нужен для переносимости
    Mem(Vec<u8>),
}

impl Source {
    fn as_slice(&self) -> &[u8] {
        match self {
            #[cfg(unix)]
            Source::Map(m) => m.as_slice(),
            Source::Mem(v) => v,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            #[cfg(unix)]
            Source::Map(_) => "mmap",
            Source::Mem(_) => "mem",
        }
    }
}

fn load(path: &str) -> Result<Source, String> {
    #[cfg(unix)]
    return pqw::Mmap::open(Path::new(path))
        .map(Source::Map)
        .map_err(|e| e.to_string());
    #[cfg(not(unix))]
    return std::fs::read(path)
        .map(Source::Mem)
        .map_err(|e| e.to_string());
}

// --- Разбор аргументов ---

fn take_value(args: &[String], i: &mut usize, name: &str) -> Result<String, String> {
    *i += 1;
    args.get(*i)
        .cloned()
        .ok_or_else(|| format!("missing value for {name}"))
}

fn parse_num<T: std::str::FromStr>(s: &str, name: &str) -> Result<T, String> {
    s.parse().map_err(|_| format!("bad value for {name}: {s}"))
}

#[derive(Clone, Copy, PartialEq)]
enum EngineChoice {
    Auto,
    Sv,
    Product,
}

struct RunConfig {
    shots: u64,
    seed: u64,
    top: usize,
    purify: usize,
    engine: EngineChoice,
    max_sv_qubits: usize,
    verify: bool,
    marginals_all: bool,
}

impl Default for RunConfig {
    fn default() -> Self {
        RunConfig {
            shots: 1024,
            seed: 42,
            top: 8,
            purify: 0,
            engine: EngineChoice::Auto,
            max_sv_qubits: DEFAULT_MAX_SV_QUBITS,
            verify: false,
            marginals_all: false,
        }
    }
}

/// Разбор общих опций; `file` заполняется первым позиционным аргументом.
fn parse_common(
    args: &[String],
    cfg: &mut RunConfig,
    file: &mut Option<String>,
) -> Result<(), String> {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        match a.as_str() {
            "--shots" => cfg.shots = parse_num(&take_value(args, &mut i, "--shots")?, "--shots")?,
            "--seed" => cfg.seed = parse_num(&take_value(args, &mut i, "--seed")?, "--seed")?,
            "--top" => cfg.top = parse_num(&take_value(args, &mut i, "--top")?, "--top")?,
            "--purify" => {
                cfg.purify = parse_num(&take_value(args, &mut i, "--purify")?, "--purify")?
            }
            "--max-sv-qubits" => {
                cfg.max_sv_qubits = parse_num(
                    &take_value(args, &mut i, "--max-sv-qubits")?,
                    "--max-sv-qubits",
                )?
            }
            "--verify" => cfg.verify = true,
            "--marginals" => cfg.marginals_all = true,
            "--engine" => {
                let v = take_value(args, &mut i, "--engine")?;
                cfg.engine = match v.as_str() {
                    "auto" => EngineChoice::Auto,
                    "sv" | "statevector" => EngineChoice::Sv,
                    "product" => EngineChoice::Product,
                    other => return Err(format!("bad engine: {other} (auto|sv|product)")),
                };
            }
            other if !other.starts_with("--") => {
                if file.replace(other.to_string()).is_some() {
                    return Err("file given twice".into());
                }
            }
            other => return Err(format!("unknown option {other}")),
        }
        i += 1;
    }
    Ok(())
}

// --- Общий конвейер отчёта ---

fn run_reader(
    source_desc: &str,
    source_kind: &str,
    size: usize,
    reader: &PqwReader,
    cfg: &RunConfig,
) -> Result<(), String> {
    let hyper = reader.hyperparams();
    println!("POLER Quantum Core — Born sampling");
    println!("source    : {source_desc} ({size} B, {source_kind})");
    println!(
        "container : d_pol={} nnz={} eps={:.3} mcweeny_residual={:.3e}",
        reader.d_pol(),
        reader.nnz(),
        hyper.epsilon_threshold,
        reader.mcweeny_residual()
    );
    println!(
        "hyper     : eta={} gamma={} rho={}",
        hyper.eta, hyper.gamma, hyper.rho
    );

    let mut opts = LoadOptions {
        purify_steps: cfg.purify,
        max_sv_qubits: cfg.max_sv_qubits,
        verify_payload: cfg.verify,
    };
    match cfg.engine {
        EngineChoice::Auto => {}
        EngineChoice::Sv => {
            if reader.d_pol() as usize > MAX_QUBITS {
                return Err(format!(
                    "sv engine: d_pol={} exceeds hard limit {} (use --engine product)",
                    reader.d_pol(),
                    MAX_QUBITS
                ));
            }
            opts.max_sv_qubits = MAX_QUBITS;
        }
        EngineChoice::Product => opts.max_sv_qubits = 0,
    }

    let ansatz = Ansatz::from_reader(reader, &opts).map_err(|e| e.to_string())?;
    match &ansatz {
        Ansatz::Statevector(sv) => println!(
            "engine    : statevector ({} qubits, {} amplitudes)",
            sv.n_qubits(),
            sv.dim()
        ),
        Ansatz::Product(pa) => println!(
            "engine    : product (d_pol={}, nnz={})",
            pa.d_pol(),
            pa.nnz()
        ),
    }
    if cfg.purify > 0 {
        println!("purify    : {} McWeeny steps", cfg.purify);
    }

    let mut rng = Rng::seed_from_u64(cfg.seed);
    let report = ansatz
        .sample(&mut rng, cfg.shots, cfg.top)
        .map_err(|e| e.to_string())?;
    println!(
        "shots     : {}  seed: {}  distinct: {}",
        report.shots, cfg.seed, report.distinct
    );

    if report.top.is_empty() {
        println!("top       : (nnz > 64 — паттерны дуг не помещаются в u64)");
    } else {
        println!("top-{} исходов:", report.top.len());
        let bits = match &ansatz {
            Ansatz::Statevector(sv) => Some(sv.n_qubits()),
            Ansatz::Product(_) => None,
        };
        for (k, ((outcome, count), theory)) in report.top.iter().zip(&report.top_probs).enumerate()
        {
            match bits {
                Some(n) => println!(
                    "  #{k}  |{:0width$b}⟩  count {count:>6}  p̂={:.4}  P={:.4}",
                    outcome,
                    p_hat(*count, report.shots),
                    theory,
                    width = n
                ),
                None => println!(
                    "  #{k}  arcs 0x{outcome:016X}  count {count:>6}  p̂={:.4}  P={:.4}",
                    p_hat(*count, report.shots),
                    theory
                ),
            }
        }
    }

    let limit = if cfg.marginals_all {
        report.marginals.len()
    } else {
        report.marginals.len().min(20)
    };
    println!("marginals P(b=1):");
    for (idx, theory, obs) in report.marginals.iter().take(limit) {
        println!("  arc {idx:>6}: theory {theory:.4}  observed {obs:.4}");
    }
    if limit < report.marginals.len() {
        println!(
            "  ... ещё {} (весь список: --marginals)",
            report.marginals.len() - limit
        );
    }

    // Теоретическая дисперсия веса: независимые биты продукта.
    let var_theory: f64 = match &ansatz {
        Ansatz::Statevector(_) => report
            .marginals
            .iter()
            .map(|&(_, t, _)| t * (1.0 - t))
            .sum(),
        Ansatz::Product(pa) => {
            let background = f64::from(pa.d_pol()) - pa.nnz() as f64;
            background * 0.25
                + pa.arcs()
                    .iter()
                    .map(|&(_, p)| {
                        let m = 0.5 * (1.0 - p);
                        m * (1.0 - m)
                    })
                    .sum::<f64>()
        }
    };
    println!(
        "hamming weight: mean {:.2} (theory {:.2})  var {:.2} (theory {:.2})",
        report.weight_mean, report.expected_weight, report.weight_var, var_theory
    );
    Ok(())
}

fn p_hat(count: u64, shots: u64) -> f64 {
    count as f64 / shots.max(1) as f64
}

// --- Команды ---

fn cmd_run(args: &[String]) -> i32 {
    let mut cfg = RunConfig::default();
    let mut file = None;
    if let Err(e) = parse_common(args, &mut cfg, &mut file) {
        eprintln!("pqc run: {e}\n\n{USAGE}");
        return 2;
    }
    let Some(file) = file else {
        eprintln!("pqc run: file required\n\n{USAGE}");
        return 2;
    };
    let src = match load(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pqc run: {e}");
            return 1;
        }
    };
    let reader = match PqwReader::from_bytes(src.as_slice()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc run: {e}");
            return 1;
        }
    };
    match run_reader(&file, src.kind(), src.as_slice().len(), &reader, &cfg) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("pqc run: {e}");
            1
        }
    }
}

fn cmd_demo(args: &[String]) -> i32 {
    let mut cfg = RunConfig {
        shots: 4096,
        ..RunConfig::default()
    };
    let mut n: usize = 12;

    // --n — опция только demo: вырезаем её до общего разбора.
    let mut rest: Vec<String> = Vec::with_capacity(args.len());
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--n" {
            let Some(v) = args.get(i + 1) else {
                eprintln!("pqc demo: missing value for --n\n\n{USAGE}");
                return 2;
            };
            match v.parse::<usize>() {
                Ok(val) => n = val,
                Err(_) => {
                    eprintln!("pqc demo: bad --n: {v}");
                    return 2;
                }
            }
            i += 2;
        } else {
            rest.push(args[i].clone());
            i += 1;
        }
    }
    let mut file = None;
    if let Err(e) = parse_common(&rest, &mut cfg, &mut file) {
        eprintln!("pqc demo: {e}\n\n{USAGE}");
        return 2;
    }
    if let Some(f) = file {
        eprintln!("pqc demo: unexpected argument {f}");
        return 2;
    }
    if n == 0 || n > 1_000_000 {
        eprintln!("pqc demo: --n must be in [1, 1000000]");
        return 2;
    }

    // Детерминированный фазовый вектор: смешанный фон и спайки.
    let mut ps = vec![0.0_f32; n];
    for (i, p) in ps.iter_mut().enumerate() {
        let x = (i as u64)
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            .wrapping_add(0x1234_5678_9ABC_DEF0);
        let v = ((x >> 33) % 2001) as i64 - 1000;
        *p = v as f32 / 1000.0;
    }
    let mut w = match PqwWriter::new(n as u32) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("pqc demo: {e}");
            return 1;
        }
    };
    w = w.hyperparams(0.01, 0.1, 0.99, 0.1);
    if let Err(e) = w.add_state(&ps) {
        eprintln!("pqc demo: {e}");
        return 1;
    }
    let bytes = match w.to_bytes() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pqc demo: {e}");
            return 1;
        }
    };
    let reader = match PqwReader::from_bytes(&bytes) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc demo: {e}");
            return 1;
        }
    };
    match run_reader("demo (in-memory)", "mem", bytes.len(), &reader, &cfg) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("pqc demo: {e}");
            1
        }
    }
}
