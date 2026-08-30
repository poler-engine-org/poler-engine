//! # Terminal Gateway: двойной контур исполнения (v0.22.0)
//!
//! **Контур 1 (Engine Native, приоритет):** команда движка исполняется
//! внутри процесса — без спавна внешних шеллов. Существующие команды
//! внутреннего REPL делегируются в `shell::commands::dispatch` (один
//! источник правды), новые (grep/chunk/benchmark/service/…) реализованы
//! здесь поверх библиотечных API.
//!
//! **Контур 2 (Controlled Host OS Proxy):** всё прочее исполняется в
//! хостовой ОС через Sandboxed OS Subshell (sandbox::judge → hostexec::run)
//! — с блокировкой деструктивного, подтверждением эскалаций, капом вывода
//! и таймаутом.
//!
//! Конвейеры смешивают контуры: `ls -la | chunk` (host→engine),
//! `grep "fn " --stdin | wc -l` (engine→host).

use super::hostexec::{self, EnvMode, HostLimits};
use super::pipeline::{parse_line, Pipeline, Segment};
use super::sandbox::{self, Policy};
use super::service;
use crate::shell::{self, ShellState};
use std::io::Write;
use std::path::PathBuf;

/// Ошибка команд движка: сообщение ИЛИ выход (quit из делегированного шелла).
enum EngineFail {
    Msg(String),
    Quit,
}

// ---------------------------------------------------------------------------
// Состояние
// ---------------------------------------------------------------------------

/// Состояние Terminal Gateway.
pub struct GatewayState {
    /// Внутренний шелл движка (search/nlm/notes/…) — делегация.
    pub shell: ShellState,
    /// Рабочий каталог gateway (cd/pwd; хостовые команды стартуют отсюда).
    pub cwd: PathBuf,
    /// Лимиты host-прокси (таймаут/кап/env).
    pub limits: HostLimits,
    /// Авто-подтверждение опасных команд (set autoyes on).
    pub auto_yes: bool,
    /// Код возврата последней команды (для $-статуса).
    pub last_exit: i32,
}

impl GatewayState {
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            shell: ShellState::new(db_path),
            cwd: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            limits: HostLimits::default(),
            auto_yes: false,
            last_exit: 0,
        }
    }
}

/// Результат исполнения строки.
pub enum GatewayResult {
    Done(String),
    Quit,
    Empty,
}

// ---------------------------------------------------------------------------
// Словарь команд движка (Контур 1)
// ---------------------------------------------------------------------------

/// Команды, перехватываемые движком (приоритет над PATH!). `grep` здесь —
/// POLER Native Grep, а не /bin/grep; системный — через `!grep` / `host grep`.
pub fn is_engine_command(name: &str) -> bool {
    matches!(
        name,
        "search" | "web" | "stats" | "nlm" | "sync" | "set" | "crawl" | "impact" | "gh" | "gl"
            | "gt" | "gix" | "notes" | "sources" | "grep" | "chunk" | "benchmark" | "service"
            | "attach" | "weblens" | "license" | "cd" | "pwd" | "clear" | "host" | "help"
            | "version" | "quit" | "exit" | "q" | "ver"
    )
}

/// Команды, которые осмысленно принимают stdin в конвейере.
fn accepts_stdin(cmd: &str) -> bool {
    matches!(cmd, "grep" | "chunk" | "impact")
}

// ---------------------------------------------------------------------------
// Главная точка входа
// ---------------------------------------------------------------------------

/// Исполнить строку ввода gateway. `interactive` — можно ли задавать
/// вопросы пользователю (Confirm-политика); в тестах/скриптах Confirm
/// без auto_yes = отказ.
pub fn exec_line(state: &mut GatewayState, line: &str, interactive: bool) -> GatewayResult {
    hostexec::clear_interrupt();
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return GatewayResult::Empty;
    }

    // `!cmd` — принудительный хостовый контур (обход приоритета движка)
    let effective: String = if let Some(rest) = trimmed.strip_prefix('!') {
        if rest.trim().is_empty() {
            return GatewayResult::Done("!: пустая команда".into());
        }
        format!("host {}", rest.trim())
    } else {
        trimmed.to_string()
    };

    let pipeline = match parse_line(&effective) {
        Ok(p) => p,
        Err(e) => {
            state.last_exit = 2;
            return GatewayResult::Done(format!("⚡ парсинг: {e}"));
        }
    };

    // Классификация сегментов
    enum Kind {
        Engine,
        Host,
    }
    let mut kinds: Vec<Kind> = Vec::new();
    let mut seg_tokens: Vec<Vec<String>> = Vec::new();
    for seg in &pipeline.segments {
        let tokens = super::pipeline::strip_poler_prefix(&seg.tokens, &is_engine_command);
        let cmd = tokens
            .first()
            .map(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        if cmd == "host" {
            // явное принуждение к хостовому контуру: host <cmd…>
            seg_tokens.push(tokens[1..].to_vec());
            kinds.push(Kind::Host);
            continue;
        }
        seg_tokens.push(tokens);
        if is_engine_command(&cmd) {
            kinds.push(Kind::Engine);
        } else {
            kinds.push(Kind::Host);
        }
    }

    // Sandbox: вердикт по ВСЕМ хостовым сегментам + редиректу — ДО исполнения
    let host_idx: Vec<usize> = kinds
        .iter()
        .enumerate()
        .filter(|(_, k)| matches!(k, Kind::Host))
        .map(|(i, _)| i)
        .collect();
    if !host_idx.is_empty() {
        // пересобираем pipeline с уже срезанными poler-префиксами
        let judged = rebuild_for_judge(&pipeline, &seg_tokens);
        match sandbox::judge_pipeline(&judged, &host_idx) {
            Policy::Allow => {}
            Policy::Block(why) => {
                state.last_exit = 2;
                return GatewayResult::Done(format!("⛔ блокировка sandbox: {why}"));
            }
            Policy::Confirm(why) => {
                // interactive — спросить; non-interactive — только auto_yes
                let ok = if interactive {
                    ask_confirm(&why, state.auto_yes)
                } else {
                    if !state.auto_yes {
                        eprintln!("⚠ {why} — требуется подтверждение (в скрипте: set autoyes on)");
                    }
                    state.auto_yes
                };
                if !ok {
                    state.last_exit = 125;
                    return GatewayResult::Done(format!("⛔ отклонено (не подтверждено): {why}"));
                }
            }
        }
    }

    // Исполнение конвейера слева направо
    let mut stdin_data: Option<String> = None;
    let mut final_out = String::new();
    let mut stderr_out = String::new();
    let mut exit = 0;

    for (i, kind) in kinds.iter().enumerate() {
        // прерывание между сегментами (Ctrl+C во время длинного engine-этапа)
        if hostexec::INTERRUPT.load(std::sync::atomic::Ordering::SeqCst) {
            state.last_exit = 130;
            return GatewayResult::Done("⛔ прервано пользователем (Ctrl+C)".into());
        }
        let tokens = &seg_tokens[i];
        match kind {
            Kind::Engine => {
                let cmd = tokens.first().map(|s| s.as_str()).unwrap_or("");
                if stdin_data.is_some() && !accepts_stdin(cmd) {
                    state.last_exit = 2;
                    return GatewayResult::Done(format!(
                        "⚡ команда {cmd} не принимает stdin из конвейера (принимают: grep, chunk, impact)"
                    ));
                }
                match run_engine(state, tokens, stdin_data.as_deref()) {
                    Ok(out) => {
                        final_out = out;
                        stdin_data = Some(final_out.clone());
                    }
                    Err(EngineFail::Quit) => return GatewayResult::Quit,
                    Err(EngineFail::Msg(e)) => {
                        state.last_exit = 2;
                        return GatewayResult::Done(format!("⚡ {e}"));
                    }
                }
            }
            Kind::Host => {
                let outcome =
                    hostexec::run(tokens, stdin_data.as_deref(), &state.limits, &state.cwd, &[]);
                if !outcome.stderr.is_empty() {
                    stderr_out.push_str(&outcome.stderr);
                }
                if let Some(e) = &outcome.spawn_error {
                    state.last_exit = 127;
                    return GatewayResult::Done(format!("⚡ {e}"));
                }
                exit = outcome.code.unwrap_or(if outcome.timed_out || outcome.interrupted {
                    124
                } else {
                    1
                });
                final_out = outcome.stdout.clone();
                stdin_data = Some(final_out.clone());
                // видимость кода возврата последнего сегмента (как в POSIX)
                let is_last = i + 1 == kinds.len();
                if is_last && exit != 0 && !outcome.timed_out && !outcome.interrupted {
                    stderr_out.push_str(&format!("exit-код: {exit}\n"));
                }
                if outcome.timed_out || outcome.interrupted {
                    // дальше конвейер не имеет смысла
                    let note = if outcome.timed_out {
                        "таймаут"
                    } else {
                        "прервано (Ctrl+C)"
                    };
                    state.last_exit = 124;
                    let mut msg = final_out;
                    if !msg.is_empty() {
                        msg.push('\n');
                    }
                    msg.push_str(&format!("⚠ {note}: процесс убит\n"));
                    return finish(state, msg, stderr_out, &pipeline);
                }
            }
        }
    }

    state.last_exit = exit;
    finish(state, final_out, stderr_out, &pipeline)
}

/// Финал: применяем редирект (stdout → файл) или возвращаем текст.
fn finish(
    _state: &mut GatewayState,
    out: String,
    stderr: String,
    pipeline: &Pipeline,
) -> GatewayResult {
    if let Some(r) = &pipeline.redirect {
        let path = super::sandbox::expand_home(&r.path);
        let mut text = out;
        if r.stderr {
            text = stderr;
        }
        let write_result = if r.append {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .and_then(|mut f| f.write_all(text.as_bytes()))
        } else {
            std::fs::write(&path, &text)
        };
        return match write_result {
            Ok(()) => GatewayResult::Done(format!("→ {} ({} байт)", path, text.len())),
            Err(e) => GatewayResult::Done(format!("⚡ редирект в {path}: {e}")),
        };
    }
    // stderr без редиректа показываем вместе с stdout (с меткой)
    let mut full = out;
    if !stderr.is_empty() {
        if !full.is_empty() && !full.ends_with('\n') {
            full.push('\n');
        }
        for l in stderr.lines() {
            full.push_str(&format!("⚠ stderr: {l}\n"));
        }
    }
    GatewayResult::Done(full)
}

/// Пересобрать Pipeline для sandbox-анализа (poler-префиксы срезаны).
fn rebuild_for_judge(orig: &Pipeline, seg_tokens: &[Vec<String>]) -> Pipeline {
    Pipeline {
        segments: orig
            .segments
            .iter()
            .zip(seg_tokens)
            .map(|(_s, t)| Segment {
                tokens: t.clone(),
                raw: t.join(" "),
            })
            .collect(),
        redirect: orig.redirect.clone(),
    }
}

/// Интерактивное подтверждение опасного действия.
fn ask_confirm(reason: &str, auto_yes: bool) -> bool {
    if auto_yes {
        println!("⚠ auto-yes: {reason}");
        return true;
    }
    println!("⚠ {reason}");
    print!("исполнить? [yes/No] ");
    let _ = std::io::stdout().flush();
    let mut ans = String::new();
    match std::io::stdin().read_line(&mut ans) {
        Ok(0) => false, // EOF — не подтверждено
        Ok(_) => {
            let a = ans.trim().to_lowercase();
            a == "y" || a == "yes" || a == "д" || a == "да"
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Контур 1: исполнение команд движка
// ---------------------------------------------------------------------------

fn run_engine(
    state: &mut GatewayState,
    tokens: &[String],
    stdin: Option<&str>,
) -> Result<String, EngineFail> {
    let cmd = tokens.first().map(|s| s.as_str()).unwrap_or("");
    let args = &tokens[1..];
    match cmd {
        "grep" => cmd_grep(args, stdin).map_err(EngineFail::Msg),
        "chunk" => cmd_chunk(args, stdin).map_err(EngineFail::Msg),
        "impact" => {
            if stdin.is_some() {
                cmd_impact(args, stdin).map_err(EngineFail::Msg)
            } else {
                // без stdin — анализ репозитория по cwd: делегация
                delegate_shell(state, tokens)
            }
        }
        "benchmark" => cmd_benchmark(args).map_err(EngineFail::Msg),
        "service" => cmd_service(args).map_err(EngineFail::Msg),
        "attach" => cmd_attach(args).map_err(EngineFail::Msg),
        "weblens" => cmd_weblens(args).map_err(EngineFail::Msg),
        "license" => Ok(crate::license::status_text()),
        "cd" => cmd_cd(state, args).map_err(EngineFail::Msg),
        "pwd" => Ok(format!("{}\n", state.cwd.display())),
        "clear" => Ok("\x1b[2J\x1b[1;1H".into()),
        "help" | "?" => Ok(gateway_help()),
        "version" | "ver" | "v" => Ok(format!(
            "poler-engine {} — Terminal Gateway v0.22.0 (двойной контур: engine-native + sandboxed host proxy)\n",
            env!("CARGO_PKG_VERSION")
        )),
        "quit" | "exit" | "q" => Err(EngineFail::Quit),
        "set" => {
            // gateway-настройки, иначе делегация во внутренний шелл
            if let Some(sub) = args.first() {
                match sub.as_str() {
                    "hosttimeout" => return cmd_set_hosttimeout(state, args).map_err(EngineFail::Msg),
                    "hostenv" => return cmd_set_hostenv(state, args).map_err(EngineFail::Msg),
                    "autoyes" => return cmd_set_autoyes(state, args).map_err(EngineFail::Msg),
                    _ => {}
                }
            }
            delegate_shell(state, tokens)
        }
        _ => delegate_shell(state, tokens),
    }
}

/// Делегация во внутренний шелл движка (v0.15+): search/stats/nlm/notes/
/// sources/crawl/impact/gh/gl/gt/gix/sync/set… Слова с пробелами
/// перекавычиваются — семантика аргументов сохраняется.
fn delegate_shell(state: &mut GatewayState, tokens: &[String]) -> Result<String, EngineFail> {
    let line = tokens
        .iter()
        .map(|t| {
            if t.contains(' ') {
                format!("\"{t}\"")
            } else {
                t.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    match shell::commands::dispatch(&mut state.shell, &line) {
        shell::CmdResult::Done(out) => {
            // выход внутреннего шелла без завершающего \n — нормализуем
            if out.ends_with('\n') {
                Ok(out)
            } else {
                Ok(format!("{out}\n"))
            }
        }
        shell::CmdResult::Quit => Err(EngineFail::Quit),
        shell::CmdResult::Empty => Ok(String::new()),
    }
}

// ---------------------------------------------------------------------------
// grep — POLER Native Grep (слой 0, v0.20.0) с stdin из конвейера
// ---------------------------------------------------------------------------

fn cmd_grep(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    use crate::retrieval as nr;
    let mut pattern: Option<String> = None;
    let mut mode = nr::GrepMode::Literal;
    let mut icase = false;
    let mut before = 0usize;
    let mut after = 0usize;
    let mut max_count: Option<usize> = None;
    let mut output = nr::GrepOutput::Content;
    let mut hidden = false;
    let mut json = false;
    let mut force_stdin = false;
    let mut roots: Vec<String> = Vec::new();

    let mut i = 0usize;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--regex" | "-E" => mode = nr::GrepMode::Regex,
            "-i" | "--ignore-case" | "--grep-i" => icase = true,
            "--count" | "-c" => output = nr::GrepOutput::Count,
            "--list" | "-l" => output = nr::GrepOutput::ListMatching,
            "--list-nonmatching" | "-L" => output = nr::GrepOutput::ListNonMatching,
            "--hidden" => hidden = true,
            "--json" => json = true,
            "--stdin" => force_stdin = true,
            "-A" | "--after" | "-B" | "--before" | "-m" | "--max-count" => {
                let v = args.get(i + 1).ok_or_else(|| format!("{a}: ожидается число"))?;
                let n: usize = v
                    .parse()
                    .map_err(|_| format!("{a}: не число: {v}"))?;
                match a.as_str() {
                    "-A" | "--after" => after = n,
                    "-B" | "--before" => before = n,
                    _ => max_count = Some(n),
                }
                i += 1;
            }
            _ => {
                if a.starts_with('-') && a.len() > 1 {
                    return Err(format!("grep: неизвестный флаг {a} (движковый grep; системный — !grep)"));
                }
                if pattern.is_none() {
                    pattern = Some(a.clone());
                } else {
                    roots.push(a.clone());
                }
            }
        }
        i += 1;
    }
    let pattern = pattern.ok_or("grep: укажите PATTERN (grep PATTERN [PATH…])")?;
    let config = nr::GrepConfig {
        pattern,
        mode,
        case_insensitive: icase,
        before,
        after,
        max_count,
        output,
        include_hidden: hidden,
        respect_ignore: true,
    };

    let report = if stdin.is_some() || force_stdin {
        let text = stdin.unwrap_or("");
        nr::grep_buffer("stdin", text, &config)?
    } else {
        let root_paths: Vec<PathBuf> = if roots.is_empty() {
            vec![PathBuf::from(".")]
        } else {
            roots.into_iter().map(PathBuf::from).collect()
        };
        nr::grep_run(&root_paths, &config)?
    };
    let text = if json {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    } else {
        nr::render_text(&report, false)
    };
    Ok(text)
}

// ---------------------------------------------------------------------------
// chunk — RAG-чанкер (слой B, v0.20.0) с stdin из конвейера
// ---------------------------------------------------------------------------

fn cmd_chunk(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    use crate::retrieval as nr;
    let mut path: Option<String> = None;
    let mut size = 512usize;
    let mut overlap = 64usize;
    let mut json = false;
    let mut force_stdin = false;
    let mut format: Option<nr::ChunkFormat> = None;

    let mut i = 0usize;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--size" | "-s" => {
                size = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .ok_or("--size: ожидается число")?;
                i += 1;
            }
            "--overlap" => {
                overlap = args
                    .get(i + 1)
                    .and_then(|v| v.parse().ok())
                    .ok_or("--overlap: ожидается число")?;
                i += 1;
            }
            "--json" => json = true,
            "--stdin" => force_stdin = true,
            "--format" => {
                let f = args.get(i + 1).ok_or("--format: md|plain|code")?;
                format = Some(match f.as_str() {
                    "md" | "markdown" => nr::ChunkFormat::Markdown,
                    "code" => nr::ChunkFormat::Code,
                    "plain" => nr::ChunkFormat::Plain,
                    other => return Err(format!("--format: неизвестный {other}")),
                });
                i += 1;
            }
            _ => {
                if a.starts_with('-') {
                    return Err(format!("chunk: неизвестный флаг {a}"));
                }
                if path.is_none() {
                    path = Some(a.clone());
                } else {
                    return Err("chunk: только один PATH (или stdin из конвейера)".into());
                }
            }
        }
        i += 1;
    }

    let config = nr::ChunkConfig {
        target_tokens: size,
        overlap_tokens: overlap,
        ..Default::default()
    };

    let (text, fmt, name) = if stdin.is_some() || force_stdin {
        (
            stdin.unwrap_or("").to_string(),
            format.unwrap_or(nr::ChunkFormat::Plain),
            "stdin".to_string(),
        )
    } else {
        let p = path.ok_or("chunk: укажите PATH (или подайте stdin через конвейер)")?;
        let pb = PathBuf::from(&p);
        let t = std::fs::read_to_string(&pb).map_err(|e| format!("chunk: {p}: {e}"))?;
        let f = format.unwrap_or_else(|| nr::ChunkFormat::detect(&pb));
        (t, f, p)
    };
    let report = nr::chunk_document(&text, fmt, &config);
    let out = if json {
        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
    } else {
        nr::render_chunks_text(&report, &name)
    };
    Ok(out)
}

// ---------------------------------------------------------------------------
// impact — AIDDE impact-анализ; в конвейере код приходит из stdin
// ---------------------------------------------------------------------------

fn cmd_impact(args: &[String], stdin: Option<&str>) -> Result<String, String> {
    let symbol = args
        .first()
        .ok_or("impact: укажите SYMBOL (impact SYMBOL [PATH])")?
    ;
    let depth = args
        .get(1)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(2);

    // код из конвейера: временный файл → SymbolTable → impact
    let code = stdin.ok_or("impact: нет stdin")?;
    let tmp = std::env::temp_dir().join(format!("poler-gateway-impact-{}.rs", std::process::id()));
    std::fs::write(&tmp, code).map_err(|e| format!("impact: tmp: {e}"))?;
    let files = vec![tmp.clone()];
    let table = crate::aidde::SymbolTable::build(&files, 8 * 1024 * 1024);
    let result = crate::aidde::impact_analysis(&table, symbol, depth, 200)
        .map(|r| serde_json::to_string_pretty(&r).unwrap_or_default());
    let _ = std::fs::remove_file(&tmp);
    result.ok_or_else(|| format!("impact: символ не найден в stdin-коде: {symbol}"))
}

// ---------------------------------------------------------------------------
// benchmark — Bench Suite (v0.21.0)
// ---------------------------------------------------------------------------

fn cmd_benchmark(args: &[String]) -> Result<String, String> {
    let json_path = args
        .iter()
        .position(|a| a == "--json")
        .and_then(|i| args.get(i + 1).cloned());
    let opts = crate::bench::BenchOpts::default();
    let res = crate::bench::run_suite(&opts)?;
    if let Some(p) = json_path {
        let json = serde_json::to_string_pretty(&res).map_err(|e| e.to_string())?;
        std::fs::write(&p, json).map_err(|e| format!("benchmark-json: {p}: {e}"))?;
    }
    Ok(crate::bench::report_text(&res))
}

// ---------------------------------------------------------------------------
// service / attach / weblens — управление нижним слоем
// ---------------------------------------------------------------------------

fn cmd_service(args: &[String]) -> Result<String, String> {
    let sub = args.first().map(|s| s.as_str()).unwrap_or("status");
    match sub {
        "status" => Ok(service::status(args.get(1).map(|s| s.as_str()))),
        "start" => {
            let name = args.get(1).ok_or("service start <имя> [bind]")?;
            let bind = args.get(2).map(|s| s.as_str());
            service::start(name, bind)
        }
        "stop" => {
            let name = args.get(1).ok_or("service stop <имя>")?;
            service::stop(name)
        }
        "restart" => {
            let name = args.get(1).ok_or("service restart <имя> [bind]")?;
            let bind = args.get(2).map(|s| s.as_str());
            if service::stop(name).is_ok() {
                service::start(name, bind)
            } else {
                service::start(name, bind)
            }
        }
        "attach" => {
            let name = args.get(1).ok_or("service attach <имя>")?;
            attach_session(name)
        }
        other => Err(format!(
            "service: неизвестная подкоманда {other} (start|stop|restart|status|attach)"
        )),
    }
}

fn cmd_attach(args: &[String]) -> Result<String, String> {
    let name = args.first().map(|s| s.as_str()).unwrap_or("mcp");
    attach_session(name)
}

fn cmd_weblens(args: &[String]) -> Result<String, String> {
    match args.first().map(|s| s.as_str()).unwrap_or("start") {
        "start" => service::start("weblens", args.get(1).map(|s| s.as_str())),
        "stop" => service::stop("weblens"),
        "status" => Ok(service::status(Some("weblens"))),
        "install" => Err("weblens install: используйте `poler-engine --web-lens-install` (материализация + инструкция)".into()),
        other => Err(format!("weblens: {other}? (start|stop|status)")),
    }
}

/// Подключение к сессии сервиса: mcp → JSON-RPC REPL, прочие → tail лога.
fn attach_session(name: &str) -> Result<String, String> {
    match name {
        "mcp" => attach_mcp(),
        "weblens" | "companion" => tail_log(name),
        other => Err(format!(
            "attach: неизвестный сервис {other} ({})",
            service::service_names().join(", ")
        )),
    }
}

/// JSON-RPC REPL поверх живого MCP-сервера.
fn attach_mcp() -> Result<String, String> {
    let st_text = service::status(Some("mcp"));
    let endpoint = extract_endpoint(&st_text)
        .ok_or("attach mcp: сервис не запущен (`service start mcp`)")?;
    let token = service::service_token("mcp")?;
    println!("attach: {} (токен {})", endpoint, service::mask_token(&token));
    println!("команды: tools | call <tool> {{json}} | {{сырой JSON-RPC}} | quit");

    let stdin = std::io::stdin();
    let mut line = String::new();
    loop {
        line.clear();
        print!("mcp> ");
        let _ = std::io::stdout().flush();
        match stdin.read_line(&mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t == "quit" || t == "exit" || t == "q" {
            break;
        }
        let (method, params) = if t == "tools" {
            ("tools/list".to_string(), serde_json::json!({}))
        } else if let Some(rest) = t.strip_prefix("call ") {
            let rest = rest.trim();
            let (tool, args_str) = rest
                .split_once(char::is_whitespace)
                .unwrap_or((rest, "{}"));
            let params = serde_json::from_str(args_str.trim())
                .unwrap_or_else(|_| serde_json::json!({}));
            ("tools/call".to_string(), serde_json::json!({"name": tool, "arguments": params}))
        } else if t.starts_with('{') {
            // сырой JSON-RPC: {"method": "...", "params": {...}}
            match serde_json::from_str::<serde_json::Value>(t) {
                Ok(v) => (
                    v.get("method").and_then(|m| m.as_str()).unwrap_or("tools/list").to_string(),
                    v.get("params").cloned().unwrap_or(serde_json::json!({})),
                ),
                Err(e) => {
                    println!("⚡ JSON: {e}");
                    continue;
                }
            }
        } else {
            println!("⚡ неизвестная команда (tools | call <tool> {{json}} | {{JSON-RPC}} | quit)");
            continue;
        };
        let (code, body) = service::mcp_rpc(&endpoint, &token, &method, params.clone());
        if code == 0 {
            println!("⚡ {body}");
        } else if code == 200 || code == 405 {
            // 405: GET не нужен, POST-ответ уже в body
            println!("{body}");
        } else {
            println!("⚡ HTTP {code}: {body}");
        }
    }
    Ok("attach: отключение\n".into())
}

/// Извлечь endpoint из вывода status (мини-парсинг таблицы).
fn extract_endpoint(status_text: &str) -> Option<String> {
    // строка вида "mcp         12345    running ✓ http://127.0.0.1:8765/"
    for line in status_text.lines() {
        if line.starts_with("mcp") && line.contains("http") {
            let url = line
                .split_whitespace()
                .find(|w| w.starts_with("http"))?;
            return Some(url.to_string());
        }
    }
    None
}

/// tail -f лога сервиса до Ctrl+C / EOF.
fn tail_log(name: &str) -> Result<String, String> {
    let path = service::log_file(name);
    if !path.exists() {
        return Ok(format!(
            "attach {name}: лога ещё нет ({}) — сервис не запускался?",
            path.display()
        ));
    }
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(4096);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    print!("{}", String::from_utf8_lossy(&buf));
    println!("— tail {}: Ctrl+C/Ctrl+D для выхода —", name);
    loop {
        if hostexec::INTERRUPT.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        let mut chunk = Vec::new();
        let n = f.read_to_end(&mut chunk).unwrap_or(0);
        if n > 0 {
            print!("{}", String::from_utf8_lossy(&chunk));
            let _ = std::io::stdout().flush();
        }
    }
    hostexec::clear_interrupt();
    Ok(format!("\nattach {name}: отключение\n"))
}

// ---------------------------------------------------------------------------
// set-настройки gateway
// ---------------------------------------------------------------------------

fn cmd_set_hosttimeout(state: &mut GatewayState, args: &[String]) -> Result<String, String> {
    let v = args.get(1).ok_or("set hosttimeout <сек>")?;
    let n: u64 = v.parse().map_err(|_| format!("не число: {v}"))?;
    state.limits.timeout_secs = n;
    Ok(format!("host-таймаут: {n} с\n"))
}

fn cmd_set_hostenv(state: &mut GatewayState, args: &[String]) -> Result<String, String> {
    let v = args.get(1).map(|s| s.as_str()).ok_or("set hostenv filtered|full")?;
    state.limits.env_mode = match v {
        "filtered" => EnvMode::Filtered,
        "full" => EnvMode::Full,
        other => return Err(format!("set hostenv: {other}? (filtered|full)")),
    };
    Ok(format!(
        "host-окружение: {} (секреты {})\n",
        state.limits.env_mode,
        if state.limits.env_mode == EnvMode::Filtered {
            "вырезаются"
        } else {
            "наследуются ПОЛНОСТЬЮ"
        }
    ))
}

fn cmd_set_autoyes(state: &mut GatewayState, args: &[String]) -> Result<String, String> {
    let v = args.get(1).map(|s| s.as_str()).ok_or("set autoyes on|off")?;
    state.auto_yes = match v {
        "on" | "yes" => true,
        "off" | "no" => false,
        other => return Err(format!("set autoyes: {other}? (on|off)")),
    };
    Ok(format!("auto-подтверждение: {}\n", if state.auto_yes { "ВКЛ (осторожно!)" } else { "выкл" }))
}

fn cmd_cd(state: &mut GatewayState, args: &[String]) -> Result<String, String> {
    let target = match args.first() {
        Some(p) => super::sandbox::expand_home(p),
        None => {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            home
        }
    };
    let path = if target.starts_with('/') {
        PathBuf::from(&target)
    } else {
        state.cwd.join(&target)
    };
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("cd: {}: {e}", path.display()))?;
    if !canonical.is_dir() {
        return Err(format!("cd: {} не каталог", path.display()));
    }
    state.cwd = canonical;
    Ok(String::new())
}

// ---------------------------------------------------------------------------
// Help
// ---------------------------------------------------------------------------

pub fn gateway_help() -> String {
    let mut s = String::from(
        "POLER Terminal Gateway — единый терминальный шлюз (v0.22.0)\n\
         ═════════════════════════════════════════════════════════════\n\
         Двойной контур: команды движка исполняются нативно (приоритет),\n\
         всё остальное — хостовая ОС в sandbox-режиме.\n\n",
    );
    s.push_str("КОНТУР 1 — движок (нативно, без спавна шеллов):\n");
    s.push_str("  search «запрос» [--top N]   поиск по web-index (BM25+WebRank+мост)\n");
    s.push_str("  grep PAT [PATH…] [-E] [-i] [-A/-B N] [--count] [--json]\n");
    s.push_str("                              точный поиск (полнота = GNU grep)\n");
    s.push_str("  chunk [PATH|--stdin] [--size N] [--overlap N] [--json]\n");
    s.push_str("                              RAG-чанковка (секция→абзац→предложение)\n");
    s.push_str("  impact SYMBOL               AIDDE: подграф влияния символа\n");
    s.push_str("  crawl URL / notes / nlm / sources / gh / gl / gt / gix / stats\n");
    s.push_str("                              (делегация во внутренний шелл: help там же)\n");
    s.push_str("  benchmark [--json PATH]     бенчмарк-сьют движка\n");
    s.push_str("  license                     статус лицензии + EULA-условия\n\n");
    s.push_str("КОНТУР 2 — хост (Sandboxed OS Subshell):\n");
    s.push_str("  любая системная команда     ls, git, cargo, python…\n");
    s.push_str("  !grep / host grep           принудительно системный (а не движковый)\n");
    s.push_str("  БЛОКИРУЕТСЯ: rm -rf /, форк-бомбы, dd of=/dev/*, shutdown,\n");
    s.push_str("               curl|sh, > /dev/sd*, > /etc/*\n");
    s.push_str("  ПОДТВЕРЖДАЕТСЯ: sudo/su, rm -r, dd\n\n");
    s.push_str("КОНВЕЙЕРЫ (любые комбинации контуров):\n");
    s.push_str("  ls -la src/ | chunk --size 200\n");
    s.push_str("  cat main.rs | impact main\n");
    s.push_str("  grep \"fn \" --stdin | wc -l\n");
    s.push_str("  poler-префикс опционален: `ls | poler chunk` ≡ `ls | chunk`\n\n");
    s.push_str("СЕРВИСНЫЙ СЛОЙ (Service Substrate):\n");
    s.push_str("  service start|stop|restart|status [mcp|weblens|companion]\n");
    s.push_str("  attach [mcp|weblens|companion]   подключение к сессии\n");
    s.push_str("  weblens [start|stop|status]      сахар над service\n\n");
    s.push_str("НАСТРОЙКИ/СЛУЖЕБНЫЕ:\n");
    s.push_str("  set hosttimeout N | set hostenv filtered|full | set autoyes on|off\n");
    s.push_str("  set format|top … (внутренний шелл)   cd / pwd / clear / version\n");
    s.push_str("  quit (^D) — выход\n\n");
    s.push_str("Документация: docs/terminal-gateway-architecture.md\n");
    s
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> GatewayState {
        GatewayState::new(PathBuf::from("/tmp/poler-gw-test.db"))
    }

    fn run(state: &mut GatewayState, line: &str) -> Option<String> {
        match exec_line(state, line, false) {
            GatewayResult::Done(s) => Some(s),
            GatewayResult::Quit => None,
            GatewayResult::Empty => Some(String::new()),
        }
    }

    #[test]
    fn engine_priority_over_host_binaries() {
        // grep существует и в PATH, но движок перехватывает
        let mut st = state();
        let out = run(&mut st, "grep hello").unwrap();
        // движковый grep на пустом паттерне-каталоге — не «command not found»
        assert!(!out.contains("не удалось запустить"), "движок должен перехватить grep: {out}");
    }

    #[test]
    fn unknown_command_falls_to_host() {
        let mut st = state();
        // несуществующий бинарник → spawn_error от host-контур
        let out = run(&mut st, "zzz-not-a-command-xyz").unwrap();
        assert!(out.contains("не удалось запустить"), "должен уйти в host-контур: {out}");
    }

    #[test]
    fn forced_host_with_bang() {
        let mut st = state();
        let out = run(&mut st, "!echo forced-host").unwrap();
        assert_eq!(out.trim(), "forced-host");
    }

    #[test]
    fn host_keyword_forces_host() {
        let mut st = state();
        let out = run(&mut st, "host echo via-host-kw").unwrap();
        assert_eq!(out.trim(), "via-host-kw");
    }

    #[test]
    fn pipeline_host_to_engine_chunk() {
        let mut st = state();
        let out = run(&mut st, "echo -e \"line one\\nline two\\nline three\" | chunk --stdin --size 10").unwrap();
        assert!(out.contains("chunk") || !out.is_empty(), "чанкование stdin должно что-то вывести: {out}");
    }

    #[test]
    fn pipeline_host_to_engine_grep() {
        let mut st = state();
        let out = run(&mut st, "printf 'apple\\nbanana\\ncherry\\n' | grep ap --stdin").unwrap();
        assert!(out.contains("apple"), "grep --stdin: {out}");
        assert!(!out.contains("banana"), "banana без 'ap' не должен попасть: {out}");
        assert!(!out.contains("cherry"), "cherry без 'ap' не должен попасть: {out}");
    }

    #[test]
    fn pipeline_engine_to_host() {
        let mut st = state();
        let out = run(&mut st, "printf 'x\\ny\\n' | grep x --stdin | wc -l").unwrap();
        assert_eq!(out.trim(), "1", "engine→host pipe: {out}");
    }

    #[test]
    fn sandbox_blocks_in_repl() {
        let mut st = state();
        let out = run(&mut st, "rm -rf /").unwrap();
        assert!(out.contains("блокировка"), "rm -rf / должен блокироваться: {out}");
    }

    #[test]
    fn sandbox_confirm_refused_noninteractive() {
        let mut st = state();
        let out = run(&mut st, "rm -rf ./some-build-dir").unwrap();
        assert!(out.contains("не подтверждено") || out.contains("блокировка"), "Confirm в non-interactive = отказ: {out}");
    }

    #[test]
    fn sandbox_autoyes_allows_confirm() {
        let mut st = state();
        st.auto_yes = true;
        // безопасный подтверждаемый кейс: rm -r на несуществующем каталоге
        // (rm вернёт ошибку, но sandbox пропустит)
        let out = run(&mut st, "rm -r /tmp/definitely-not-exist-xyz").unwrap();
        assert!(!out.contains("не подтверждено"), "auto_yes должен пропустить: {out}");
    }

    #[test]
    fn redirect_writes_file() {
        let mut st = state();
        let tmp = std::env::temp_dir().join(format!("poler-gw-redir-{}.txt", std::process::id()));
        let path = tmp.display().to_string();
        let out = run(&mut st, &format!("echo hello > {path}")).unwrap();
        assert!(out.contains("→"), "редирект должен отчитаться: {out}");
        let content = std::fs::read_to_string(&tmp).unwrap();
        assert_eq!(content.trim(), "hello");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn redirect_to_dev_blocked() {
        let mut st = state();
        let out = run(&mut st, "echo x > /dev/sda").unwrap();
        assert!(out.contains("блокировка"), "редирект в /dev/sda — Block: {out}");
    }

    #[test]
    fn cd_and_pwd() {
        let mut st = state();
        run(&mut st, "cd /tmp").unwrap();
        let out = run(&mut st, "pwd").unwrap();
        assert_eq!(out.trim(), "/tmp");
        // относительный cd
        run(&mut st, "cd /").unwrap();
        run(&mut st, "cd tmp").unwrap();
        let out = run(&mut st, "pwd").unwrap();
        assert_eq!(out.trim(), "/tmp");
        // cd в несуществующий — ошибка
        let out = run(&mut st, "cd /no/such/dir/xyz");
        assert!(out.unwrap().contains("⚡"));
    }

    #[test]
    fn license_command_shows_eula() {
        let mut st = state();
        let out = run(&mut st, "license").unwrap();
        assert!(out.contains("Source-Available"));
        assert!(out.contains("dev@poler-engine.org"));
        assert!(out.contains("Notification Clause"));
    }

    #[test]
    fn help_lists_both_circuits() {
        let h = gateway_help();
        assert!(h.contains("КОНТУР 1"));
        assert!(h.contains("КОНТУР 2"));
        assert!(h.contains("service start"));
        assert!(h.contains("!grep"));
    }

    #[test]
    fn version_mentions_gateway() {
        let mut st = state();
        let out = run(&mut st, "version").unwrap();
        assert!(out.contains("Terminal Gateway"));
    }

    #[test]
    fn quit_returns_quit() {
        let mut st = state();
        assert!(matches!(exec_line(&mut st, "quit", false), GatewayResult::Quit));
    }

    #[test]
    fn engine_stdin_rejected_for_search() {
        let mut st = state();
        let out = run(&mut st, "echo x | search query").unwrap();
        assert!(out.contains("не принимает stdin"), "search не принимает stdin: {out}");
    }

    #[test]
    fn parse_error_reported() {
        let mut st = state();
        let out = run(&mut st, "echo \"unclosed").unwrap();
        assert!(out.contains("⚡"));
    }

    #[test]
    fn service_status_table() {
        let mut st = state();
        let out = run(&mut st, "service status").unwrap();
        assert!(out.contains("mcp"));
        assert!(out.contains("weblens"));
        assert!(out.contains("companion"));
    }

    #[test]
    fn service_unknown_name() {
        let mut st = state();
        let out = run(&mut st, "service start zzz").unwrap();
        assert!(out.contains("неизвестный сервис"));
    }

    #[test]
    fn set_hosttimeout_and_env() {
        let mut st = state();
        let out = run(&mut st, "set hosttimeout 42").unwrap();
        assert!(out.contains("42"));
        assert_eq!(st.limits.timeout_secs, 42);
        let out = run(&mut st, "set hostenv full").unwrap();
        assert!(out.contains("full"));
        assert_eq!(st.limits.env_mode, EnvMode::Full);
        let _ = run(&mut st, "set hostenv filtered").unwrap();
        assert_eq!(st.limits.env_mode, EnvMode::Filtered);
    }

    #[test]
    fn delegate_shell_search_help() {
        // делегация во внутренний шелл: пустой search печатает подсказку
        let mut st = state();
        let out = run(&mut st, "search").unwrap();
        assert!(out.contains("пустой запрос") || out.contains("ничего"), "поиск-подсказка: {out}");
    }

    #[test]
    fn delegate_shell_quotes_survive() {
        // кавычки в аргументах переживают делегацию (рекавычивание)
        let mut st = state();
        let out = run(&mut st, "search \"запрос с пробелами\"").unwrap();
        assert!(!out.contains("⚡"), "не должно быть ошибки: {out}");
    }

    #[test]
    fn poler_prefix_stripped_in_pipeline() {
        let mut st = state();
        let out = run(&mut st, "printf 'a b c\\n' | poler grep b --stdin").unwrap();
        assert!(out.contains("a b c"), "poler-префикс должен срезаться: {out}");
    }
}
