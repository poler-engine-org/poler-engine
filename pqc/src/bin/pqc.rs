//! CLI `pqc`: Born-сэмплирование поверх контейнера `.poler` / `.pqw` —
//! полностью автономный бинарник (mmap zero-copy, без Python и Qiskit).
//!
//! ```text
//! pqc run <file> [--shots N] [--seed S] [--top K] [--purify N]
//!            [--engine auto|sv|product] [--max-sv-qubits N] [--verify]
//!            [--marginals]
//! pqc demo [--n N] [--shots M] [--seed S]
//! pqc stream (--url URL | --file PATH | --text TEXT | --stdin)
//!            [--dim N] [--epsilon E] [--shots N] [--steps N] [--seed S]
//!            [--eta0 H] [--beta B] [--gamma G] [--decay] [--json] [--out F]
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
    pqc stream (--url URL | --file PATH | --text TEXT | --stdin) [options]
    pqc unfurl <file> [--threshold T]                         AOT phase unfurling to syntax

STREAM OPTIONS (RQ6: zero-storage потоковое обучение):
    --url <URL>               http:// страница (zero-dep клиент; https → --file)
    --file <PATH>             локальный HTML/текст файл
    --text <TEXT>             встроенный текст чанка
    --stdin                   читать стандартный ввод до EOF
    --dim <N>                 размерность d_pol (default 512)
    --epsilon <E>             порог LENS ε-плотности (default 0.2)
    --shots <N>               Born-выстрелов на измерение (default 10000)
    --steps <N>               шагов Active Inference к цели (default 8)
    --seed <S>                семя xoshiro256++ (default 42)
    --eta0 <H>                базовый шаг η₀ (default 0.25)
    --beta <B>                затухание шага по сюрпризу β (default 1.0)
    --gamma <G>               трение γ (default 0.5, полюсная защита RQ5)
    --decay                   политика фона Decay (по умолчанию Hold)
    --json                    машинно-читаемый отчёт (zero-dep JSON)
    --out <F>                 дамп Packed4-контейнера в файл (опционально)

OPTIONS (run/demo):
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
        Some("stream") => cmd_stream(&args[1..]),
        Some("unfurl") => cmd_unfurl(&args[1..]),
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

// ============================================================================
// pqc stream (RQ6): Zero-Storage Streaming Learning Engine
// ============================================================================

struct StreamConfig {
    url: Option<String>,
    file: Option<String>,
    text: Option<String>,
    stdin: bool,
    dim: u32,
    epsilon: f32,
    shots: u64,
    steps: usize,
    seed: u64,
    eta0: f64,
    beta: f64,
    gamma: f64,
    decay: bool,
    json: bool,
    out: Option<String>,
}

impl Default for StreamConfig {
    fn default() -> Self {
        StreamConfig {
            url: None,
            file: None,
            text: None,
            stdin: false,
            dim: 512,
            epsilon: 0.2,
            shots: 10_000,
            steps: 8,
            seed: 42,
            eta0: 0.25,
            beta: 1.0,
            gamma: 0.5,
            decay: false,
            json: false,
            out: None,
        }
    }
}

fn cmd_stream(args: &[String]) -> i32 {
    let mut cfg = StreamConfig::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        let mut val = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("missing value for {name}"))
        };
        match a.as_str() {
            "--url" => match val("--url") {
                Ok(v) => cfg.url = Some(v),
                Err(e) => return stream_usage_err(&e),
            },
            "--file" => match val("--file") {
                Ok(v) => cfg.file = Some(v),
                Err(e) => return stream_usage_err(&e),
            },
            "--text" => match val("--text") {
                Ok(v) => cfg.text = Some(v),
                Err(e) => return stream_usage_err(&e),
            },
            "--stdin" => cfg.stdin = true,
            "--dim" => match val("--dim").and_then(|v| parse_num::<u32>(&v, "--dim")) {
                Ok(v) => cfg.dim = v,
                Err(e) => return stream_usage_err(&e),
            },
            "--epsilon" => match val("--epsilon")
                .and_then(|v| v.parse::<f32>().map_err(|_| "bad --epsilon".to_string()))
            {
                Ok(v) => cfg.epsilon = v,
                Err(_) => return stream_usage_err("bad --epsilon"),
            },
            "--shots" => match val("--shots").and_then(|v| parse_num::<u64>(&v, "--shots")) {
                Ok(v) => cfg.shots = v,
                Err(e) => return stream_usage_err(&e),
            },
            "--steps" => match val("--steps").and_then(|v| parse_num::<usize>(&v, "--steps")) {
                Ok(v) => cfg.steps = v,
                Err(e) => return stream_usage_err(&e),
            },
            "--seed" => match val("--seed").and_then(|v| parse_num::<u64>(&v, "--seed")) {
                Ok(v) => cfg.seed = v,
                Err(e) => return stream_usage_err(&e),
            },
            "--eta0" => match val("--eta0")
                .and_then(|v| v.parse::<f64>().map_err(|_| "bad --eta0".to_string()))
            {
                Ok(v) => cfg.eta0 = v,
                Err(_) => return stream_usage_err("bad --eta0"),
            },
            "--beta" => match val("--beta")
                .and_then(|v| v.parse::<f64>().map_err(|_| "bad --beta".to_string()))
            {
                Ok(v) => cfg.beta = v,
                Err(_) => return stream_usage_err("bad --beta"),
            },
            "--gamma" => match val("--gamma")
                .and_then(|v| v.parse::<f64>().map_err(|_| "bad --gamma".to_string()))
            {
                Ok(v) => cfg.gamma = v,
                Err(_) => return stream_usage_err("bad --gamma"),
            },
            "--decay" => cfg.decay = true,
            "--json" => cfg.json = true,
            "--out" => match val("--out") {
                Ok(v) => cfg.out = Some(v),
                Err(e) => return stream_usage_err(&e),
            },
            other => return stream_usage_err(&format!("unknown option {other}")),
        }
        i += 1;
    }

    // Ровно один источник входа.
    let sources = cfg.url.is_some() as u8
        + cfg.file.is_some() as u8
        + cfg.text.is_some() as u8
        + cfg.stdin as u8;
    if sources != 1 {
        return stream_usage_err("укажите ровно один источник: --url | --file | --text | --stdin");
    }
    if cfg.dim == 0 || cfg.dim > 1 << 20 {
        return stream_usage_err("--dim должен быть в [1, 1048576]");
    }
    if !(0.0..=1.0).contains(&cfg.epsilon) || !cfg.epsilon.is_finite() {
        return stream_usage_err("--epsilon должен быть в [0, 1]");
    }

    // Входные байты.
    let (bytes, source_desc, source_kind) = if let Some(url) = &cfg.url {
        match http_get(url, MAX_HTTP_BYTES) {
            Ok((body, final_url)) => (body, final_url, "http".to_string()),
            Err(e) => {
                eprintln!("pqc stream: {e}");
                return 1;
            }
        }
    } else if let Some(path) = &cfg.file {
        match std::fs::read(path) {
            Ok(b) => (b, path.clone(), "file".to_string()),
            Err(e) => {
                eprintln!("pqc stream: {e}");
                return 1;
            }
        }
    } else if let Some(text) = &cfg.text {
        (
            text.clone().into_bytes(),
            "--text".to_string(),
            "text".to_string(),
        )
    } else {
        use std::io::Read;
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
            eprintln!("pqc stream: stdin: {e}");
            return 1;
        }
        (buf, "stdin".to_string(), "stdin".to_string())
    };

    // Движок: zero-storage цикл целиком в RAM.
    use pqc::stream_engine::{Forget, StreamEngine};
    let forget = if cfg.decay {
        Forget::Decay
    } else {
        Forget::Hold
    };
    let mut engine = match StreamEngine::new(cfg.dim, cfg.epsilon, cfg.seed) {
        Ok(e) => e
            .with_shots(cfg.shots)
            .with_hyper(cfg.eta0, cfg.beta, cfg.gamma)
            .with_forget(forget),
        Err(e) => {
            eprintln!("pqc stream: {e}");
            return 1;
        }
    };
    let rep = match engine.ingest_html(&bytes, cfg.steps) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc stream: {e}");
            return 1;
        }
    };

    // Опциональный дамп контейнера (единственная точка касания диска).
    if let Some(out) = &cfg.out {
        if let Err(e) = std::fs::write(out, engine.container()) {
            eprintln!("pqc stream: --out: {e}");
            return 1;
        }
    }

    if cfg.json {
        print_stream_json(&rep, &source_desc, &source_kind, bytes.len(), cfg.dim);
    } else {
        print_stream_human(&rep, &source_desc, &source_kind, bytes.len(), &engine);
    }
    0
}

fn stream_usage_err(msg: &str) -> i32 {
    eprintln!("pqc stream: {msg}\n\n{USAGE}");
    2
}

fn print_stream_human(
    rep: &pqc::stream_engine::StreamChunkReport,
    source_desc: &str,
    source_kind: &str,
    input_bytes: usize,
    engine: &pqc::stream_engine::StreamEngine,
) {
    println!("POLER Quantum Core — Zero-Storage Streaming Engine (RQ6)");
    println!(
        "source    : {source_desc} ({:.1} КиБ, {source_kind})",
        input_bytes as f64 / 1024.0
    );
    println!(
        "text      : {} токенов, LENS eps={:.2}",
        rep.tokens,
        engine.epsilon()
    );
    if rep.no_hits {
        println!("barrier   : NO_HITS — свидетельство пусто, детерминированный отказ");
        println!("fock      : [F, DM]_S = 0 точно (нет факта - нет галлюцинации)");
    } else {
        println!(
            "container : d_pol={} nnz={} {} B (Packed4 v0.2: 4 трита/байт, magic POLER_Q2)",
            engine.d_pol(),
            rep.nnz,
            rep.container_bytes
        );
        println!(
            "buffer    : capacity {} B (реюз, системных аллокаций после прогрева нет)",
            rep.buffer_capacity
        );
        println!(
            "fock      : raw={:.4} normalized={:.4} (коммутатор [F, DM]_S свидетельство x память)",
            rep.fock.raw, rep.fock.normalized
        );
        if let Some(s) = &rep.step {
            println!(
                "step 1    : surprise Sigma={:.4}  eta={:.4}  (eta0*e^(-beta*Sigma))",
                s.surprise, s.eta
            );
        }
        println!(
            "loss      : {:.6} ({} шагов Active Inference, gamma={})",
            rep.param_loss,
            rep.steps_run,
            engine.learner().gamma()
        );
        println!(
            "qcm       : theory {:.4}  observed {:.4}  gap {:.4}  ({} выстрелов)",
            rep.qcm.qcm_theory,
            rep.qcm.qcm_observed,
            rep.qcm.qcm_gap(),
            rep.qcm.shots
        );
    }
    println!(
        "elapsed   : {:.3} мс (сквозной цикл в RAM, диск не затронут)",
        rep.elapsed.as_secs_f64() * 1000.0
    );
}

fn print_stream_json(
    rep: &pqc::stream_engine::StreamChunkReport,
    source_desc: &str,
    source_kind: &str,
    input_bytes: usize,
    dim: u32,
) {
    use pqc::json::Json;
    let step_obj = |s: &pqc::learn::ActiveStepReport| {
        Json::Obj(vec![
            ("step".into(), Json::num(s.step as f64)),
            ("shots".into(), Json::num(s.shots as f64)),
            ("surprise".into(), Json::num(s.surprise)),
            ("eta".into(), Json::num(s.eta)),
            ("surrogate_loss".into(), Json::num(s.surrogate_loss)),
            ("grad_norm".into(), Json::num(s.grad_norm)),
            ("max_dp".into(), Json::num(s.max_dp)),
            ("measurement_mad".into(), Json::num(s.measurement_mad)),
            ("purified".into(), Json::Bool(s.purified)),
            ("mean_abs_p".into(), Json::num(s.mean_abs_p)),
        ])
    };
    let obj = Json::Obj(vec![
        ("source".into(), Json::str(source_desc)),
        ("source_kind".into(), Json::str(source_kind)),
        ("input_bytes".into(), Json::num(input_bytes as f64)),
        ("d_pol".into(), Json::num(dim as f64)),
        ("tokens".into(), Json::num(rep.tokens as f64)),
        ("nnz".into(), Json::num(rep.nnz as f64)),
        (
            "container_bytes".into(),
            Json::num(rep.container_bytes as f64),
        ),
        (
            "buffer_capacity".into(),
            Json::num(rep.buffer_capacity as f64),
        ),
        ("docs_seen".into(), Json::num(rep.docs_seen as f64)),
        ("no_hits".into(), Json::Bool(rep.no_hits)),
        (
            "fock".into(),
            Json::Obj(vec![
                ("raw".into(), Json::num(rep.fock.raw)),
                ("normalized".into(), Json::num(rep.fock.normalized)),
            ]),
        ),
        (
            "step".into(),
            match &rep.step {
                Some(s) => step_obj(s),
                None => Json::Null,
            },
        ),
        ("steps_run".into(), Json::num(rep.steps_run as f64)),
        ("param_loss".into(), Json::num(rep.param_loss)),
        (
            "qcm".into(),
            Json::Obj(vec![
                ("theory".into(), Json::num(rep.qcm.qcm_theory)),
                ("observed".into(), Json::num(rep.qcm.qcm_observed)),
                (
                    "born_entropy_bits".into(),
                    Json::num(rep.qcm.born_entropy_bits),
                ),
                ("marginal_mad".into(), Json::num(rep.qcm.marginal_mad)),
            ]),
        ),
        (
            "elapsed_ms".into(),
            Json::num(rep.elapsed.as_secs_f64() * 1000.0),
        ),
    ]);
    println!("{}", obj.to_string());
}

/// Потолок тела HTTP-ответа (защита от бесконечных потоков).
const MAX_HTTP_BYTES: usize = 16 * 1024 * 1024;
/// Лимит переходов по редиректам.
const MAX_REDIRECTS: usize = 3;

/// Минимальный HTTP/1.1 GET-клиент на `std::net::TcpStream` —
/// **ноль внешних зависимостей** (TLS сознательно не поддерживается:
/// https-страницу следует сохранить и передать через `--file`).
fn http_get(url: &str, max_bytes: usize) -> Result<(Vec<u8>, String), String> {
    let mut current = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let (host, port, path) = parse_http_url(&current)?;
        let stream = std::net::TcpStream::connect((host.as_str(), port))
            .map_err(|e| format!("connect {host}:{port}: {e}"))?;
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(15)))
            .map_err(|e| e.to_string())?;
        stream
            .set_write_timeout(Some(std::time::Duration::from_secs(15)))
            .map_err(|e| e.to_string())?;
        let mut stream = stream;
        let host_header = if port == 80 {
            host.clone()
        } else {
            format!("{host}:{port}")
        };
        let req = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {host_header}\r\n\
             User-Agent: pqc-stream/0.3\r\n\
             Accept: text/html,application/xhtml+xml,text/plain;q=0.9,*/*;q=0.8\r\n\
             Accept-Encoding: identity\r\n\
             Connection: close\r\n\r\n"
        );
        use std::io::{Read, Write};
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("send: {e}"))?;

        // Ответ целиком (Connection: close упрощает границы тела).
        let mut raw = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        loop {
            let n = stream.read(&mut chunk).map_err(|e| format!("recv: {e}"))?;
            if n == 0 {
                break;
            }
            if raw.len() + n > max_bytes + 64 * 1024 {
                return Err(format!("ответ превышает лимит {max_bytes} байт"));
            }
            raw.extend_from_slice(&chunk[..n]);
        }

        // Заголовки / тело.
        let sep = find_headers_end(&raw).ok_or("ответ без завершения заголовков")?;
        let head = String::from_utf8_lossy(&raw[..sep]).to_string();
        let body = raw[sep + 4..].to_vec();
        let mut lines = head.split("\r\n");
        let status_line = lines.next().unwrap_or_default().to_string();
        let code: u16 = status_line
            .split_ascii_whitespace()
            .nth(1)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| format!("битый статус: {status_line}"))?;

        let mut location = None;
        let mut chunked = false;
        for line in lines {
            let Some((k, v)) = line.split_once(':') else {
                continue;
            };
            let k = k.trim().to_ascii_lowercase();
            let v = v.trim();
            if k == "location" {
                location = Some(v.to_string());
            }
            if k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked") {
                chunked = true;
            }
        }

        match code {
            200..=299 => {
                let body = if chunked {
                    decode_chunked(&body, max_bytes)?
                } else {
                    body
                };
                return Ok((body, current));
            }
            301 | 302 | 303 | 307 | 308 => {
                let loc = location.ok_or_else(|| format!("редирект {code} без Location"))?;
                current = resolve_url(&current, &loc)?;
                continue;
            }
            _ => return Err(format!("HTTP {code}: {status_line}")),
        }
    }
    Err("слишком много редиректов".into())
}

/// `http://host[:port]/path?query` → (host, port, path).
fn parse_http_url(url: &str) -> Result<(String, u16, String), String> {
    if url.starts_with("https://") {
        return Err(format!(
            "https не поддерживается zero-dep клиентом: сохраните страницу и передайте --file ({url})"
        ));
    }
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| format!("неподдерживаемая схема (нужен http://): {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    if authority.is_empty() {
        return Err("пустой хост".into());
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>().map_err(|_| format!("битый порт: {p}"))?,
        ),
        None => (authority.to_string(), 80),
    };
    Ok((host, port, path.to_string()))
}

/// Индекс `\r\n\r\n` (конец заголовков).
fn find_headers_end(raw: &[u8]) -> Option<usize> {
    raw.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Декодирование Transfer-Encoding: chunked.
fn decode_chunked(body: &[u8], max_bytes: usize) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut i = 0usize;
    loop {
        let Some(nl) = body[i..].windows(2).position(|w| w == b"\r\n") else {
            return Err("битый chunked: нет конца size-строки".into());
        };
        let size_line = String::from_utf8_lossy(&body[i..i + nl]).to_string();
        let size_hex = size_line.split(';').next().unwrap_or_default().trim();
        let size = usize::from_str_radix(size_hex, 16)
            .map_err(|_| format!("битый chunked размер: {size_hex}"))?;
        i += nl + 2;
        if size == 0 {
            break;
        }
        if out.len() + size > max_bytes {
            return Err(format!("chunked тело превышает лимит {max_bytes} байт"));
        }
        if i + size > body.len() {
            return Err("битый chunked: обрыв данных".into());
        }
        out.extend_from_slice(&body[i..i + size]);
        i += size + 2; // данные + CRLF
    }
    Ok(out)
}

/// Относительный Location → абсолютный URL.
fn resolve_url(base: &str, location: &str) -> Result<String, String> {
    if location.starts_with("http://") || location.starts_with("https://") {
        if location.starts_with("https://") {
            return Err("редирект на https не поддерживается zero-dep клиентом".into());
        }
        return Ok(location.to_string());
    }
    let (host, port, _) = parse_http_url(base)?;
    let path = if location.starts_with('/') {
        location.to_string()
    } else {
        // Относительный путь от корня (упрощение: без разбора '..').
        format!("/{location}")
    };
    Ok(format!("http://{host}:{port}{path}"))
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

fn cmd_unfurl(args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("pqc unfurl: требуется путь к файлу .poler / .pqw");
        return 2;
    }
    let file_path = &args[0];
    let raw = match std::fs::read(file_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pqc unfurl: ошибка чтения {file_path}: {e}");
            return 1;
        }
    };
    let reader = match PqwReader::from_bytes(&raw) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc unfurl: ошибка парсинга {file_path}: {e}");
            return 1;
        }
    };

    println!("POLER Quantum Core — AOT Syntax Unfolder");
    println!("source    : {} ({} B, d_pol={})", file_path, raw.len(), reader.d_pol());

    let mut ps = vec![0.0f64; reader.d_pol() as usize];
    if reader.header().is_packed() {
        if let Ok(iter) = reader.iter_packed_trits() {
            for (_i, trit) in iter.enumerate() {
                ps[trit.0 as usize] = match trit.1 {
                    pqw::Trit::Pos => 1.0,
                    pqw::Trit::Neg => -1.0,
                    pqw::Trit::Zero => 0.0,
                };
            }
        }
    }

    let mut out_buf = [0u8; pqc::syntax_unfolder::MAX_OUT];
    let written = pqc::syntax_unfolder::unfurl(&ps, &mut out_buf);

    if written > 0 {
        let s = std::str::from_utf8(&out_buf[..written]).unwrap_or("<non-utf8 bytes>");
        println!("unfurled  : '{}' ({} bytes, zero heap alloc)", s, written);
    } else {
        println!("unfurled  : <zero / background state> (0 bytes)");
    }

    0
}
