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
//! pqc inspect <file> [--hex N|all] [--decode all|N] [--graph] [--dot F|-]
//!            [--matrix] [--strings] [--raw-dim N] [--json] [--all]
//! ```

use std::path::Path;
use std::process::exit;

use pqc::inspect::{
    arcs_csr_text, arcs_dot, ascii_matrix, ascii_strings, born_entropy, crypto_recon, detect_kind,
    graph_stats, header_rows, hex_dump, qcm_theory, raw_arcs, reader_arcs, report_json,
    try_pqw_reader, CryptoRecon, DecodedArc, FileKind, ARCS_PREVIEW, MATRIX_D_MAX,
};
use pqc::json::Json;
use pqc::{Ansatz, LoadOptions, Rng, DEFAULT_MAX_SV_QUBITS, MAX_QUBITS};
use pqc::bloch_stream;
use pqw::{PqwReader, PqwWriter};

const USAGE: &str = "\
POLER Quantum Core — statevector + Born sampling над .poler/.pqw

USAGE:
    pqc run <file> [options]
    pqc demo [--n N] [--shots M] [--seed S]
    pqc stream (--url URL | --file PATH | --text TEXT | --stdin) [options]
    pqc unfurl <file> [--threshold T]                         AOT phase unfurling to syntax
    pqc precess <file> [options]                              RQ11: петля архетипа поверх чекпоинта
    pqc bloch <file.pqw> [--head N] [--window W] [--json]    RQ14: триты → углы Блоха (mmap, θ на лету)
    pqc encrypt <IN> --key <ARCH.pqw> --out <C.pqt> [opts]    RQ13: триты GF(3), прецессия+лавина
    pqc decrypt <C.pqt> --key <ARCH.pqw> [--out <PLAIN>]      RQ13: m = p* ⊕ (a ⊗_ε p*) точно
    pqc inspect <file> [options]
    pqc train (--corpus DIR | --stdin) [options]              накопительное обучение

ENCRYPT/DECRYPT OPTIONS (RQ13: трит-схема GF(3) по умолчанию, файл 285;
                          --f32 — исследовательская схема RQ12):
    --key <F>                 контейнер-ключ .pqw v3 с гироскопом J
                              (precess --out / train --gyro)
    --out <F>                 encrypt: файл шифртекста .pqt (обязателен);
                              decrypt: файл открытого текста (без — stdout)
    --f32                     RQ12-схема на f32-фазах (×43, без лавины) —
                              для сравнения/воспроизведения RQ12
    --modes <N>               число мод проектора (default: авто-калибровка
                              по помехе проекции; 1..=8)
    --seed <S>                семя IV xoshiro256++ (default: энтропия)
    --text <T>                encrypt: сообщение из строки вместо файла
    --stdin                   encrypt: сообщение из stdin
    --json                    машинно-читаемый отчёт

INSPECT OPTIONS (чтение бинарников, графов и крипто-разведка):
    --hex <N|all>             hex-дамп первых N байтов (default 512)
    --no-hex                  без hex-дампа
    --decode <all|N>          декод дуг: все или первые N (default 64)
    --graph                   CSR-дамп дуг + статистика LENS-графа
    --dot <FILE|->            Graphviz DOT в файл ('-' — в stdout)
    --matrix                  ASCII-матрица смежности (d_pol <= 64)
    --strings                 печатаемые ASCII-строки (min 6)
    --raw-dim <N>             raw Packed4 с d_pol = N (<= 4 × размер)
    --json                    машинно-читаемый отчёт (zero-dep JSON)
    --all                     полный отчёт: весь hex, все дуги, граф,
                              матрица, строки

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

TRAIN OPTIONS (RQ8: плотный LENS-граф; RQ15: --quantized — решётка тритов):
    --corpus <DIR>            каталог корпуса (рекурсивно, текст. расширения)
    --stdin                   поток блоков из стандартного ввода
    --dim <N>                 размерность d_pol (default 4096)
    --epsilon <E>             порог LENS ε-плотности (default 0.05 — плотный)
    --block <N>               размер блока в байтах (default 8192)
    --shots <N>               Born-выстрелов на измерение (default 20000)
    --steps <N>               шагов Active Inference на блок (default 10)
    --out <F>                 чекпоинт .pqw накопленной памяти
    --fingerprint <F>         raw Packed4-отпечаток фазовой памяти (снимок CPU)
    --snapshot-every <N>      снапшот каждые N блоков (default 64)
    --max-bytes <N>           потолок корпуса в байтах (default 256 МиБ)
    --log <F>                 файл телеметрии обучения
    --every <N>               прогресс в stdout каждые N файлов (default 25)
    --resume <F>              RQ9: поднять память из .pqw и продолжить обучение
    --curriculum [SCHEDULE]   RQ9: фазовая сборка — уровни с растущим чанком;
                              default 10:1:512:8K,128:2:1024:32K,
                              1024:4:4096:256K + LENS-уровень из --block/--steps
                              формат уровня BLOCK[:STEPS[:SHOTS[:BUDGET]]]
    --gyro [W]                RQ10: гироскоп памяти J = A − Aᵀ — направленная
                              циркуляция смысла в топологической секции v3;
                              окно W токенов (default 256), O(W)/токен — без O(N²)
    --gyro-budget <N>         RQ10: потолок сырых направленных пар (default 65536)
    --quantized               RQ15: квантованный curriculum — born-шаг прямо
                              в 2-битной решётке тритов: память = контейнер v2
                              бит-в-бит (чекпоинт без переквантования),
                              гистерезис π/3, полюса детерминированы;
                              η — калибровка RQ14 (0.6), если --eta0 не задан;
                              несовместим с --gyro и --decay (v1)

INSPECT OPTIONS (дополнительно):
    --modes <N>               RQ10: число резонансных мод Im(P) для v3 (default 8)
                              RQ11: моды печатаются с невязками Ritz (инвариантность
                              плоскости) и ортонормальности ≡ идемпотентности
                              A² = A + захватом компонент

PRECESS OPTIONS (RQ11: уравнение архетипа a ⊗_ε a = a, p* = a ⊗_ε p*):
    --eta <H>                 шаг прецессии (default: eta из контейнера;
                              веса J нормируются на max|J| — H задаёт скорость)
    --ticks <N>               лимит тиков (default 16384)
    --delta <D>               стоп-порог max|Δθ| за тик (default 1e-9)
    --purify-every <K>        проекция McWeeny каждые K тиков (default 0 —
                              чистая унитарная прецессия; K=2..4 — оседание
                              к архетипу: фазы стягиваются в триты)
    --out <F>                 записать итоговое состояние (контейнер v3,
                              фазы p = cos θ, русла J сохраняются)
    --trace                   печатать всю траекторию (тик, |Δθ|, r, torque)
    --json                    машинно-читаемый отчёт

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
        Some("precess") => cmd_precess(&args[1..]),
        Some("bloch") => cmd_bloch(&args[1..]),
        Some("encrypt") => cmd_encrypt(&args[1..]),
        Some("decrypt") => cmd_decrypt(&args[1..]),
        Some("inspect") => cmd_inspect(&args[1..]),
        Some("train") => cmd_train(&args[1..]),
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

fn cmd_inspect(args: &[String]) -> i32 {
    struct InspectConfig {
        hex_limit: Option<usize>, // None — без hex
        decode: Option<usize>,    // None — без декода; usize::MAX — все дуги
        graph: bool,
        dot: Option<String>,
        matrix: bool,
        strings: bool,
        raw_dim: Option<u32>,
        json: bool,
        modes: usize, // RQ10: число резонансных мод гироскопа
    }
    let mut cfg = InspectConfig {
        hex_limit: Some(512),
        decode: Some(ARCS_PREVIEW),
        graph: false,
        dot: None,
        matrix: false,
        strings: false,
        raw_dim: None,
        json: false,
        modes: 8,
    };
    let mut file: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        macro_rules! val {
            ($name:expr) => {{
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("pqc inspect: missing value for {}\n\n{USAGE}", $name);
                    return 2;
                };
                v.clone()
            }};
        }
        match a {
            "--hex" => {
                let v = val!("--hex");
                cfg.hex_limit = if v == "all" {
                    Some(usize::MAX)
                } else {
                    match v.parse::<usize>() {
                        Ok(n) => Some(n),
                        Err(_) => {
                            eprintln!("pqc inspect: bad --hex: {v} (N|all)\n\n{USAGE}");
                            return 2;
                        }
                    }
                };
            }
            "--no-hex" => cfg.hex_limit = None,
            "--decode" => {
                let v = val!("--decode");
                cfg.decode = if v == "all" {
                    Some(usize::MAX)
                } else {
                    match v.parse::<usize>() {
                        Ok(n) => Some(n),
                        Err(_) => {
                            eprintln!("pqc inspect: bad --decode: {v} (all|N)\n\n{USAGE}");
                            return 2;
                        }
                    }
                };
            }
            "--graph" => cfg.graph = true,
            "--dot" => cfg.dot = Some(val!("--dot")),
            "--matrix" => cfg.matrix = true,
            "--strings" => cfg.strings = true,
            "--raw-dim" => {
                let v = val!("--raw-dim");
                match v.parse::<u32>() {
                    Ok(n) if n > 0 => cfg.raw_dim = Some(n),
                    _ => {
                        eprintln!("pqc inspect: bad --raw-dim: {v} (> 0)\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            "--json" => cfg.json = true,
            "--modes" => {
                let v = val!("--modes");
                match v.parse::<usize>() {
                    Ok(n) if n > 0 && n <= 64 => cfg.modes = n,
                    _ => {
                        eprintln!("pqc inspect: bad --modes: {v} (1..=64)\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            "--all" => {
                cfg.hex_limit = Some(usize::MAX);
                cfg.decode = Some(usize::MAX);
                cfg.graph = true;
                cfg.matrix = true;
                cfg.strings = true;
            }
            other if !other.starts_with("--") => {
                if file.replace(other.to_string()).is_some() {
                    eprintln!("pqc inspect: file given twice\n\n{USAGE}");
                    return 2;
                }
            }
            other => {
                eprintln!("pqc inspect: unknown option {other}\n\n{USAGE}");
                return 2;
            }
        }
        i += 1;
    }

    let Some(file) = file else {
        eprintln!("pqc inspect: file required\n\n{USAGE}");
        return 2;
    };
    let src = match load(&file) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pqc inspect: {e}");
            return 1;
        }
    };
    let data = src.as_slice();
    let size = data.len();

    // ── Детект формата и загрузка дуг ──
    let mut kind = detect_kind(data);
    let mut parse_error: Option<String> = None;
    let mut reader: Option<PqwReader> = None;
    let mut arcs: Vec<DecodedArc> = Vec::new();
    let mut d_pol: u32 = 0;

    match kind {
        FileKind::PqwV1 | FileKind::PqwV2 | FileKind::PqwV3 => match try_pqw_reader(data) {
            Ok(Some(r)) => {
                d_pol = r.d_pol();
                arcs = reader_arcs(&r);
                reader = Some(r);
            }
            Ok(None) => parse_error = Some("file shorter than the 128-byte header".into()),
            Err(e) => parse_error = Some(e),
        },
        FileKind::RawPacked4 { d_pol: d } => {
            d_pol = cfg.raw_dim.unwrap_or(d);
            if d_pol as usize > size * 4 {
                eprintln!(
                    "pqc inspect: --raw-dim {d_pol} exceeds 4 × {size} = {} trits",
                    size * 4
                );
                return 2;
            }
            arcs = raw_arcs(data);
            arcs.retain(|a| a.index < d_pol);
        }
        FileKind::Opaque => {
            if let Some(d) = cfg.raw_dim {
                kind = FileKind::RawPacked4 { d_pol: d };
                d_pol = d;
                if d_pol as usize > size * 4 {
                    eprintln!(
                        "pqc inspect: --raw-dim {d_pol} exceeds 4 × {size} = {} trits",
                        size * 4
                    );
                    return 2;
                }
                arcs = raw_arcs(data);
                arcs.retain(|a| a.index < d_pol);
            }
        }
    }

    let recon: CryptoRecon = crypto_recon(data);

    // ── RQ10: гироскопная топология v3 — J = A − Aᵀ + моды Im(P) ──
    let gyro_section = reader.as_ref().and_then(|r| r.gyro());
    let gyro_modes = gyro_section.as_ref().map(|g| {
        let pairs: Vec<(u32, u32, f64)> = g
            .pairs()
            .iter()
            .map(|p| (p.i, p.j, p.weight))
            .collect();
        pqc::gyro::resonant_modes_from_pairs(&pairs, cfg.modes)
    });
    let gyro_json: Option<Json> = gyro_section.as_ref().map(|g| {
        let mut obj = vec![
            ("present".into(), Json::Bool(true)),
            ("window".into(), Json::num(g.window() as f64)),
            ("ticks".into(), Json::num(g.ticks() as f64)),
            ("pairs".into(), Json::num(g.pairs().len() as f64)),
            ("scale".into(), Json::num(g.scale() as f64)),
            ("index16".into(), Json::Bool(g.index16())),
        ];
        if let Some(modes) = &gyro_modes {
            // Фазовый портрет Im(P): p̂ и θ = arccos(p̂) дуг моды —
            // из фазовой секции контейнера.
            let phase: std::collections::HashMap<u32, f64> =
                arcs.iter().map(|a| (a.index, a.p_hat)).collect();
            let comp = |v: &[(u32, f64)]| -> Json {
                Json::Arr(
                    v.iter()
                        .map(|&(n, c)| {
                            let mut fields = vec![
                                ("idx".into(), Json::num(f64::from(n))),
                                ("comp".into(), Json::num(c)),
                            ];
                            if let Some(&p) = phase.get(&n) {
                                fields.push(("p".into(), Json::num(p)));
                                fields.push(("theta".into(), Json::num(p.acos())));
                            }
                            Json::Obj(fields)
                        })
                        .collect(),
                )
            };
            let modes_json: Vec<Json> = modes
                .iter()
                .map(|m| {
                    Json::Obj(vec![
                        ("lambda".into(), Json::num(m.lambda)),
                        ("ritz_residual".into(), Json::num(m.ritz_residual)),
                        ("ortho_residual".into(), Json::num(m.ortho_residual)),
                        ("capture".into(), Json::num(m.capture)),
                        ("u".into(), comp(&m.u)),
                        ("v".into(), comp(&m.v)),
                    ])
                })
                .collect();
            obj.push(("modes".into(), Json::Arr(modes_json)));
        }
        Json::Obj(obj)
    });

    // ── JSON-режим: единый объект и выход ──
    if cfg.json {
        println!(
            "{}",
            report_json(&file, data, kind, &arcs, d_pol, &recon, gyro_json).to_string()
        );
        return 0;
    }

    // ── Человекочитаемый отчёт ──
    println!("POLER Quantum Core — inspect");
    println!("file      : {file} ({size} B, {})", src.kind());
    println!("sha256    : {}", pqc::inspect::sha256_hex(data));
    if let Some(e) = &parse_error {
        println!("parse     : ERROR — {e} (дальше — сырая разведка)");
    }

    println!("\nFORMAT");
    println!("  kind       : {}", kind.name());
    if d_pol > 0 {
        let neg = arcs.iter().filter(|a| a.trit < 0).count();
        let pos = arcs.iter().filter(|a| a.trit > 0).count();
        let density = arcs.len() as f64 / f64::from(d_pol) * 100.0;
        println!("  d_pol      : {d_pol}");
        println!(
            "  nnz        : {} ({} × −1, {} × +1), density {density:.3}%",
            arcs.len(),
            neg,
            pos
        );
        println!(
            "  born       : H = {:.4} bits, QCM = {:.6}",
            born_entropy(&arcs),
            qcm_theory(&arcs, d_pol)
        );
    }

    // ── RQ10: гироскоп J = A − Aᵀ (топологическая секция v3) ──
    if let Some(g) = &gyro_section {
        let h = reader.as_ref().map(|r| r.header()).unwrap();
        println!("\nGYRO (J = A − Aᵀ, кососимметричный резонансный оператор)");
        println!(
            "  window     : {} токенов направленного контекста",
            g.window()
        );
        println!("  ticks      : {} наблюдённых событий потока", g.ticks());
        println!(
            "  pairs      : {} хранимых пар (верхний треугольник, ε-ворота)",
            g.pairs().len()
        );
        println!(
            "  section    : {} B @ 0x{:x} (5 B/пара, веса i8, scale {:.4})",
            h.topology_len, h.topology_offset, g.scale()
        );
        // Превью главных русел циркуляции.
        let mut top: Vec<&pqw::GyroPair> = g.pairs().iter().collect();
        top.sort_by(|a, b| b.weight.abs().total_cmp(&a.weight.abs()));
        let preview = top.len().min(8);
        if preview > 0 {
            println!("  top flow   : (src → dst, вес — направление потока смысла)");
            for p in &top[..preview] {
                println!("               {:>5} → {:<5} {:+.4}", p.i, p.j, p.weight);
            }
        }
        // Резонансные моды: Im(P) — фазовые векторы смысла.
        if let Some(modes) = &gyro_modes {
            if !modes.is_empty() {
                println!("\n  моды Im(P): плоскости вращения (u, v), λ = угловая скорость;");
                println!("               θ = arccos(p̂) — фазовый угол дуги (вектор смысла);");
                println!("               Ritz — инвариантность плоскости, орто ≡ идемпотентность");
                println!("               A² = A (уравнение архетипа), захват — доля массы в ≤12 дугах");
                let phase: std::collections::HashMap<u32, f64> =
                    arcs.iter().map(|a| (a.index, a.p_hat)).collect();
                for (k, m) in modes.iter().enumerate() {
                    println!(
                        "  λ{} = {:.4}  [Ritz {:.1e} | орто {:.1e} | захват {:.1}%]",
                        k + 1,
                        m.lambda,
                        m.ritz_residual,
                        m.ortho_residual,
                        100.0 * m.capture
                    );
                    for (label, vec) in [("u", &m.u), ("v", &m.v)] {
                        let shown = vec.len().min(6);
                        if shown == 0 {
                            continue;
                        }
                        let comps: Vec<String> = vec[..shown]
                            .iter()
                            .map(|&(n, c)| {
                                match phase.get(&n) {
                                    Some(&p) => format!(
                                        "{n}({:+.2}|p{:+.1}|θ{:.2}π)",
                                        c,
                                        p,
                                        p.acos() / std::f64::consts::PI
                                    ),
                                    None => format!("{n}({c:+.2})"),
                                }
                            })
                            .collect();
                        println!("    {label}      : {}", comps.join("  "));
                    }
                }
            }
        }
    }

    println!("\nCRYPTO RECON");
    println!("  entropy    : {:.4} / 8.0000 bits/byte", recon.entropy);
    if recon.chi2.is_finite() {
        println!(
            "  chi2       : {:.1} (uniform band 255 ± 352) — {}",
            recon.chi2,
            if (recon.chi2 - 255.0).abs() <= 352.0 {
                "uniform"
            } else {
                "NOT uniform"
            }
        );
    }
    println!("  distinct   : {} / 256 byte values", recon.distinct);
    if !recon.blocks.is_empty() {
        let line: Vec<String> = recon.blocks.iter().map(|h| format!("{h:.2}")).collect();
        println!("  blocks 64B : {}", line.join(" "));
    }
    if recon.container_hits.is_empty() {
        println!("  containers : — (шифро-контейнеров не найдено)");
    } else {
        println!("  containers : {}", recon.container_hits.join("; "));
    }
    match recon.verdict {
        "structured" => {
            println!("  verdict    : structured — шифрования НЕТ, данные полностью читаемы")
        }
        "compressed" => {
            println!("  verdict    : compressed — похоже на сжатые данные (структура скрыта)")
        }
        _ => println!(
            "  verdict    : encrypted-like — похоже на шифр/случайность; без ключа не читать"
        ),
    }

    // ── Побайтовая карта заголовка .pqw ──
    if let Some(r) = &reader {
        println!("\nBYTE MAP (header 0x00..0x80)");
        println!("  offset size  field                    value");
        for row in header_rows(data, r) {
            println!(
                "  0x{:04x} {:>4}   {:<24} {}",
                row.offset, row.size, row.name, row.value
            );
        }
        println!("  секции: header 0x00..0x80 → topology → phases → EOF");
    }

    // ── Hex-дамп ──
    if let Some(limit) = cfg.hex_limit {
        let shown = limit.min(size);
        println!("\nHEX (first {shown} B of {size})");
        print!("{}", hex_dump(data, shown));
        if shown < size {
            println!("  … (обрезано; --hex all — весь файл)");
        }
    }

    // ── Декод дуг ──
    if let Some(limit) = cfg.decode {
        if !arcs.is_empty() {
            let shown = limit.min(arcs.len());
            println!("\nARCS ({} of {}; --decode all — все)", shown, arcs.len());
            println!("       idx   hex   trit   p̂        θ̂");
            print!("{}", arcs_csr_text(&arcs[..shown]));
        }
    }

    // ── Граф LENS ──
    if cfg.graph && !arcs.is_empty() {
        let indices: Vec<u32> = arcs.iter().map(|a| a.index).collect();
        let stats = graph_stats(&indices, d_pol);
        println!("\nGRAPH (LENS: рёбра = соседние дуги u_k → u_k+1)");
        println!(
            "  nodes {}, edges {}, components {}, density {:.3}%",
            stats.nodes,
            stats.edges,
            stats.components,
            stats.density * 100.0
        );
        let csr: Vec<String> = indices.iter().map(|i| i.to_string()).collect();
        println!("  CSR indices: [{}]", csr.join(", "));
    }

    // ── DOT ──
    if let Some(target) = &cfg.dot {
        let dot = arcs_dot(&arcs, d_pol);
        if target == "-" {
            println!("\nDOT");
            print!("{dot}");
        } else {
            match std::fs::write(target, dot) {
                Ok(()) => println!("\nDOT       : written to {target}"),
                Err(e) => {
                    eprintln!("pqc inspect: cannot write --dot {target}: {e}");
                    return 1;
                }
            }
        }
    }

    // ── ASCII-матрица ──
    if cfg.matrix {
        if d_pol == 0 {
            println!("\nMATRIX    : нет дуг — матрица пуста");
        } else if let Some(m) = ascii_matrix(&arcs, d_pol) {
            println!("\nMATRIX (adjacency, d_pol = {d_pol}; '#' — активная дуга, 'X' — ребро)");
            print!("{m}");
        } else {
            println!(
                "\nMATRIX    : пропущена — d_pol = {d_pol} > {MATRIX_D_MAX} (матрица осмысленна при d_pol ≤ {MATRIX_D_MAX})"
            );
        }
    }

    // ── Строки ──
    if cfg.strings {
        let strings = ascii_strings(data, 6);
        if strings.is_empty() {
            println!("\nSTRINGS   : — (печатаемых ASCII-строк ≥ 6 нет)");
        } else {
            println!("\nSTRINGS (ASCII ≥ 6, первые {})", strings.len());
            for s in &strings {
                println!("  {s}");
            }
        }
    }

    // ── Целостность (для контейнеров .pqw) ──
    if let Some(r) = &reader {
        println!("\nINTEGRITY");
        println!("  header checksum : OK (FNV-1a64, валидирована при разборе)");
        match r.verify_payload() {
            Ok(()) => println!("  payload digest  : OK (SHA-256 trunc-24)"),
            Err(e) => println!("  payload digest  : FAIL — {e}"),
        }
        println!("  mcweeny stored  : {:.3e}", r.mcweeny_residual());
        let residual = pqc::inspect::mcweeny_of_arcs(&arcs);
        if residual.is_finite() {
            println!("  mcweeny actual  : {residual:.3e} (max |λ² − λ| по дугам)");
        }
    } else if kind.is_pqw() {
        println!("\nINTEGRITY");
        println!("  недоступна: контейнер не разобран (см. parse ERROR выше)");
    }

    0
}

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

// ============================================================================
// pqc precess — RQ11: петля архетипа поверх чекпоинта.
//
// Уравнение архетипа в алгебре смыслов (O, ⊕, ⊗_ε):
//   * a ⊗_ε a = a      — идемпотентность (McWeeny / спектральные проекторы мод);
//   * p* = a ⊗_ε p*    — фиксация (неподвижная точка транспорта фаз).
//
// Петля: транспорт фаз precess_step (унитарный, O(nnz(J))) с опциональной
// проекцией McWeeny каждые K тиков (диссипативная половина). Чистая
// прецессия орбитирует вокруг архетипа (спектр J мнимый); с очисткой
// фазы садятся на триты {0, ±1} при погашенных моментах — контейнер-
// архетип с нулевым остатком идемпотентности.
// ============================================================================

struct PrecessConfig {
    eta: Option<f64>,
    ticks: u64,
    delta: f64,
    purify_every: u64,
    out: Option<String>,
    trace: bool,
    json: bool,
}

impl Default for PrecessConfig {
    fn default() -> PrecessConfig {
        PrecessConfig {
            eta: None,
            ticks: 16_384,
            delta: 1e-9,
            purify_every: 0,
            out: None,
            trace: false,
            json: false,
        }
    }
}

fn cmd_precess(args: &[String]) -> i32 {
    let mut cfg = PrecessConfig::default();
    let mut file: Option<String> = None;
    let mut i = 0usize;
    macro_rules! val {
        ($name:literal) => {{
            i += 1;
            if i >= args.len() {
                eprintln!("pqc precess: {} требует значение\n\n{USAGE}", $name);
                return 2;
            }
            args[i].clone()
        }};
    }
    while i < args.len() {
        match args[i].as_str() {
            "--eta" => {
                let v = val!("--eta");
                match v.parse::<f64>() {
                    Ok(h) if h > 0.0 && h.is_finite() => cfg.eta = Some(h),
                    _ => {
                        eprintln!("pqc precess: bad --eta: {v} (> 0)\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            "--ticks" => {
                let v = val!("--ticks");
                match v.parse::<u64>() {
                    Ok(n) if n > 0 && n <= 100_000_000 => cfg.ticks = n,
                    _ => {
                        eprintln!("pqc precess: bad --ticks: {v} (1..=1e8)\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            "--delta" => {
                let v = val!("--delta");
                match v.parse::<f64>() {
                    Ok(d) if d > 0.0 && d.is_finite() => cfg.delta = d,
                    _ => {
                        eprintln!("pqc precess: bad --delta: {v} (> 0)\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            "--purify-every" => {
                let v = val!("--purify-every");
                match v.parse::<u64>() {
                    Ok(k) if k <= 1024 => cfg.purify_every = k,
                    _ => {
                        eprintln!("pqc precess: bad --purify-every: {v} (0..=1024)\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            "--out" => cfg.out = Some(val!("--out")),
            "--trace" => cfg.trace = true,
            "--json" => cfg.json = true,
            other if !other.starts_with("--") => {
                if file.replace(other.to_string()).is_some() {
                    eprintln!("pqc precess: file given twice\n\n{USAGE}");
                    return 2;
                }
            }
            other => {
                eprintln!("pqc precess: неизвестный флаг {other}\n\n{USAGE}");
                return 2;
            }
        }
        i += 1;
    }
    let Some(file_path) = file else {
        eprintln!("pqc precess: требуется путь к контейнеру .poler / .pqw (v3 с --gyro)\n\n{USAGE}");
        return 2;
    };

    let raw = match std::fs::read(&file_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pqc precess: ошибка чтения {file_path}: {e}");
            return 1;
        }
    };
    let reader = match PqwReader::from_bytes(&raw) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc precess: ошибка парсинга {file_path}: {e}");
            return 1;
        }
    };
    let Some(section) = reader.gyro() else {
        eprintln!(
            "pqc precess: контейнер без топологической секции (v1/v2) — \\\n\
             петле архетипа нужен гироскоп J = A − Aᵀ; обучите с --gyro"
        );
        return 1;
    };

    let d_pol = reader.d_pol() as usize;
    let hyper = reader.hyperparams();
    let eta = cfg.eta.unwrap_or(f64::from(hyper.eta));

    // Вектор фаз: θ = arccos(p̂) для хранимых дуг, фон π/2 (p = 0).
    let mut thetas = vec![std::f64::consts::FRAC_PI_2; d_pol];
    for (idx, p) in reader.decoded() {
        if (idx as usize) < d_pol {
            thetas[idx as usize] = p.clamp(-1.0, 1.0).acos();
        }
    }

    let pairs: Vec<(u32, u32, f64)> = section
        .pairs()
        .iter()
        .map(|p| (p.i, p.j, p.weight))
        .collect();
    // Дуги русел J (уникальные) — для сводки.
    let mut channel_arcs: Vec<u32> = pairs.iter().flat_map(|&(i, j, _)| [i, j]).collect();
    channel_arcs.sort_unstable();
    channel_arcs.dedup();

    if !cfg.json {
        println!("POLER Quantum Core — Precess: петля архетипа (RQ11)");
        println!("container : {file_path} ({} B)", raw.len());
        println!(
            "d_pol     : {d_pol} | фазы: {} дуг | русла J: {} пар, {} дуг | такты гироскопа: {}",
            reader.nnz(),
            pairs.len(),
            channel_arcs.len(),
            section.ticks()
        );
        println!(
            "eta       : {eta:.4} ({}) | лимит: {} тиков | стоп: |Δθ| < {:.0e} | McWeeny: {}",
            if cfg.eta.is_some() { "флаг" } else { "из контейнера" },
            cfg.ticks,
            cfg.delta,
            match cfg.purify_every {
                0 => "выключена (чистая прецессия)".to_string(),
                k => format!("каждые {k} тиков"),
            }
        );
        if cfg.purify_every == 0 {
            println!("режим     : унитарная прецессия — орбита вокруг архетипа (спектр J мнимый);");
            println!("            для оседания к тритам добавьте --purify-every 4");
        }
    }

    // Петля.
    let report = pqc::archetype::precess_to_fixpoint(
        &pairs,
        &mut thetas,
        eta,
        cfg.ticks,
        cfg.delta,
        cfg.purify_every,
    );

    if cfg.json {
        let mut obj = vec![
            ("file".into(), Json::str(&file_path)),
            ("d_pol".into(), Json::num(d_pol as f64)),
            ("nnz".into(), Json::num(reader.nnz() as f64)),
            ("eta".into(), Json::num(eta)),
            ("ticks_limit".into(), Json::num(cfg.ticks as f64)),
            ("stop_delta".into(), Json::num(cfg.delta)),
            ("purify_every".into(), Json::num(cfg.purify_every as f64)),
            ("gyro_pairs".into(), Json::num(pairs.len() as f64)),
            ("gyro_ticks".into(), Json::num(section.ticks() as f64)),
            ("fixated".into(), Json::Bool(report.fixated)),
            ("ticks".into(), Json::num(report.ticks as f64)),
            ("participants".into(), Json::num(report.participants as f64)),
            ("delta_first".into(), Json::num(report.delta_first)),
            ("delta_last".into(), Json::num(report.delta_last)),
            ("contraction".into(), Json::num(report.contraction)),
            ("r_initial".into(), Json::num(report.r_initial)),
            ("r_final".into(), Json::num(report.r_final)),
            ("torque_initial".into(), Json::num(report.torque_initial)),
            ("torque_final".into(), Json::num(report.torque_final)),
            ("omega".into(), Json::num(report.omega)),
            ("travel_max".into(), Json::num(report.travel_max)),
            ("travel_mean".into(), Json::num(report.travel_mean)),
            ("trits_exact".into(), Json::Bool(report.trits_exact)),
        ];
        let trace: Vec<Json> = report
            .trace
            .iter()
            .map(|p| {
                Json::Obj(vec![
                    ("tick".into(), Json::num(p.tick as f64)),
                    ("delta".into(), Json::num(p.delta)),
                    ("r".into(), Json::num(p.r)),
                    ("torque".into(), Json::num(p.torque)),
                ])
            })
            .collect();
        obj.push(("trace".into(), Json::Arr(trace)));
        println!("{}", Json::Obj(obj).to_string());
    } else {
        println!("\nтик        max|Δθ|      r       torque");
        let print_point = |p: &pqc::archetype::TracePoint| {
            println!(
                "{:>8}  {:>10.3e}  {:7.4}  {:8.2e}{}",
                p.tick,
                p.delta,
                p.r,
                p.torque,
                if p.tick == report.ticks && report.fixated {
                    "   ← фиксация"
                } else {
                    ""
                }
            );
        };
        if cfg.trace {
            for p in &report.trace {
                print_point(p);
            }
        } else {
            let n = report.trace.len();
            for p in &report.trace[..n.min(4)] {
                print_point(p);
            }
            if n > 7 {
                println!("       ... ({} точек, --trace — вся траектория)", n);
                for p in &report.trace[n - 3..] {
                    print_point(p);
                }
            }
        }

        println!("\nРЕЗУЛЬТАТ");
        println!(
            "фиксация     : {}",
            if report.fixated {
                format!("достигнута за {} тиков (лимит {})", report.ticks, cfg.ticks)
            } else if report.torque_final < 1e-6 {
                format!(
                    "квази-фиксация: невязка p* = {:.1e}, остаточная микро-орбита |Δθ| = {:.1e} \
                     (меньше η — например --eta 0.05 — снимает её)",
                    report.torque_final, report.delta_last
                )
            } else {
                format!("нет за {} тиков — орбита/маховик", report.ticks)
            }
        );
        println!(
            "сжатие ρ     : {:.5} за тик ({})",
            report.contraction,
            if report.contraction < 1.0 {
                "Банах: отображение сжимающее"
            } else {
                "сжатия нет — унитарный транспорт"
            }
        );
        println!(
            "r (Курамото) : {:.4} → {:.4}",
            report.r_initial, report.r_final
        );
        println!(
            "невязка p*   : torque {:.2e} → {:.2e}   (уравнение p* = a ⊗_ε p*)",
            report.torque_initial, report.torque_final
        );
        if !report.fixated && report.torque_final >= 1e-6 {
            println!(
                "маховик ω    : {:+.6} рад/тик (циркуляция не утихает)",
                report.omega
            );
        }
        println!(
            "пробег дуг   : max {:.3} рад, средний {:.3} рад",
            report.travel_max, report.travel_mean
        );
        println!(
            "триты        : {}",
            if report.trits_exact {
                "точные (|cos θ| ∈ {0,1}) — идемпотентность a ⊗_ε a = a"
            } else {
                "нет (фазы вне решётки {0, π/2, π})"
            }
        );
        let both = report.trits_exact && report.torque_final < 1e-6;
        println!(
            "архетип      : {}",
            if both {
                "ДОСТИГНУТ — оба уравнения выполнены"
            } else if report.torque_final < 1e-6 {
                "фиксация без идемпотентности (добавьте --purify-every 2..4)"
            } else {
                "вращение (орбита вокруг архетипа)"
            }
        );
    }

    // Запись итогового состояния: контейнер v3, фазы p = cos θ,
    // русла J переносятся без изменений (веса исходного масштаба).
    if let Some(out_path) = &cfg.out {
        let state_f32: Vec<f32> = thetas.iter().map(|&t| t.cos() as f32).collect();
        let gyro_pairs: Vec<(u32, u32, f64)> = section
            .pairs()
            .iter()
            .map(|p| (p.i, p.j, p.weight))
            .collect();
        let write = (|| -> Result<usize, String> {
            let mut w = PqwWriter::new(d_pol as u32)
                .map_err(|e| e.to_string())?
                .hyperparams(hyper.eta, hyper.gamma, hyper.rho, hyper.epsilon_threshold);
            w.add_state(&state_f32).map_err(|e| e.to_string())?;
            let data = pqw::GyroData::new(section.window(), section.ticks(), gyro_pairs, d_pol as u32)
                .map_err(|e| e.to_string())?;
            let mut buf = Vec::new();
            w.write_v3(&mut buf, &data).map_err(|e| e.to_string())?;
            std::fs::write(out_path, &buf).map_err(|e| e.to_string())?;
            Ok(buf.len())
        })();
        match write {
            Ok(len) => {
                if !cfg.json {
                    let back = PqwReader::from_bytes(&std::fs::read(out_path).unwrap_or_default())
                        .ok()
                        .map(|r| r.nnz());
                    println!(
                        "\nзаписано     : {out_path} (v3, {len} B, {} дуг, {} пар J)",
                        back.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                        section.pairs().len()
                    );
                    if !report.trits_exact {
                        println!("               (снимок орбиты — фазы не на тритовой решётке)");
                    }
                }
            }
            Err(e) => {
                eprintln!("pqc precess: --out: {e}");
                return 1;
            }
        }
    }

    0
}

// pqc encrypt / decrypt — RQ12: крипто-схема алгебры архетипа (файл 285).
//
// Уравнение: p* = a ⊗_ε p* ⊕ m, восстановление m = p* ⊕ (a ⊗_ε p*).
// Ключ — контейнер v3 с гироскопом J (готовый `precess --out`):
// из русел J детерминированно берутся плотные моды Im(P), проектор
// a = Σ (u uᵀ + v vᵀ) идемпотентен (RQ11: ортонормальность ≡
// идемпотентность), сообщение — паттерн δ = (I−a)(ε·b) в дополнении
// подпространства мод, ключевой поток — a·p₀ (проекция фаз ключа).

struct CryptoConfig {
    key: Option<String>,
    out: Option<String>,
    modes: usize,
    seed: Option<u64>,
    text: Option<String>,
    stdin: bool,
    json: bool,
    /// RQ12-схема на f32-фазах (legacy; трит-схема GF(3) — по умолчанию).
    legacy_f32: bool,
}

impl Default for CryptoConfig {
    fn default() -> Self {
        CryptoConfig {
            key: None,
            out: None,
            modes: 0,
            seed: None,
            text: None,
            stdin: false,
            json: false,
            legacy_f32: false,
        }
    }
}

/// Общий парсер флагов encrypt/decrypt.
fn parse_crypto_args(cmd: &str, args: &[String]) -> Result<(Option<String>, CryptoConfig), i32> {
    let mut cfg = CryptoConfig::default();
    let mut file: Option<String> = None;
    let mut i = 0usize;
    macro_rules! val {
        ($name:literal) => {{
            i += 1;
            if i >= args.len() {
                eprintln!("pqc {cmd}: {} требует значение\n\n{USAGE}", $name);
                return Err(2);
            }
            args[i].clone()
        }};
    }
    while i < args.len() {
        match args[i].as_str() {
            "--key" => cfg.key = Some(val!("--key")),
            "--out" => cfg.out = Some(val!("--out")),
            "--modes" => {
                let v = val!("--modes");
                match v.parse::<usize>() {
                    Ok(k) if k <= pqc::MAX_MODES => cfg.modes = k,
                    _ => {
                        eprintln!("pqc {cmd}: bad --modes: {v} (1..={})", pqc::MAX_MODES);
                        return Err(2);
                    }
                }
            }
            "--seed" => {
                let v = val!("--seed");
                match v.parse::<u64>() {
                    Ok(sd) => cfg.seed = Some(sd),
                    _ => {
                        eprintln!("pqc {cmd}: bad --seed: {v} (u64)");
                        return Err(2);
                    }
                }
            }
            "--text" => cfg.text = Some(val!("--text")),
            "--stdin" => cfg.stdin = true,
            "--json" => cfg.json = true,
            "--f32" => cfg.legacy_f32 = true,
            other if !other.starts_with("--") => {
                if file.replace(other.to_string()).is_some() {
                    eprintln!("pqc {cmd}: файл задан дважды\n\n{USAGE}");
                    return Err(2);
                }
            }
            other => {
                eprintln!("pqc {cmd}: неизвестный флаг {other}\n\n{USAGE}");
                return Err(2);
            }
        }
        i += 1;
    }
    Ok((file, cfg))
}

/// Загрузка ключа-архетипа.
fn load_cipher_key(path: &str, modes: usize) -> Result<pqc::CipherKey, i32> {
    let raw = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pqc crypto: ошибка чтения ключа {path}: {e}");
            return Err(1);
        }
    };
    let reader = match PqwReader::from_bytes(&raw) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc crypto: ошибка парсинга ключа {path}: {e}");
            return Err(1);
        }
    };
    match pqc::CipherKey::from_reader(&reader, modes) {
        Ok(k) => Ok(k),
        Err(e) => {
            eprintln!("pqc crypto: {e}");
            Err(1)
        }
    }
}

fn load_trite_key(path: &str, modes: usize) -> Result<pqc::trite::TritKey, i32> {
    let raw = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pqc crypto: ошибка чтения ключа {path}: {e}");
            return Err(1);
        }
    };
    let reader = match PqwReader::from_bytes(&raw) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc crypto: ошибка парсинга ключа {path}: {e}");
            return Err(1);
        }
    };
    match pqc::trite::TritKey::from_reader(&reader, modes) {
        Ok(k) => Ok(k),
        Err(e) => {
            eprintln!("pqc crypto: {e}");
            Err(1)
        }
    }
}

fn cmd_encrypt(args: &[String]) -> i32 {
    let (file, cfg) = match parse_crypto_args("encrypt", args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let Some(key_path) = &cfg.key else {
        eprintln!("pqc encrypt: требуется --key <ARCH.pqw> (контейнер v3 с гироскопом)");
        return 2;
    };
    let Some(out_path) = &cfg.out else {
        eprintln!("pqc encrypt: требуется --out <CIPHER.pqt>");
        return 2;
    };
    // Сообщение: --text | --stdin | файл.
    let msg: Vec<u8> = if let Some(text) = &cfg.text {
        text.clone().into_bytes()
    } else if cfg.stdin {
        use std::io::Read;
        let mut buf = Vec::new();
        if std::io::stdin().read_to_end(&mut buf).is_err() {
            eprintln!("pqc encrypt: ошибка чтения stdin");
            return 1;
        }
        buf
    } else if let Some(path) = &file {
        match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("pqc encrypt: ошибка чтения {path}: {e}");
                return 1;
            }
        }
    } else {
        eprintln!("pqc encrypt: нужен вход: файл, --text или --stdin");
        return 2;
    };

    if !cfg.legacy_f32 {
        return cmd_encrypt_trite(key_path, out_path, &msg, &cfg);
    }

    let key = match load_cipher_key(key_path, cfg.modes) {
        Ok(k) => k,
        Err(c) => return c,
    };
    let mut rng = match cfg.seed {
        Some(s) => pqc::Rng::seed_from_u64(s),
        None => pqc::Rng::from_entropy(),
    };
    let t0 = std::time::Instant::now();
    let (cipher, rep) = match pqc::encrypt(&key, &msg, &mut rng) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pqc encrypt: {e}");
            return 1;
        }
    };
    let dt = t0.elapsed();
    if let Err(e) = std::fs::write(out_path, &cipher) {
        eprintln!("pqc encrypt: ошибка записи {out_path}: {e}");
        return 1;
    }

    if cfg.json {
        let obj = pqc::Json::Obj(vec![
            ("key".into(), pqc::Json::str(key_path)),
            ("out".into(), pqc::Json::str(out_path)),
            ("msg_len".into(), pqc::Json::num(rep.msg_len as f64)),
            ("out_len".into(), pqc::Json::num(rep.out_len as f64)),
            ("blocks".into(), pqc::Json::num(rep.blocks as f64)),
            ("d_pol".into(), pqc::Json::num(key.d_pol as f64)),
            ("k_modes".into(), pqc::Json::num(rep.k_modes as f64)),
            ("raw_pairs".into(), pqc::Json::num(key.raw_pairs as f64)),
            ("interference_max".into(), pqc::Json::num(rep.interference_max)),
            ("margin_min".into(), pqc::Json::num(rep.margin_min)),
            ("ritz_max".into(), pqc::Json::num(key.ritz_max)),
            ("ortho_max".into(), pqc::Json::num(key.ortho_max)),
            ("digest".into(), pqc::Json::str(&rep.digest_hex)),
            ("seconds".into(), pqc::Json::num(dt.as_secs_f64())),
        ]);
        println!("{}", obj.to_string());
    } else {
        println!("POLER Quantum Core — Encrypt: крипто-схема алгебры архетипа (RQ12)");
        println!("key       : {key_path} (d_pol={}, русел J={}, мод K={})", key.d_pol, key.raw_pairs, key.k_modes);
        println!("            Ritz {:.1e} | орто {:.1e} (идемпотентность a ⊗ a = a)", key.ritz_max, key.ortho_max);
        println!("message   : {} B → {} блоков", rep.msg_len, rep.blocks);
        println!("cipher    : {out_path} ({} B, расширение ×{:.1})", rep.out_len, rep.out_len as f64 / (rep.msg_len.max(1)) as f64);
        println!("помеха    : max |a·(ε·b)| = {:.4} (порог ε/2 = {:.4})", rep.interference_max, pqc::crypto::DECODE_THRESHOLD);
        println!("запас     : min |δ̂ − ε/2| = {:.4}", rep.margin_min);
        println!("digest    : sha256-24 = {}", rep.digest_hex);
        println!("время     : {:.3} с", dt.as_secs_f64());
        println!("уравнение : p* = a ⊗_ε p* ⊕ m — коллапс за 1 такт (a идемпотентен)");
    }
    0
}

/// RQ13: шифрование трит-схемы GF(3) (по умолчанию).
fn cmd_encrypt_trite(key_path: &str, out_path: &str, msg: &[u8], cfg: &CryptoConfig) -> i32 {
    let key = match load_trite_key(key_path, cfg.modes) {
        Ok(k) => k,
        Err(c) => return c,
    };
    let mut rng = match cfg.seed {
        Some(s) => pqc::Rng::seed_from_u64(s),
        None => pqc::Rng::from_entropy(),
    };
    let t0 = std::time::Instant::now();
    let (cipher, rep) = match pqc::trite::encrypt(&key, msg, &mut rng) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pqc encrypt: {e}");
            return 1;
        }
    };
    let dt = t0.elapsed();
    if let Err(e) = std::fs::write(out_path, &cipher) {
        eprintln!("pqc encrypt: ошибка записи {out_path}: {e}");
        return 1;
    }

    if cfg.json {
        let obj = pqc::Json::Obj(vec![
            ("key".into(), pqc::Json::str(key_path)),
            ("out".into(), pqc::Json::str(out_path)),
            ("scheme".into(), pqc::Json::str("trite-gf3")),
            ("msg_len".into(), pqc::Json::num(rep.msg_len as f64)),
            ("out_len".into(), pqc::Json::num(rep.out_len as f64)),
            ("blocks".into(), pqc::Json::num(rep.blocks as f64)),
            ("d_pol".into(), pqc::Json::num(key.d_pol as f64)),
            ("k_modes".into(), pqc::Json::num(rep.k_modes as f64)),
            ("capacity_trites".into(), pqc::Json::num(rep.capacity_trites as f64)),
            ("raw_pairs".into(), pqc::Json::num(key.raw_pairs as f64)),
            ("pairs_total".into(), pqc::Json::num(rep.pairs_total as f64)),
            ("ticks".into(), pqc::Json::num(rep.ticks as f64)),
            ("avalanche".into(), pqc::Json::num(rep.avalanche)),
            ("expansion".into(), pqc::Json::num(rep.expansion)),
            ("ritz_max".into(), pqc::Json::num(key.ritz_max)),
            ("ortho_max".into(), pqc::Json::num(key.ortho_max)),
            ("digest".into(), pqc::Json::str(&rep.digest_hex)),
            ("seconds".into(), pqc::Json::num(dt.as_secs_f64())),
        ]);
        println!("{}", obj.to_string());
    } else {
        println!("POLER Quantum Core — Encrypt: трит-схема алгебры архетипа (RQ13, GF(3))");
        println!("key       : {key_path} (d_pol={}, русел J={}, мод K={})", key.d_pol, key.raw_pairs, key.k_modes);
        println!("            Ritz {:.1e} | орто {:.1e} (идемпотентность a ⊗ a = a)", key.ritz_max, key.ortho_max);
        println!("message   : {} B → {} блоков × {} трит (упаковка {}↔{})", rep.msg_len, rep.blocks, rep.capacity_trites, 19, 30);
        println!("cipher    : {out_path} ({} B, расширение ×{:.2} — было ×43 в RQ12)", rep.out_len, rep.expansion);
        println!("прецессия : {} тактов × {} русл (J {} + решётка LENS {})", rep.ticks, rep.pairs_total, key.raw_pairs, rep.pairs_total - key.raw_pairs);
        println!("лавина    : {:.1}% трит блока от 1 бита (было: 1 бит → 1 фаза в RQ12; потолок GF(3) 66.7%)", 100.0 * rep.avalanche);
        println!("digest    : sha256-24 = {}", rep.digest_hex);
        println!("время     : {:.3} с", dt.as_secs_f64());
        println!("уравнение : p* = a ⊗_ε p* ⊕ m — Packed4-триты, расшифровка побитово точна (GF(3))");
    }
    0
}

fn cmd_decrypt(args: &[String]) -> i32 {
    let (file, cfg) = match parse_crypto_args("decrypt", args) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let Some(key_path) = &cfg.key else {
        eprintln!("pqc decrypt: требуется --key <ARCH.pqw>");
        return 2;
    };
    let Some(cipher_path) = &file else {
        eprintln!("pqc decrypt: требуется путь к шифртексту .pqt / .pqc");
        return 2;
    };
    let cipher = match std::fs::read(cipher_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("pqc decrypt: ошибка чтения {cipher_path}: {e}");
            return 1;
        }
    };
    // Автодетект схемы по магии контейнера: PQT1 — триты RQ13,
    // PQC1 — f32-фазы RQ12 (legacy).
    if cipher.len() >= 4 && cipher[..4] == pqc::trite::TRITE_MAGIC {
        return cmd_decrypt_trite(key_path, &cipher, &cfg, cipher_path);
    }
    let key = match load_cipher_key(key_path, cfg.modes) {
        Ok(k) => k,
        Err(c) => return c,
    };
    let t0 = std::time::Instant::now();
    let (msg, rep) = match pqc::decrypt(&key, &cipher) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pqc decrypt: {e}");
            return 1;
        }
    };
    let dt = t0.elapsed();
    match &cfg.out {
        Some(out_path) => {
            if let Err(e) = std::fs::write(out_path, &msg) {
                eprintln!("pqc decrypt: ошибка записи {out_path}: {e}");
                return 1;
            }
        }
        None => {
            // stdout: как текст (бинарный поток теряется — используйте --out).
            match String::from_utf8(msg.clone()) {
                Ok(text) => print!("{text}"),
                Err(_) => {
                    eprintln!("pqc decrypt: бинарное сообщение — запишите через --out");
                    return 1;
                }
            }
        }
    }

    if cfg.json {
        let obj = pqc::Json::Obj(vec![
            ("key".into(), pqc::Json::str(key_path)),
            ("cipher".into(), pqc::Json::str(cipher_path)),
            ("blocks".into(), pqc::Json::num(rep.blocks as f64)),
            ("msg_len".into(), pqc::Json::num(rep.msg_len as f64)),
            ("margin_min".into(), pqc::Json::num(rep.margin_min)),
            ("margin_mean".into(), pqc::Json::num(rep.margin_mean)),
            ("seconds".into(), pqc::Json::num(dt.as_secs_f64())),
        ]);
        println!("{}", obj.to_string());
    } else {
        println!("POLER Quantum Core — Decrypt: m = p* ⊕ (a ⊗_ε p*) (RQ12)");
        println!("cipher    : {cipher_path} ({} блоков)", rep.blocks);
        println!(
            "message   : {} B → {}",
            rep.msg_len,
            cfg.out.as_deref().unwrap_or("<stdout>")
        );
        println!("запас     : min {:.4} | средний {:.4} (порог ε/2 = {:.4})",
            rep.margin_min, rep.margin_mean, pqc::crypto::DECODE_THRESHOLD);
        println!("время     : {:.3} с", dt.as_secs_f64());
    }
    0
}

/// RQ13: расшифровка трит-схемы GF(3) — побитово точная (без порогов).
fn cmd_decrypt_trite(
    key_path: &str,
    cipher: &[u8],
    cfg: &CryptoConfig,
    cipher_path: &str,
) -> i32 {
    let key = match load_trite_key(key_path, cfg.modes) {
        Ok(k) => k,
        Err(c) => return c,
    };
    let t0 = std::time::Instant::now();
    let (msg, rep) = match pqc::trite::decrypt(&key, cipher) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("pqc decrypt: {e}");
            return 1;
        }
    };
    let dt = t0.elapsed();
    match &cfg.out {
        Some(out_path) => {
            if let Err(e) = std::fs::write(out_path, &msg) {
                eprintln!("pqc decrypt: ошибка записи {out_path}: {e}");
                return 1;
            }
        }
        None => match String::from_utf8(msg.clone()) {
            Ok(text) => print!("{text}"),
            Err(_) => {
                eprintln!("pqc decrypt: бинарное сообщение — запишите через --out");
                return 1;
            }
        },
    }

    if cfg.json {
        let obj = pqc::Json::Obj(vec![
            ("key".into(), pqc::Json::str(key_path)),
            ("cipher".into(), pqc::Json::str(cipher_path)),
            ("scheme".into(), pqc::Json::str("trite-gf3")),
            ("blocks".into(), pqc::Json::num(rep.blocks as f64)),
            ("msg_len".into(), pqc::Json::num(rep.msg_len as f64)),
            ("ticks".into(), pqc::Json::num(rep.ticks as f64)),
            ("seconds".into(), pqc::Json::num(dt.as_secs_f64())),
        ]);
        println!("{}", obj.to_string());
    } else {
        println!("POLER Quantum Core — Decrypt: m = p* ⊕ (a ⊗_ε p*) в GF(3) (RQ13)");
        println!("cipher    : {cipher_path} ({} блоков, {} тактов прецессии)", rep.blocks, rep.ticks);
        println!(
            "message   : {} B → {}",
            rep.msg_len,
            cfg.out.as_deref().unwrap_or("<stdout>")
        );
        println!("точность  : GF(3)-арифметика целая — расшифровка побитово точна");
        println!("время     : {:.3} с", dt.as_secs_f64());
    }
    0
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
    println!(
        "source    : {} ({} B, d_pol={})",
        file_path,
        raw.len(),
        reader.d_pol()
    );

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

// ============================================================================
// pqc train — накопительное глубокое обучение с плотным LENS-графом (RQ8).
//
// Отличие от `pqc stream`: движок ОДИН на весь корпус, чанки (блоки по
// --block байт) льются подряд, support объединяется (merge_support),
// а снапшот сериализует НАКОПЛЕННУЮ память engine.model() — не буфер
// последнего чанка. Файл-отпечаток (raw Packed4) обновляется каждые
// --snapshot-every блоков: состояние можно мониторить на живую
// (pqc inspect / hexdump / radare2) прямо во время обучения.
// ============================================================================

/// Текстовые расширения корпуса обучения.
const TRAIN_EXTENSIONS: &[&str] = &[
    "rs", "py", "md", "txt", "json", "c", "h", "cpp", "hpp", "cc", "js", "ts", "html", "htm",
    "css", "toml", "yaml", "yml", "sh", "java", "scala", "kt", "go", "rb", "php", "sql", "tex",
];

/// Потолок размера одного файла корпуса (гигантские тома пропускаем).
const TRAIN_FILE_MAX: u64 = 8 << 20;

struct TrainConfig {
    corpus: Option<String>,
    stdin: bool,
    dim: u32,
    epsilon: f32,
    block: usize,
    shots: u64,
    steps: usize,
    seed: u64,
    eta0: f64,
    beta: f64,
    gamma: f64,
    decay: bool,
    out: Option<String>,
    fingerprint: Option<String>,
    snapshot_every: usize,
    max_bytes: u64,
    log: Option<String>,
    json: bool,
    every: usize,
    /// RQ9: поднять накопленную память из .pqw-чекпоинта перед обучением.
    resume: Option<String>,
    /// RQ9: фазовая сборка — расписание уровней с растущим чанком.
    curriculum: Option<Vec<TrainStage>>,
    /// RQ10: окно гироскопа J = A − Aᵀ (None — гироскоп выключен).
    gyro: Option<usize>,
    /// RQ10: бюджет сырых направленных пар гироскопа.
    gyro_budget: usize,
    /// RQ15: квантованный curriculum — born-шаг в тритовой решётке.
    quantized: bool,
    /// --eta0 задан явно (иначе квантованный путь берёт калибровку RQ14 η=0.6).
    eta0_explicit: bool,
}

/// Один уровень фазовой сборки (RQ9): размер чанка, шаги Born-петли,
/// выстрелы на измерение и бюджет уровня в байтах (0 = без потолка).
#[derive(Clone)]
struct TrainStage {
    block: usize,
    steps: usize,
    shots: u64,
    budget: u64,
}

/// Статистика пройденного уровня curriculum.
struct StageStats {
    level: usize,
    name: &'static str,
    block: usize,
    steps: usize,
    shots: u64,
    budget: u64,
    blocks: u64,
    tokens: u64,
    nnz_before: u64,
    nnz_after: u64,
    support_before: usize,
    support_after: usize,
    elapsed: f64,
}

impl Default for TrainConfig {
    fn default() -> Self {
        TrainConfig {
            corpus: None,
            stdin: false,
            dim: 4096,
            epsilon: 0.05,
            block: 8192,
            shots: 20_000,
            steps: 10,
            seed: 42,
            eta0: 0.25,
            beta: 1.0,
            gamma: 0.5,
            decay: false,
            out: None,
            fingerprint: None,
            snapshot_every: 64,
            max_bytes: 256 << 20,
            log: None,
            json: false,
            every: 25,
            resume: None,
            curriculum: None,
            gyro: None,
            gyro_budget: 65_536,
            quantized: false,
            eta0_explicit: false,
        }
    }
}

/// Расписание фазовой сборки по умолчанию: три уровня разогрева памяти
/// (микрочанки → морфемы → синтаксис) + финальный LENS-уровень из
/// --block/--steps/--shots. Бюджеты уровней — префиксы корпуса.
///
/// Калибровка (физика стоимости): Born-шаг стоит O(shots × support), а
/// микро-чанки рождают дуги почти на каждый токен — support взлетает к
/// ~90% d_pol за первые же сотни блоков. Поэтому бюджеты разогрева —
/// выборки алфавита (он повторяется!), а не весь корпус: алфавит и
/// морфемы выучиваются на малой доле данных, полную LENS-топологию
/// строит финальный уровень на всём объёме.
fn default_curriculum(cfg: &TrainConfig) -> Vec<TrainStage> {
    vec![
        TrainStage {
            block: 10,
            steps: 1,
            shots: 512,
            budget: 8 << 10,
        },
        TrainStage {
            block: 128,
            steps: 2,
            shots: 1024,
            budget: 32 << 10,
        },
        TrainStage {
            block: 1024,
            steps: 4,
            shots: 4096,
            budget: 256 << 10,
        },
        TrainStage {
            block: cfg.block,
            steps: cfg.steps,
            shots: cfg.shots,
            budget: 0,
        },
    ]
}

/// Имя уровня для баннера (физика фазовой сборки).
fn stage_name(level: usize, total: usize) -> &'static str {
    match (level, total) {
        (1, 4) => "РЕГИСТРЫ: буквы, опкоды, шум тактов",
        (2, 4) => "МОРФЕМЫ: стыковка корней и типов",
        (3, 4) => "СИНТАКСИС: правила блоков и скобок",
        (4, 4) => "LENS-ТОПОЛОГИЯ: граф связей реальности",
        _ => "УРОВЕНЬ",
    }
}

/// Парсер расписания `B1[:S1[:H1[:Z1]]],B2:...` (block:steps:shots:budget).
fn parse_curriculum(spec: &str) -> Result<Vec<TrainStage>, String> {
    let mut stages = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err("пустой уровень в --curriculum".to_string());
        }
        let fields: Vec<&str> = part.split(':').collect();
        if fields.len() > 4 {
            return Err(format!(
                "уровень '{part}': максимум 4 поля block:steps:shots:budget"
            ));
        }
        let block: usize = fields[0]
            .parse()
            .map_err(|_| format!("уровень '{part}': block не число"))?;
        if block == 0 {
            return Err(format!("уровень '{part}': block должен быть > 0"));
        }
        let steps: usize = match fields.get(1) {
            Some(s) => s
                .parse()
                .map_err(|_| format!("уровень '{part}': steps не число"))?,
            None => 1,
        };
        let shots: u64 = match fields.get(2) {
            Some(s) => s
                .parse()
                .map_err(|_| format!("уровень '{part}': shots не число"))?,
            None => 2_000,
        };
        let budget: u64 = match fields.get(3) {
            Some(s) => s
                .parse()
                .map_err(|_| format!("уровень '{part}': budget не число"))?,
            None => 0,
        };
        stages.push(TrainStage {
            block,
            steps,
            shots,
            budget,
        });
    }
    if stages.is_empty() {
        return Err("--curriculum: пустое расписание".to_string());
    }
    Ok(stages)
}

/// Счётчики цикла обучения.
struct TrainStats {
    total_tokens: u64,
    total_blocks: u64,
    no_hits: u64,
    last_loss: f64,
    last_qcm_gap: f64,
    blocks_since_snapshot: usize,
    snapshot: (usize, u64, usize, usize),
    /// RQ15: суммарные перещёлкивания тритов за прогон.
    moved_total: usize,
    /// RQ15: доля перещёлкиваний последнего блока.
    last_moved_frac: f64,
}

fn train_usage_err(msg: &str) -> i32 {
    eprintln!("pqc train: {msg}\n\n{USAGE}");
    2
}

/// Рекурсивный обход каталога: текстовые файлы ≤ TRAIN_FILE_MAX, сортировка путей.
fn collect_corpus(root: &Path, files: &mut Vec<std::path::PathBuf>, total: &mut u64, cap: u64) {
    let rd = match std::fs::read_dir(root) {
        Ok(r) => r,
        Err(_) => return,
    };
    let mut entries: Vec<_> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if *total >= cap {
            return;
        }
        if p.is_dir() {
            collect_corpus(&p, files, total, cap);
        } else if p.is_file() {
            let ext_ok = p
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| TRAIN_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
                .unwrap_or(false);
            if !ext_ok {
                continue;
            }
            let sz = match p.metadata() {
                Ok(m) => m.len(),
                Err(_) => continue,
            };
            if sz == 0 || sz > TRAIN_FILE_MAX {
                continue;
            }
            *total += sz;
            files.push(p);
        }
    }
}

/// Диспетчер учебных движков cmd_train: плотный LENS (RQ8) или
/// квантованная решётка тритов (RQ15).
enum Trainer {
    Dense(pqc::stream_engine::StreamEngine),
    Quantized(pqc::qcurriculum::QuantizedCurriculum),
}

/// Итог одного блока обучения для счётчиков цикла.
struct FeedOutcome {
    tokens: usize,
    no_hits: bool,
    param_loss: f64,
    qcm_gap: f64,
    moved: usize,
    moved_frac: f64,
}

impl Trainer {
    /// Прогон одного блока живых данных через активный движок.
    fn feed(&mut self, text: &str, steps: usize) -> pqc::Result<FeedOutcome> {
        match self {
            Trainer::Dense(e) => {
                let r = e.ingest(text, steps)?;
                Ok(FeedOutcome {
                    tokens: r.tokens,
                    no_hits: r.no_hits,
                    param_loss: r.param_loss,
                    qcm_gap: r.qcm.qcm_gap(),
                    moved: 0,
                    moved_frac: 0.0,
                })
            }
            Trainer::Quantized(q) => {
                let r = q.ingest(text, steps)?;
                Ok(FeedOutcome {
                    tokens: r.tokens,
                    no_hits: r.no_hits,
                    param_loss: r.param_loss,
                    qcm_gap: 0.0,
                    moved: r.moved,
                    moved_frac: r.moved_frac,
                })
            }
        }
    }

    /// Бюджет выстрелов уровня curriculum.
    fn set_shots(&mut self, shots: u64) {
        match self {
            Trainer::Dense(e) => e.set_shots(shots),
            Trainer::Quantized(q) => {
                q.set_shots(shots);
            }
        }
    }

    /// Поднять накопленную память из чекпоинта.
    fn resume(&mut self, reader: &PqwReader) -> pqc::Result<usize> {
        match self {
            Trainer::Dense(e) => e.resume_from_reader(reader),
            Trainer::Quantized(q) => q.resume_from_reader(reader),
        }
    }
}

/// Снапшот НАКОПЛЕННОЙ памяти: .pqw-контейнер + raw Packed4-отпечаток.
///
/// RQ10: при включённом гироскопе и живой циркуляции пишется контейнер
/// v3 (POLER_Q3): фазы Packed4 + топологическая секция J = A − Aᵀ.
/// RQ15: квантованный путь выписывает решётку бит-в-бит — нулевая
/// переквантование, чекпоинт = сама память.
/// Отпечаток --fingerprint — только фазовые блоки (сравнимость мониторинга).
///
/// Возвращает (байты контейнера, nnz фаз, support, пар гироскопа).
fn train_snapshot(
    trainer: &Trainer,
    cfg: &TrainConfig,
) -> Result<(usize, u64, usize, usize), String> {
    let mut gyro_pairs = 0usize;
    // Контейнер: плотный путь кодирует модель (v2/v3), квантованный —
    // выписывает решётку бит-в-бит (RQ15).
    let buf = match trainer {
        Trainer::Dense(engine) => {
            let model = engine.model();
            let mut buf = Vec::new();
            let rho = if cfg.decay { 0.0 } else { 1.0 };
            let mut w = PqwWriter::new(engine.d_pol())
                .map_err(|e| e.to_string())?
                .hyperparams(cfg.eta0 as f32, cfg.gamma as f32, rho, cfg.epsilon);
            let state_f32: Vec<f32> = model.iter().map(|&p| p as f32).collect();
            w.add_state(&state_f32).map_err(|e| e.to_string())?;
            // v3 с гироскопом (если циркуляция пережила ε-ворота), иначе v2.
            let gyro_data = engine
                .gyro()
                .and_then(|g| g.gyro_data(cfg.epsilon as f64, engine.d_pol()));
            match &gyro_data {
                Some(data) => {
                    gyro_pairs = data.pairs().len();
                    w.write_v3(&mut buf, data).map_err(|e| e.to_string())?;
                }
                None => {
                    w.write_packed_trits(&mut buf).map_err(|e| e.to_string())?;
                }
            }
            buf
        }
        Trainer::Quantized(q) => q.checkpoint().map_err(|e| e.to_string())?,
    };
    let phase_len = (cfg.dim as usize).div_ceil(4);
    let reader = PqwReader::from_bytes(&buf).map_err(|e| e.to_string())?;
    let nnz = reader.nnz();
    let support = match trainer {
        Trainer::Dense(engine) => engine.model_arcs().len(),
        // Решётка: union support ≡ ненулевые триты (память = контейнер).
        Trainer::Quantized(q) => q.nnz(),
    };
    if let Some(p) = &cfg.out {
        std::fs::write(p, &buf).map_err(|e| format!("--out: {e}"))?;
    }
    if let Some(p) = &cfg.fingerprint {
        // Только фазовые блоки: размер и семантика отпечатка неизменны
        // при любом формате контейнера (v2/v3).
        let end = (pqw::HEADER_SIZE + phase_len).min(buf.len());
        let payload = &buf[pqw::HEADER_SIZE.min(buf.len())..end];
        std::fs::write(p, payload).map_err(|e| format!("--fingerprint: {e}"))?;
    }
    Ok((buf.len(), nnz, support, gyro_pairs))
}

/// RQ14: mmap-разворот тритов в углы Блоха — θ = arccos(p) на лету.
///
/// Стриминг Packed4 → углы: LUT из трёх констант (θ ∈ {0, π/2, π}),
/// полный вектор углов не материализуется.
fn cmd_bloch(args: &[String]) -> i32 {
    let mut file: Option<String> = None;
    let mut json = false;
    let mut head: usize = 8; // превью первых углов
    let mut window: usize = 4096; // окно стримера
    let mut i = 0usize;
    macro_rules! val {
        ($name:literal) => {{
            i += 1;
            if i >= args.len() {
                eprintln!("pqc bloch: {} требует значение\n\n{USAGE}", $name);
                return 2;
            }
            args[i].clone()
        }};
    }
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--head" => {
                let v = val!("--head");
                match v.parse::<usize>() {
                    Ok(n) => head = n,
                    Err(_) => {
                        eprintln!("pqc bloch: bad --head: {v}\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            "--window" => {
                let v = val!("--window");
                match v.parse::<usize>() {
                    Ok(n) if n > 0 => window = n,
                    _ => {
                        eprintln!("pqc bloch: bad --window: {v} (> 0)\n\n{USAGE}");
                        return 2;
                    }
                }
            }
            other => {
                if other.starts_with("--") {
                    eprintln!("pqc bloch: неизвестный флаг {other}\n\n{USAGE}");
                    return 2;
                }
                if file.is_some() {
                    eprintln!("pqc bloch: один файл за вызов\n\n{USAGE}");
                    return 2;
                }
                file = Some(other.to_string());
            }
        }
        i += 1;
    }
    let Some(path) = file else {
        eprintln!("pqc bloch: нужен контейнер v2 (.pqw)\n\n{USAGE}");
        return 2;
    };
    let src = match load(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("pqc bloch: {path}: {e}");
            return 1;
        }
    };
    let reader = match PqwReader::from_bytes(src.as_slice()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("pqc bloch: {path}: {e}");
            return 1;
        }
    };
    let counts = match reader.bloch_counts() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pqc bloch: контейнер не Packed4 (v2): {e}");
            return 1;
        }
    };
    let d = reader.d_pol() as usize;

    // Полный стриминговый проход по углам (микробенчмарк на лету).
    let t0 = std::time::Instant::now();
    let mut stream = bloch_stream::Packed4Angles::new(reader.phase_bytes(), d, window);
    let mut theta_sum = 0.0_f64;
    while let Some((_, w)) = stream.next_chunk() {
        for &t in w {
            theta_sum += t;
        }
    }
    let elapsed = t0.elapsed();
    let per_trit_ns = elapsed.as_nanos() as f64 / d.max(1) as f64;

    let preview: Vec<(u32, f64)> = reader
        .bloch_angles()
        .unwrap()
        .take(head)
        .collect::<Vec<_>>();

    if json {
        let report = Json::Obj(vec![
            (
                "cmd".into(),
                Json::Str("bloch".into()),
            ),
            ("file".into(), Json::Str(path.clone())),
            ("d_pol".into(), Json::Num(d as f64)),
            (
                "encoding".into(),
                Json::Str(reader.encoding().name().into()),
            ),
            ("pos".into(), Json::Num(counts.pos as f64)),
            ("neg".into(), Json::Num(counts.neg as f64)),
            ("zero".into(), Json::Num(counts.zero as f64)),
            ("density".into(), Json::Num(counts.density())),
            ("balance".into(), Json::Num(counts.balance())),
            (
                "theta_head".into(),
                Json::Arr(
                    preview
                        .iter()
                        .map(|(i, t)| Json::Arr(vec![Json::Num(*i as f64), Json::Num(*t)]))
                        .collect(),
                ),
            ),
            (
                "theta_mean".into(),
                Json::Num(theta_sum / d.max(1) as f64),
            ),
            (
                "stream_ns_per_trit".into(),
                Json::Num(per_trit_ns),
            ),
            (
                "window".into(),
                Json::Num(window as f64),
            ),
        ]);
        println!("{}", report.to_string());
        return 0;
    }

    println!("RQ14: разворот тритов в углы Блоха — mmap, θ = arccos(p) на лету");
    println!("  файл      : {path} ({})", src.kind());
    println!("  d_pol     : {d} тритов, кодировка {}", reader.encoding().name());
    println!(
        "  решётка   : +1 × {}, −1 × {}, 0 × {}  (плотность LENS {:.4}, баланс {:.4})",
        counts.pos,
        counts.neg,
        counts.zero,
        counts.density(),
        counts.balance()
    );
    println!(
        "  стриминг  : окно {window}, {:.2} нс/трит (полный проход, без материализации)",
        per_trit_ns
    );
    println!("  средний θ : {:.6} рад", theta_sum / d.max(1) as f64);
    if !preview.is_empty() {
        let s = preview
            .iter()
            .map(|(i, t)| format!("#{i}: {:.4}", t))
            .collect::<Vec<_>>()
            .join(", ");
        println!("  первые углы: {s}");
    }
    0
}

fn cmd_train(args: &[String]) -> i32 {
    let mut cfg = TrainConfig::default();
    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].clone();
        let mut val = |name: &str| -> Result<String, String> {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| format!("missing value for {name}"))
        };
        match a.as_str() {
            "--corpus" => match val("--corpus") {
                Ok(v) => cfg.corpus = Some(v),
                Err(e) => return train_usage_err(&e),
            },
            "--stdin" => cfg.stdin = true,
            "--dim" => match val("--dim").and_then(|v| parse_num::<u32>(&v, "--dim")) {
                Ok(v) => cfg.dim = v,
                Err(e) => return train_usage_err(&e),
            },
            "--epsilon" => match val("--epsilon")
                .and_then(|v| v.parse::<f32>().map_err(|_| "bad --epsilon".to_string()))
            {
                Ok(v) => cfg.epsilon = v,
                Err(_) => return train_usage_err("bad --epsilon"),
            },
            "--block" => match val("--block").and_then(|v| parse_num::<usize>(&v, "--block")) {
                Ok(v) => cfg.block = v,
                Err(e) => return train_usage_err(&e),
            },
            "--shots" => match val("--shots").and_then(|v| parse_num::<u64>(&v, "--shots")) {
                Ok(v) => cfg.shots = v,
                Err(e) => return train_usage_err(&e),
            },
            "--steps" => match val("--steps").and_then(|v| parse_num::<usize>(&v, "--steps")) {
                Ok(v) => cfg.steps = v,
                Err(e) => return train_usage_err(&e),
            },
            "--seed" => match val("--seed").and_then(|v| parse_num::<u64>(&v, "--seed")) {
                Ok(v) => cfg.seed = v,
                Err(e) => return train_usage_err(&e),
            },
            "--eta0" => match val("--eta0")
                .and_then(|v| v.parse::<f64>().map_err(|_| "bad --eta0".to_string()))
            {
                Ok(v) => {
                    cfg.eta0 = v;
                    cfg.eta0_explicit = true;
                }
                Err(_) => return train_usage_err("bad --eta0"),
            },
            "--beta" => match val("--beta")
                .and_then(|v| v.parse::<f64>().map_err(|_| "bad --beta".to_string()))
            {
                Ok(v) => cfg.beta = v,
                Err(_) => return train_usage_err("bad --beta"),
            },
            "--gamma" => match val("--gamma")
                .and_then(|v| v.parse::<f64>().map_err(|_| "bad --gamma".to_string()))
            {
                Ok(v) => cfg.gamma = v,
                Err(_) => return train_usage_err("bad --gamma"),
            },
            "--decay" => cfg.decay = true,
            "--quantized" => cfg.quantized = true,
            "--out" => match val("--out") {
                Ok(v) => cfg.out = Some(v),
                Err(e) => return train_usage_err(&e),
            },
            "--fingerprint" => match val("--fingerprint") {
                Ok(v) => cfg.fingerprint = Some(v),
                Err(e) => return train_usage_err(&e),
            },
            "--snapshot-every" => {
                match val("--snapshot-every")
                    .and_then(|v| parse_num::<usize>(&v, "--snapshot-every"))
                {
                    Ok(v) => cfg.snapshot_every = v.max(1),
                    Err(e) => return train_usage_err(&e),
                }
            }
            "--max-bytes" => {
                match val("--max-bytes").and_then(|v| parse_num::<u64>(&v, "--max-bytes")) {
                    Ok(v) => cfg.max_bytes = v,
                    Err(e) => return train_usage_err(&e),
                }
            }
            "--log" => match val("--log") {
                Ok(v) => cfg.log = Some(v),
                Err(e) => return train_usage_err(&e),
            },
            "--every" => match val("--every").and_then(|v| parse_num::<usize>(&v, "--every")) {
                Ok(v) => cfg.every = v.max(1),
                Err(e) => return train_usage_err(&e),
            },
            "--json" => cfg.json = true,
            "--resume" => match val("--resume") {
                Ok(v) => cfg.resume = Some(v),
                Err(e) => return train_usage_err(&e),
            },
            "--curriculum" => {
                // Значение опционально: следующий аргумент без "--" — расписание,
                // иначе расписание по умолчанию (пустой вектор — маркер,
                // разворачивается после парсинга, когда известны --block/--steps).
                let next = args.get(i + 1).map(|s| s.as_str());
                match next {
                    Some(s) if !s.starts_with("--") => {
                        i += 1;
                        match parse_curriculum(s) {
                            Ok(st) => cfg.curriculum = Some(st),
                            Err(e) => return train_usage_err(&e),
                        }
                    }
                    _ => cfg.curriculum = Some(Vec::new()),
                }
            }
            "--gyro" => {
                // RQ10: окно направленного контекста (default 256);
                // значение опционально — как у --curriculum.
                let next = args.get(i + 1).map(|s| s.as_str());
                match next {
                    Some(s) if !s.starts_with("--") => match s.parse::<usize>() {
                        Ok(w) if w > 0 => {
                            i += 1;
                            cfg.gyro = Some(w);
                        }
                        _ => {
                            return train_usage_err("--gyro: окно должно быть > 0")
                        }
                    },
                    _ => cfg.gyro = Some(256),
                }
            }
            "--gyro-budget" => {
                match val("--gyro-budget")
                    .and_then(|v| parse_num::<usize>(&v, "--gyro-budget"))
                {
                    Ok(v) if v >= 16 => cfg.gyro_budget = v,
                    _ => return train_usage_err("--gyro-budget должен быть ≥ 16"),
                }
            }
            other => return train_usage_err(&format!("unknown option {other}")),
        }
        i += 1;
    }

    if cfg.corpus.is_none() && !cfg.stdin {
        return train_usage_err("укажите --corpus DIR или --stdin");
    }
    if cfg.dim == 0 || cfg.dim > 1 << 20 {
        return train_usage_err("--dim должен быть в [1, 1048576]");
    }
    if !(0.0..=1.0).contains(&cfg.epsilon) || !cfg.epsilon.is_finite() {
        return train_usage_err("--epsilon должен быть в [0, 1]");
    }
    if cfg.block == 0 {
        return train_usage_err("--block должен быть > 0");
    }
    // RQ15: квантованный curriculum — честные ограничения v1.
    if cfg.quantized && cfg.gyro.is_some() {
        return train_usage_err(
            "--quantized несовместим с --gyro (v1: решётка хранит только фазы)",
        );
    }
    if cfg.quantized && cfg.decay {
        return train_usage_err(
            "--quantized несовместим с --decay (v1: политика Hold — фон заморожен)",
        );
    }

    // Расписание фазовой сборки: маркер по умолчанию разворачивается здесь,
    // когда --block/--steps/--shots уже известны. Без --curriculum —
    // одиночный проход классическими параметрами.
    if let Some(stages) = &cfg.curriculum {
        if stages.is_empty() {
            cfg.curriculum = Some(default_curriculum(&cfg));
        }
    }
    let stages: Vec<TrainStage> = cfg.curriculum.clone().unwrap_or_else(|| {
        vec![TrainStage {
            block: cfg.block,
            steps: cfg.steps,
            shots: cfg.shots,
            budget: 0,
        }]
    });

    // Корпус: каталог (рекурсивно) или stdin.
    let mut log_file = cfg.log.as_ref().and_then(|p| {
        std::fs::File::create(p)
            .map(|mut f| {
                use std::io::Write;
                let title = if cfg.quantized {
                    "КВАНТОВАННЫЙ CURRICULUM (RQ15): born-шаг в решётке тритов"
                } else {
                    "ПЛОТНОЕ LENS-ОБУЧЕНИЕ"
                };
                let _ = writeln!(
                    f,
                    "=== {title} (eps={}, block={} B, d_pol={}) ===",
                    cfg.epsilon, cfg.block, cfg.dim
                );
                f
            })
            .ok()
    });

    let mut files: Vec<std::path::PathBuf> = Vec::new();
    let mut corpus_bytes: u64 = 0;
    if let Some(root) = &cfg.corpus {
        let mut total = 0u64;
        collect_corpus(Path::new(root), &mut files, &mut total, cfg.max_bytes);
        corpus_bytes = total;
        if files.is_empty() {
            eprintln!("pqc train: в {root} не найдено текстовых файлов");
            return 1;
        }
    }

    // Движок ОДИН на весь корпус: память накапливается между блоками.
    // RQ15: --quantized — born-шаг прямо в 2-битной решётке (η = --eta0
    // напрямую; β не используется — расписание сюрприза остаётся у плотного).
    use pqc::stream_engine::{Forget, StreamEngine};
    let mut trainer = if cfg.quantized {
        match pqc::qcurriculum::QuantizedCurriculum::new(cfg.dim, cfg.epsilon, cfg.seed) {
            Ok(mut q) => {
                // Калибровка: плотный дефолт η₀ = 0.25 НЕ пересекает порог
                // кристаллизации решётки (η·v* = 0.5 < π/6) — квантованный
                // путь берёт η = 0.6 из RQ14, если --eta0 не задан явно.
                let eta = if cfg.eta0_explicit {
                    cfg.eta0
                } else {
                    0.6
                };
                q.set_shots(cfg.shots).set_hyper(eta, cfg.gamma);
                Trainer::Quantized(q)
            }
            Err(e) => {
                eprintln!("pqc train: {e}");
                return 1;
            }
        }
    } else {
        let forget = if cfg.decay {
            Forget::Decay
        } else {
            Forget::Hold
        };
        match StreamEngine::new(cfg.dim, cfg.epsilon, cfg.seed) {
            Ok(e) => {
                // RQ10: гироскоп J = A − Aᵀ — только плотный путь
                // (валидация выше уже отклонила --quantized --gyro).
                let e = if let Some(window) = cfg.gyro {
                    e.with_gyro(window, cfg.gyro_budget)
                } else {
                    e
                };
                Trainer::Dense(
                    e.with_shots(cfg.shots)
                        .with_hyper(cfg.eta0, cfg.beta, cfg.gamma)
                        .with_forget(forget),
                )
            }
            Err(e) => {
                eprintln!("pqc train: {e}");
                return 1;
            }
        }
    };

    // RQ9: resume — поднять накопленную память из .pqw-чекпоинта.
    let mut resumed_nnz: Option<usize> = None;
    if let Some(path) = &cfg.resume {
        let raw = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("pqc train: --resume: ошибка чтения {path}: {e}");
                return 1;
            }
        };
        let reader = match pqw::PqwReader::from_bytes(&raw) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("pqc train: --resume: ошибка парсинга {path}: {e}");
                return 1;
            }
        };
        if reader.d_pol() != cfg.dim {
            eprintln!(
                "pqc train: --resume: d_pol контейнера {} ≠ --dim {}",
                reader.d_pol(),
                cfg.dim
            );
            return 1;
        }
        match trainer.resume(&reader) {
            Ok(n) => resumed_nnz = Some(n),
            Err(e) => {
                eprintln!("pqc train: --resume: {e}");
                return 1;
            }
        }
    }

    if !cfg.json {
        let src = if let Some(root) = &cfg.corpus {
            format!(
                "{root} ({} файлов, {:.1} МиБ)",
                files.len(),
                corpus_bytes as f64 / 1048576.0
            )
        } else {
            "stdin".to_string()
        };
        println!("POLER Quantum Core — {}", if cfg.quantized {
            "Quantized Born Trainer (RQ15): решётка тритов, born-шаг в 2-битных регистрах"
        } else {
            "Dense LENS Trainer (RQ8/RQ9)"
        });
        println!("corpus    : {src}");
        if cfg.quantized {
            println!(
                "lattice   : d_pol={} тритов × 2 бита = {} Б Packed4 (память = контейнер v2)",
                cfg.dim,
                (cfg.dim as usize).div_ceil(4)
            );
            println!(
                "physics   : η={}, γ={}, гистерезис π/3, полюса детерминированы{}",
                if cfg.eta0_explicit { cfg.eta0 } else { 0.6 },
                cfg.gamma,
                if cfg.eta0_explicit { "" } else { " (η — калибровка RQ14)" }
            );
        }
        if stages.len() > 1 {
            println!(
                "curriculum: {} уровней фазовой сборки — чанк растёт, память одна",
                stages.len()
            );
        } else {
            println!(
                "engine    : d_pol={}, eps={}, block={} B, shots={}, steps={}, seed={}",
                cfg.dim, cfg.epsilon, cfg.block, cfg.shots, cfg.steps, cfg.seed
            );
        }
        if let Some(n) = resumed_nnz {
            println!(
                "resumed   : {} ({} дуг поднято из чекпоинта)",
                cfg.resume.as_deref().unwrap_or("?"),
                n
            );
        }
        if let Some(w) = cfg.gyro {
            println!(
                "gyro      : J = A − Aᵀ, окно {w} токенов, бюджет {} пар — топологическая секция v3",
                cfg.gyro_budget
            );
        }
        if !cfg.quantized {
            println!(
                "policy    : forget={:?}, snapshot every {} блоков",
                if cfg.decay { Forget::Decay } else { Forget::Hold },
                cfg.snapshot_every
            );
        } else {
            println!("policy    : Hold (фон заморожен), snapshot every {} блоков", cfg.snapshot_every);
        }
    }

    let t0 = std::time::Instant::now();
    // Счётчики цикла обучения (владеет замыкание ниже).
    let mut st = TrainStats {
        total_tokens: 0,
        total_blocks: 0,
        no_hits: 0,
        last_loss: 0.0,
        last_qcm_gap: 0.0,
        blocks_since_snapshot: 0,
        snapshot: (0usize, 0u64, 0usize, 0usize),
        moved_total: 0,
        last_moved_frac: 0.0,
    };

    // Начальный снапшот: нулевой (или поднятый из чекпоинта) отпечаток.
    match train_snapshot(&trainer, &cfg) {
        Ok(s) => st.snapshot = s,
        Err(e) => {
            eprintln!("pqc train: {e}");
            return 1;
        }
    }

    // stdin читается ОДИН раз до цикла уровней: уровни берут префиксы буфера.
    let stdin_buf: Vec<u8> = if cfg.stdin {
        use std::io::Read;
        let mut buf = Vec::new();
        if let Err(e) = std::io::stdin().read_to_end(&mut buf) {
            eprintln!("pqc train: stdin: {e}");
            return 1;
        }
        buf
    } else {
        Vec::new()
    };

    let feed_block = |data: &[u8],
                      trainer: &mut Trainer,
                      st: &mut TrainStats,
                      steps: usize|
     -> Result<(), String> {
        let text = String::from_utf8_lossy(data);
        let rep = trainer.feed(&text, steps).map_err(|e| e.to_string())?;
        st.total_tokens += rep.tokens as u64;
        st.total_blocks += 1;
        if rep.no_hits {
            st.no_hits += 1;
        } else {
            st.last_loss = rep.param_loss;
            st.last_qcm_gap = rep.qcm_gap;
            st.moved_total += rep.moved;
            st.last_moved_frac = rep.moved_frac;
        }
        st.blocks_since_snapshot += 1;
        if st.blocks_since_snapshot >= cfg.snapshot_every {
            st.snapshot = train_snapshot(trainer, &cfg)?;
            st.blocks_since_snapshot = 0;
        }
        Ok(())
    };

    // Статистика уровней фазовой сборки (RQ9).
    let mut stage_stats: Vec<StageStats> = Vec::new();

    for (li, stage) in stages.iter().enumerate() {
        let level = li + 1;
        // Пересборка ученика под бюджет выстрелов уровня (детерминизм).
        trainer.set_shots(stage.shots);

        // Снапшот на входе в уровень: отпечаток фиксирует границу перехода.
        match train_snapshot(&trainer, &cfg) {
            Ok(s) => st.snapshot = s,
            Err(e) => {
                eprintln!("pqc train: {e}");
                return 1;
            }
        }
        let (nnz_before, support_before) = (st.snapshot.1, st.snapshot.2);
        let (blocks_before, tokens_before) = (st.total_blocks, st.total_tokens);
        let t_stage = std::time::Instant::now();

        if !cfg.json && stages.len() > 1 {
            let budget = if stage.budget == 0 {
                "весь корпус".to_string()
            } else {
                format!("{:.0} КиБ", stage.budget as f64 / 1024.0)
            };
            println!(
                "── УРОВЕНЬ {}/{} «{}»: чанк {} B × steps={} × shots={}, бюджет {} ──",
                level,
                stages.len(),
                stage_name(level, stages.len()),
                stage.block,
                stage.steps,
                stage.shots,
                budget
            );
        }

        if cfg.stdin {
            let input: &[u8] = if stage.budget > 0 {
                &stdin_buf[..stdin_buf.len().min(stage.budget as usize)]
            } else {
                &stdin_buf
            };
            for chunk in input.chunks(stage.block) {
                if let Err(e) = feed_block(chunk, &mut trainer, &mut st, stage.steps) {
                    eprintln!("pqc train: {e}");
                    return 1;
                }
            }
        } else {
            // Префикс корпуса под бюджет уровня (0 = весь корпус).
            // Бюджет обрезает ПОТОК БАЙТ, а не список файлов: один крупный
            // том не должен раздувать уровень (файл может быть больше
            // бюджета — тогда уровень видит лишь его префикс).
            let root = cfg.corpus.as_deref().unwrap_or(".");
            let stage_files: Vec<std::path::PathBuf> = if stage.budget > 0 {
                let mut f = Vec::new();
                let mut t = 0u64;
                collect_corpus(Path::new(root), &mut f, &mut t, stage.budget);
                f
            } else {
                files.clone()
            };
            let mut fed: u64 = 0;
            'files: for (fi, path) in stage_files.iter().enumerate() {
                let data = match std::fs::read(path) {
                    Ok(d) => d,
                    Err(_) => continue,
                };
                let take: usize = if stage.budget > 0 {
                    let remaining = (stage.budget.saturating_sub(fed)) as usize;
                    if remaining == 0 {
                        break 'files;
                    }
                    data.len().min(remaining)
                } else {
                    data.len()
                };
                fed += take as u64;
                let tokens_before_file = st.total_tokens;
                let mut file_blocks: u64 = 0;
                for chunk in data[..take].chunks(stage.block) {
                    if let Err(e) = feed_block(chunk, &mut trainer, &mut st, stage.steps) {
                        eprintln!("pqc train: {e}");
                        return 1;
                    }
                    file_blocks += 1;
                }
                let file_tokens = st.total_tokens - tokens_before_file;
                if let Some(f) = log_file.as_mut() {
                    use std::io::Write;
                    let _ = writeln!(
                        f,
                        "[L{}/{} {}/{}] {:<36} | tokens={:<6} blocks={:<3} | loss={:.6} nnz_lens={} support={}",
                        level,
                        stages.len(),
                        fi + 1,
                        stage_files.len(),
                        path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
                        file_tokens,
                        file_blocks,
                        st.last_loss,
                        st.snapshot.1,
                        st.snapshot.2
                    );
                }
                if !cfg.json && (fi + 1) % cfg.every == 0 {
                    let el = t_stage.elapsed().as_secs_f64();
                    println!(
                        "  [{:>4}/{}] {:<5.1}s | tokens={:<8} nnz_lens={:<5} support={:<5} loss={:.6}",
                        fi + 1,
                        stage_files.len(),
                        el,
                        st.total_tokens,
                        st.snapshot.1,
                        st.snapshot.2,
                        st.last_loss
                    );
                }
                if stage.budget > 0 && fed >= stage.budget {
                    break 'files;
                }
            }
        }

        // Снапшот на выходе уровня: отпечаток = завершённый уровень.
        match train_snapshot(&trainer, &cfg) {
            Ok(s) => st.snapshot = s,
            Err(e) => {
                eprintln!("pqc train: {e}");
                return 1;
            }
        }
        st.blocks_since_snapshot = 0;
        let stats = StageStats {
            level,
            name: stage_name(level, stages.len()),
            block: stage.block,
            steps: stage.steps,
            shots: stage.shots,
            budget: stage.budget,
            blocks: st.total_blocks - blocks_before,
            tokens: st.total_tokens - tokens_before,
            nnz_before,
            nnz_after: st.snapshot.1,
            support_before,
            support_after: st.snapshot.2,
            elapsed: t_stage.elapsed().as_secs_f64(),
        };
        if !cfg.json && stages.len() > 1 {
            println!(
                "  L{}: blocks={} tokens={} nnz {}→{} support {}→{} ({:.1}s)",
                stats.level,
                stats.blocks,
                stats.tokens,
                stats.nnz_before,
                stats.nnz_after,
                stats.support_before,
                stats.support_after,
                stats.elapsed
            );
        }
        stage_stats.push(stats);
    }

    // Финальный снапшот (гарантированно свежий).
    match train_snapshot(&trainer, &cfg) {
        Ok(s) => st.snapshot = s,
        Err(e) => {
            eprintln!("pqc train: {e}");
            return 1;
        }
    }
    let elapsed = t0.elapsed().as_secs_f64();
    let (container_bytes, nnz_lens, support, gyro_pairs) = st.snapshot;
    let density = nnz_lens as f64 / cfg.dim as f64 * 100.0;
    let total_tokens = st.total_tokens;
    let total_blocks = st.total_blocks;
    let no_hits = st.no_hits;
    let last_loss = st.last_loss;
    let last_qcm_gap = st.last_qcm_gap;

    if let Some(f) = log_file.as_mut() {
        use std::io::Write;
        let _ = writeln!(
            f,
            "=== ИТОГ: blocks={} tokens={} nnz_lens={} support={} density={:.2}% moved={} elapsed={:.1}s ===",
            total_blocks, total_tokens, nnz_lens, support, density, st.moved_total, elapsed
        );
    }

    if cfg.json {
        use pqc::json::Json;
        let mut pairs: Vec<(String, Json)> = vec![
            ("command".into(), Json::str("train")),
            (
                "mode".into(),
                Json::str(if cfg.quantized {
                    "quantized"
                } else {
                    "dense"
                }),
            ),
            ("d_pol".into(), Json::num(cfg.dim as f64)),
            ("epsilon".into(), Json::num(cfg.epsilon as f64)),
            ("block".into(), Json::num(cfg.block as f64)),
            ("blocks".into(), Json::num(total_blocks as f64)),
            ("tokens".into(), Json::num(total_tokens as f64)),
            ("files".into(), Json::num(files.len() as f64)),
            ("no_hits".into(), Json::num(no_hits as f64)),
            ("nnz_lens".into(), Json::num(nnz_lens as f64)),
            ("support".into(), Json::num(support as f64)),
            (
                "density_pct".into(),
                Json::num((density * 100.0).round() / 100.0),
            ),
            (
                "final_loss".into(),
                Json::num((last_loss * 1e8).round() / 1e8),
            ),
            (
                "qcm_gap".into(),
                Json::num((last_qcm_gap * 1e8).round() / 1e8),
            ),
            ("container_bytes".into(), Json::num(container_bytes as f64)),
            (
                "elapsed_sec".into(),
                Json::num((elapsed * 100.0).round() / 100.0),
            ),
        ];
        // RQ15: телеметрия квантованной решётки — «обучение произошло»
        // в числах: перещёлкивания тритов и живой момент.
        if cfg.quantized {
            pairs.push(("moved_total".into(), Json::num(st.moved_total as f64)));
            pairs.push((
                "moved_frac".into(),
                Json::num((st.last_moved_frac * 1e6).round() / 1e6),
            ));
            pairs.push((
                "lattice_bytes".into(),
                Json::num((cfg.dim as usize).div_ceil(4) as f64),
            ));
        }
        // RQ10: гироскоп J = A − Aᵀ — статистика реляционной памяти
        // (только плотный путь; --quantized --gyro отклонён валидацией).
        if let (Some(window), Trainer::Dense(engine)) = (cfg.gyro, &trainer) {
            if let Some(g) = engine.gyro() {
                let section_bytes = container_bytes
                    .saturating_sub(pqw::HEADER_SIZE + (cfg.dim as usize).div_ceil(4));
                let lambda_top = g
                    .resonant_modes(cfg.epsilon as f64, 1)
                    .first()
                    .map(|m| m.lambda)
                    .unwrap_or(0.0);
                pairs.push((
                    "gyro".into(),
                    Json::Obj(vec![
                        ("window".into(), Json::num(window as f64)),
                        ("budget".into(), Json::num(cfg.gyro_budget as f64)),
                        ("ticks".into(), Json::num(g.ticks() as f64)),
                        ("pairs_raw".into(), Json::num(g.raw_pairs() as f64)),
                        ("pairs_stored".into(), Json::num(gyro_pairs as f64)),
                        ("section_bytes".into(), Json::num(section_bytes as f64)),
                        (
                            "lambda_top".into(),
                            Json::num((lambda_top * 1e6).round() / 1e6),
                        ),
                    ]),
                ));
            }
        }
        if let Some(p) = &cfg.resume {
            pairs.push(("resume_path".into(), Json::str(p.clone())));
            if let Some(n) = resumed_nnz {
                pairs.push(("resume_nnz".into(), Json::num(n as f64)));
            }
        }
        if stages.len() > 1 {
            let arr: Vec<Json> = stage_stats
                .iter()
                .map(|s| {
                    Json::Obj(vec![
                        ("level".into(), Json::num(s.level as f64)),
                        ("name".into(), Json::str(s.name)),
                        ("block".into(), Json::num(s.block as f64)),
                        ("steps".into(), Json::num(s.steps as f64)),
                        ("shots".into(), Json::num(s.shots as f64)),
                        ("budget_bytes".into(), Json::num(s.budget as f64)),
                        ("blocks".into(), Json::num(s.blocks as f64)),
                        ("tokens".into(), Json::num(s.tokens as f64)),
                        ("nnz_before".into(), Json::num(s.nnz_before as f64)),
                        ("nnz_after".into(), Json::num(s.nnz_after as f64)),
                        ("support_before".into(), Json::num(s.support_before as f64)),
                        ("support_after".into(), Json::num(s.support_after as f64)),
                        (
                            "elapsed_sec".into(),
                            Json::num((s.elapsed * 100.0).round() / 100.0),
                        ),
                    ])
                })
                .collect();
            pairs.push(("curriculum".into(), Json::Arr(arr)));
        }
        let j = Json::Obj(pairs);
        println!("{}", j.to_string());
    } else {
        println!("------------------------------------------------------------");
        if stages.len() > 1 {
            println!("фазовая сборка (память одна, чанк растёт):");
            for s in &stage_stats {
                println!(
                    "  L{} {:<38} чанк {:>5} B | nnz {:>4}→{:<4} | support {:>4}→{:<4} | {:>6.1}s",
                    s.level,
                    format!("«{}»", s.name),
                    s.block,
                    s.nnz_before,
                    s.nnz_after,
                    s.support_before,
                    s.support_after,
                    s.elapsed
                );
            }
            println!("------------------------------------------------------------");
        }
        println!(
            "learned   : nnz_lens={} дуг (union support={}), плотность {:.2}% от d_pol={}",
            nnz_lens, support, density, cfg.dim
        );
        if cfg.quantized {
            println!(
                "stream    : {} блоков, {} токенов, no_hits={}, loss={:.6}, перещёлкнуто тритов={} (доля {:.4})",
                total_blocks, total_tokens, no_hits, last_loss, st.moved_total, st.last_moved_frac
            );
        } else {
            println!(
                "stream    : {} блоков, {} токенов, no_hits={}, loss={:.6}, QCM gap={:.6}",
                total_blocks, total_tokens, no_hits, last_loss, last_qcm_gap
            );
        }
        if let Some(p) = &cfg.out {
            let fmt = if gyro_pairs > 0 {
                "POLER_Q3 Packed4 + гироскоп J = A − Aᵀ"
            } else {
                "POLER_Q2 Packed4"
            };
            println!("checkpoint: {p} ({container_bytes} B, {fmt})");
        }
        if let (Some(_), Trainer::Dense(engine)) = (cfg.gyro, &trainer) {
            if let Some(g) = engine.gyro() {
                let section_bytes = container_bytes
                    .saturating_sub(pqw::HEADER_SIZE + (cfg.dim as usize).div_ceil(4));
                println!(
                    "gyro      : {} тактов, {} сырых пар → {} хранимых, секция {} B ({} Б/пара)",
                    g.ticks(),
                    g.raw_pairs(),
                    gyro_pairs,
                    section_bytes,
                    if gyro_pairs > 0 {
                        section_bytes / gyro_pairs
                    } else {
                        0
                    }
                );
                let lambda_top = g
                    .resonant_modes(cfg.epsilon as f64, 1)
                    .first()
                    .map(|m| m.lambda)
                    .unwrap_or(0.0);
                if lambda_top > 0.0 {
                    println!(
                        "моды Im(P): λ_max = {lambda_top:.4} — главная плоскость вращения смысла"
                    );
                }
            }
        }
        if let Some(p) = &cfg.fingerprint {
            let bytes = cfg.dim as usize / 4;
            println!("fingerprint: {p} ({bytes} B raw Packed4 — снимок фазовой памяти)");
        }
        println!(
            "elapsed   : {:.1}s ({:.1} блоков/с)",
            elapsed,
            total_blocks as f64 / elapsed.max(1e-9)
        );
    }
    0
}
