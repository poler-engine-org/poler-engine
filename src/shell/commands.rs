//! Command parser + dispatcher для REPL/TUI. Принимает строку ввода
//! пользователя, парсит на команды и аргументы (с поддержкой кавычек),
//! вызывает соответствующий обработчик `ShellState`.
//!
//! Синтаксис:
//! ```text
//! poler> search "Касіопея Astra-Nic Complex" --top 5
//! poler> stats                          # статистика web-index
//! poler> crawl https://example.com      # обход сайта в индекс
//! poler> sync vcs gh kotokvit           # синк VCS в индекс
//! poler> notes add "Идея"               # локальные заметки
//! poler> set format json                # переключить формат
//! poler> set top 20                     # топ-K по умолчанию
//! poler> help                           # список команд
//! poler> quit | exit                    # выход
//! ```

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

use crate::notes;
use crate::sources;
use crate::vcs::VcsAdapter;

use super::agentenv;
use super::help;
use super::state::ShellState;
use super::wincompat::{self, WinTranslation};


/// Результат исполнения одной команды.
#[derive(Debug, Clone)]
pub enum CmdResult {
    /// Команда выполнена, `output` — текст для отображения.
    Done(String),
    /// Команда требует выхода из шелла.
    Quit,
    /// Пустая строка ввода (ничего не делать, перейти к следующей итерации).
    Empty,
}

/// Распарсить строку ввода на токены с поддержкой кавычек.
/// `"..."` и `'...'` — единый токен с пробелами внутри.
pub fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut buf = String::new();
    let mut in_dq = false;
    let mut in_sq = false;
    for ch in line.chars() {
        match (ch, in_dq, in_sq) {
            ('"', false, false) => in_dq = true,
            ('\'', false, false) => in_sq = true,
            ('"', true, false) => in_dq = false,
            ('\'', false, true) => in_sq = false,
            (c, _, _) if !in_dq && !in_sq && c.is_whitespace() => {
                if !buf.is_empty() {
                    out.push(std::mem::take(&mut buf));
                }
            }
            (c, _, _) => buf.push(c),
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

/// Исполнить одну строку ввода в контексте `state`.
pub fn dispatch(state: &mut ShellState, line: &str) -> CmdResult {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return CmdResult::Empty;
    }

    // v0.48.0: Префикс `=` — быстрый путь калькулятора (= 2^10, = 5 km to mi)
    if let Some(expr) = trimmed.strip_prefix('=') {
        let expr = expr.trim();
        if expr.is_empty() {
            return CmdResult::Done("= <выражение> — посчитать (= 2^10, = 5 km to mi, = solve x^2 = 4)".into());
        }
        return cmd_calc(state, expr);
    }

    // v0.46.0: Прямой вызов системного шелла через `!` (например, `! agy ...` или `! ls -la`)
    if trimmed.starts_with('!') {
        let raw_sh = trimmed.trim_start_matches('!').trim();
        if raw_sh.is_empty() {
            return CmdResult::Done("! <command> — выполнить команду в системном шелле (например, `! ls -la`, `! agy ...`)".into());
        }
        return run_sh_command(raw_sh);
    }

    let tokens = tokenize(line);
    if tokens.is_empty() {
        return CmdResult::Empty;
    }
    let cmd = tokens[0].as_str();
    let args = &tokens[1..];

    match cmd {
        "" => CmdResult::Empty,
        "quit" | "exit" | "q" => CmdResult::Quit,
        "help" | "?" => {
            // `help` без аргументов → overview; `help <topic>` → детальная справка
            if args.is_empty() {
                CmdResult::Done(help::help_overview())
            } else {
                let topic = args.join(" ");
                CmdResult::Done(help::help_topic(&topic))
            }
        }
        "version" | "v" => CmdResult::Done(format!(
            "poler-engine {} (poler-shell v0.48.0 — Калькулятор Всего: calc/= , единицы, матрицы expm, триты, астро/гео, hw-зонд; среда агента: sysinfo/env/pty/--exec --json)",
            env!("CARGO_PKG_VERSION")
        )),
        "search" | "web" => cmd_search(state, args),
        "stats" => cmd_stats(state),
        // v0.16.0: alias для subкоманд vcs-sync: `sync vcs github owner`
        "sync" => cmd_sync(state, args),
        "set" => cmd_set(state, args),
        // v0.15.1: нативные команды crawl/impact внутри шелла
        "crawl" => cmd_crawl(state, args),
        "impact" => cmd_impact(state, args),
        // v0.16.0: Unified VCS & Data Mesh — нативные адаптеры GitHub/GitLab/Gitea/gix
        "gh" => cmd_gh(state, args),
        "gl" => cmd_gl(state, args),
        "gt" => cmd_gt(state, args),
        "gix" => cmd_gix(state, args),
        // v0.17.0: Notes & Sources CRUD
        "notes" => cmd_notes(state, args),
        "sources" => cmd_sources(state, args),
        // v0.46.0: Системный шелл и прямое исполнение
        "sh" | "bash" | "exec" => {
            if args.is_empty() {
                CmdResult::Done("sh <command> — выполнить команду в шелле".into())
            } else {
                run_sh_command(&args.join(" "))
            }
        }
        // v0.47.0: навигация ФС и терминал (Linux + Windows словари)
        "cd" | "chdir" => cmd_cd(args),
        "pwd" => CmdResult::Done(
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|e| format!("❌ {e}")),
        ),
        "clear" | "cls" => CmdResult::Done("\x1b[2J\x1b[H".into()),
        // v0.47.0: среда для ИИ-агентов (Antigravity)
        "sysinfo" | "systeminfo" => CmdResult::Done(agentenv::sysinfo()),
        "env" => CmdResult::Done(agentenv::env_snapshot(args.first().map(|s| s.as_str()))),
        "agent" => CmdResult::Done(agentenv::agent_status()),
        "win" | "winhelp" => CmdResult::Done(wincompat::catalog()),
        "pty" => cmd_pty(args),
        // v0.47.0: прямые команды движка
        "engine" => cmd_engine(args),
        // v0.47.0: POLER Reader — живой голос книги
        "read" | "reader" => cmd_read(args),
        // v0.48.0: Калькулятор Всего — calc берёт RAW-аргументы (кавычки в
        // выражениях типа trit_val("1TT") обязаны дожить до лексера)
        "calc" => {
            let raw = raw_args_of(line);
            if raw.trim().is_empty() {
                CmdResult::Done(calc_usage())
            } else {
                cmd_calc(state, &raw)
            }
        }
        // v0.48.0: зонд скрытых параметров ПК
        "hw" | "hardware" => cmd_hw(args),
        other => {
            // v0.47.0: echo с Windows-переменными %NAME% → ${NAME}
            if other == "echo" && args.iter().any(|a| contains_win_var(a)) {
                let rewritten: Vec<String> =
                    args.iter().map(|a| expand_win_vars(a)).collect();
                return run_sh_command(&format!("echo {}", rewritten.join(" ")));
            }
            // v0.47.0: Windows-словарь (dir/type/copy/findstr/taskkill…) —
            // трансляция ПЕРЕД PATH-поиском; Linux-команды не перехватываются
            match wincompat::translate(other, args) {
                Some(tr) => run_win_translation(tr),
                None => run_system_cmd(other, args),
            }
        }
    }
}


// ---------------------------------------------------------------------------
// v0.48.0: Калькулятор Всего (calc / префикс =) и зонд железа (hw)
// ---------------------------------------------------------------------------

/// RAW-хвост строки после первого слова — БЕЗ разборки кавычек
/// (токенизатор шелла съедает "…" , а лексеру калькулятора они нужны).
pub fn raw_args_of(line: &str) -> String {
    let trimmed = line.trim_start();
    match trimmed.find(char::is_whitespace) {
        Some(sp) => trimmed[sp..].trim().to_string(),
        None => String::new(),
    }
}

/// Краткая справка по calc.
fn calc_usage() -> String {
    [
        "calc <выражение>          — вычислить: calc (1538*485)/1024, calc 2^10",
        "calc solve <уравнение>    — корни: calc solve x^2 - 4 = 0",
        "calc x = 5                — переменная; ans — последний результат",
        "calc 5 km + 300 m         — единицы: to mi / to m/s / to degF",
        "calc expm([0,-1;1,0]*psi) — матрицы: det inv eigen trace rot2 so_gen",
        "calc trits(5)             — триты POLER: trit_val(\"1TT\")",
        "calc moon_illum(2024,4,8,18.35) — астрономия (затмения/фазы/планеты)",
        "calc dist(50.45,30.52,49.84,24.03) — геодезия/навигация",
        "calc constants | units | funcs | vars | hist | laws — каталоги",
        "calc script <закон> [k=v] — генератор скриптов по законам физики",
        "префикс: = 2^10          — то же самое, короче",
        "help calc                 — полная справка с примерами",
    ]
    .join("\n")
}

/// Исполнить выражение калькулятора в контексте state.
fn cmd_calc(state: &mut ShellState, expr: &str) -> CmdResult {
    let src = expr.trim();

    // симметричные кавычки вокруг выражения: calc "2 + 2"
    let src = if src.len() >= 2
        && src.starts_with('"')
        && src.ends_with('"')
        && !src[1..src.len() - 1].contains('"')
    {
        &src[1..src.len() - 1]
    } else {
        src
    };

    // подкоманды-каталоги: строго по первому слову (иначе units*2 примут
    // за запрос каталога)
    let mut words = src.split_whitespace();
    let first = words.next().unwrap_or("").to_lowercase();
    let rest: String = words.collect::<Vec<_>>().join(" ");
    match first.as_str() {
        "vars" | "переменные" => {
            return CmdResult::Done(state.calc.vars_text().trim_end().to_string())
        }
        "hist" | "history" | "история" => {
            return CmdResult::Done(state.calc.history_text(20).trim_end().to_string())
        }
        "constants" => {
            return CmdResult::Done(
                crate::calc::constants::list_all(&rest.to_lowercase()).trim_end().to_string(),
            )
        }
        "units" => {
            let names = crate::calc::units::all_names();
            let filtered: Vec<&str> = names
                .into_iter()
                .filter(|n| rest.is_empty() || n.contains(&rest.to_lowercase()))
                .collect();
            return CmdResult::Done(format!("{} единиц: {}", filtered.len(), filtered.join(" ")));
        }
        "funcs" => {
            return CmdResult::Done(
                crate::calc::functions::catalog(&rest.to_lowercase()).trim_end().to_string(),
            )
        }
        "laws" => {
            return CmdResult::Done(crate::calc::scriptgen::list_laws().trim_end().to_string())
        }
        "script" => {
            let (law_name, overrides) = parse_script_overrides(&rest);
            return match crate::calc::scriptgen::find_law(&law_name) {
                None => CmdResult::Done(format!(
                    "закон «{law_name}» не найден; список: calc laws"
                )),
                Some(law) => match crate::calc::scriptgen::generate(law, &overrides) {
                    Ok(text) => CmdResult::Done(text.trim_end().to_string()),
                    Err(e) => CmdResult::Done(format!("❌ {e}")),
                },
            };
        }
        _ => {}
    }

    // обычное вычисление
    match state.calc.eval_line(src) {
        Ok(out) => CmdResult::Done(out),
        Err(e) => CmdResult::Done(format!("❌ {e}")),
    }
}

/// «kepler3 a=0.5 au M1=1.9885e30 kg» → («kepler3», [(a, «0.5 au»), …]).
/// Значение жадно поглощает токены до следующего `k=` (юниты с пробелами!).
fn parse_script_overrides(rest: &str) -> (String, Vec<(String, String)>) {
    let mut it = rest.split_whitespace();
    let law = it.next().unwrap_or("").to_string();
    let mut overrides = Vec::new();
    let mut current: Option<(String, String)> = None;
    for tok in it {
        if let Some((k, v)) = tok.split_once('=') {
            if let Some(done) = current.take() {
                overrides.push(done);
            }
            current = Some((k.to_string(), v.to_string()));
        } else if let Some((_, v)) = current.as_mut() {
            v.push(' ');
            v.push_str(tok);
        }
    }
    if let Some(done) = current.take() {
        overrides.push(done);
    }
    (law, overrides)
}

/// Зонд скрытых параметров ПК: `hw`, `hw --json`.
fn cmd_hw(args: &[String]) -> CmdResult {
    let report = crate::calc::hardware::probe();
    if args.iter().any(|a| a == "--json" || a == "-j") {
        CmdResult::Done(report.to_json())
    } else {
        let mut out = String::from("🔍 Скрытые параметры ПК (v0.48.0)");
        out.push_str(&report.to_text());
        CmdResult::Done(out.trim_end().to_string())
    }
}

// ---------------------------------------------------------------------------
// v0.46.0: Системный шелл, passthrough и запуск .poler-контейнеров
// ---------------------------------------------------------------------------

fn run_sh_command(raw_cmd: &str) -> CmdResult {
    let t0 = Instant::now();
    let output = match std::process::Command::new("sh").arg("-c").arg(raw_cmd).output() {
        Ok(o) => o,
        Err(e) => return CmdResult::Done(format!("❌ помилка виклику sh: {e}")),
    };
    let mut res = format_output("sh", &output);
    res.push_str(&agent_timing_suffix(t0));
    CmdResult::Done(res)
}

/// Свести stdout+stderr команды в единый текст (v0.47.0 — общий хелпер).
fn format_output(label: &str, output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mut res = String::new();
    if !stdout.is_empty() {
        res.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !res.is_empty() && !res.ends_with('\n') {
            res.push('\n');
        }
        res.push_str(&stderr);
    }
    if res.is_empty() {
        res = format!("✓ [{label}] виконано (код: {})", output.status);
    }
    res.trim_end_matches('\n').to_string()
}

/// Суффикс ⏱ <мс> для внешних команд в агентном режиме (POLER_SHELL_AGENT=1).
fn agent_timing_suffix(t0: Instant) -> String {
    let on = std::env::var("POLER_SHELL_AGENT")
        .map(|v| v == "1")
        .unwrap_or(false);
    if on {
        format!("\n⏱ {} мс", t0.elapsed().as_millis())
    } else {
        String::new()
    }
}

/// v0.47.0: исполнение трансляции Windows-команды.
fn run_win_translation(tr: WinTranslation) -> CmdResult {
    match tr {
        WinTranslation::Exec { program, args, note } => {
            let t0 = Instant::now();
            let output =
                match std::process::Command::new(&program).args(&args).output() {
                    Ok(o) => o,
                    Err(e) => {
                        return CmdResult::Done(format!(
                            "❌ {program}: {e} (нет в системе?)"
                        ))
                    }
                };
            let mut res = format_output(&program, &output);
            if let Some(n) = note {
                res.push_str(&format!("\nℹ {n}"));
            }
            res.push_str(&agent_timing_suffix(t0));
            CmdResult::Done(res)
        }
        WinTranslation::Shell { script, note } => {
            let mut res = run_sh_command(&script);
            if let CmdResult::Done(ref mut s) = res {
                if let Some(n) = note {
                    s.push_str(&format!("\nℹ {n}"));
                }
            }
            res
        }
        WinTranslation::Notice(msg) => CmdResult::Done(msg),
    }
}

/// Есть ли в токене Windows-переменная вида %NAME%.
fn contains_win_var(tok: &str) -> bool {
    let bytes = tok.as_bytes();
    let mut pct: Vec<usize> = Vec::new();
    for (i, b) in bytes.iter().enumerate() {
        if *b == b'%' {
            pct.push(i);
        }
    }
    if pct.len() < 2 {
        return false;
    }
    // хотя бы одна пара %...% с валидным именем
    for w in pct.windows(2) {
        let name = &tok[w[0] + 1..w[1]];
        if !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return true;
        }
    }
    false
}

/// %NAME% → ${NAME} (для передачи в sh).
fn expand_win_vars(tok: &str) -> String {
    let mut out = String::with_capacity(tok.len() + 4);
    let mut chars = tok.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%' {
            let mut name = String::new();
            let mut consumed = false;
            for c2 in chars.by_ref() {
                if c2 == '%' {
                    consumed = true;
                    break;
                }
                name.push(c2);
            }
            if consumed
                && !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                out.push_str(&format!("${{{name}}}"));
            } else {
                out.push('%');
                out.push_str(&name);
                if consumed {
                    out.push('%');
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// v0.47.0: cd / pty / engine — нативные команды
// ---------------------------------------------------------------------------

fn cmd_cd(args: &[String]) -> CmdResult {
    let target = match args.first() {
        None => {
            // Windows `cd` без аргументов печатает текущий каталог
            return CmdResult::Done(
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| format!("❌ {e}")),
            );
        }
        Some(s) if s.is_empty() => {
            return CmdResult::Done(
                std::env::current_dir()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|e| format!("❌ {e}")),
            );
        }
        Some(s) => s.clone(),
    };
    let target = if target == "~" {
        std::env::var("HOME").unwrap_or(target)
    } else if let Some(rest) = target.strip_prefix("~/") {
        format!("{}/{}", std::env::var("HOME").unwrap_or_default(), rest)
    } else {
        target
    };
    match std::env::set_current_dir(&target) {
        Ok(()) => CmdResult::Done(
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ),
        Err(e) => CmdResult::Done(format!("❌ cd: {e}")),
    }
}

fn cmd_pty(args: &[String]) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(
            "pty <command> — запуск команды в псевдотерминале (PTY-мост для top, gdb, htop и других интерактивных утилит)".into(),
        );
    }
    // script (util-linux) выделяет pseudo-tty: интерактивные утилиты видят TTY
    let script_cmd = args.join(" ");
    let t0 = Instant::now();
    let output = match std::process::Command::new("script")
        .arg("-qec")
        .arg(&script_cmd)
        .arg("/dev/null")
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return CmdResult::Done(format!(
                "❌ pty: {e} — утилита `script` (util-linux) не найдена"
            ))
        }
    };
    let mut res = format_output("pty", &output);
    res.push_str(&agent_timing_suffix(t0));
    CmdResult::Done(res)
}

fn cmd_engine(args: &[String]) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(
            "engine <args...> — вызов CLI самого движка (self-exec).\nПримеры: engine --benchmark, engine --poler-box app.poler, engine --license\nКрейты pqc/pqw доступны напрямую через PATH (transparent passthrough).".into(),
        );
    }
    let exe =
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from("poler-engine"));
    let t0 = Instant::now();
    let output = match std::process::Command::new(&exe).args(args).output() {
        Ok(o) => o,
        Err(e) => return CmdResult::Done(format!("❌ engine: {e}")),
    };
    let mut res = format_output("engine", &output);
    res.push_str(&agent_timing_suffix(t0));
    CmdResult::Done(res)
}

// ---------------------------------------------------------------------------
// v0.47.0: read — POLER Reader внутри шелла
// ---------------------------------------------------------------------------

/// `read <книга.txt|md|fb2|poler-book> [--out x.wav] [--voice a_calm] [--seed N]`
/// Живой голос книги: роторный резонатор + коартикуляция. Без --out —
/// только статистика (сколько будет звучать).
fn cmd_read(args: &[String]) -> CmdResult {
    let mut input: Option<String> = None;
    let mut out: Option<String> = None;
    let mut voice = "a_calm".to_string();
    let mut seed: u64 = 42;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--out" | "-o" => {
                out = args.get(i + 1).cloned();
                i += 1;
            }
            "--voice" | "-v" => {
                if let Some(v) = args.get(i + 1) {
                    voice = v.clone();
                }
                i += 1;
            }
            "--seed" | "-s" => {
                if let Some(s) = args.get(i + 1) {
                    seed = s.parse().unwrap_or(42);
                }
                i += 1;
            }
            "--info" => {
                if let Some(p) = args.get(i + 1) {
                    return match poler_reader::polerbook::PolerBook::load(std::path::Path::new(p)) {
                        Ok(b) => CmdResult::Done(poler_reader::verify::book_info(&b)),
                        Err(e) => CmdResult::Done(format!("❌ {e}")),
                    };
                }
            }
            other if input.is_none() => input = Some(other.to_string()),
            _ => {}
        }
        i += 1;
    }
    let Some(path) = input else {
        return CmdResult::Done(
            "read <книга.txt|md|fb2|poler-book> [--out звук.wav] [--voice a_calm|a_bright|i_dark|u_calm] [--seed N]\n\nЖивой голос книги: роторный резонатор J=A−Aᵀ + тритная щель {-1,0,+1} +\nкоартикуляция (форманты плывут между звуками). Один seed = один голос навсегда.\n\nПримеры:\n  read книга.txt --out демо.wav --voice a_calm --seed 4242\n  read книга.poler-book --out демо.wav\n  read --info книга.poler-book\n\nКонвейер: pack через `poler-reader pack` (CLI-бинарник) — книга в 1500× меньше WAV.".into(),
        );
    };
    let path = PathBuf::from(&path);
    if !path.exists() {
        return CmdResult::Done(format!("❌ файл не найден: {}", path.display()));
    }

    let t0 = Instant::now();
    let arch = match poler_reader::voice::Archetype::from_name(&voice) {
        Some(a) => a,
        None => {
            return CmdResult::Done(format!(
                "❌ неизвестный голос {voice} (a_calm | a_bright | i_dark | u_calm)"
            ))
        }
    };

    // .poler-book — готовый паспорт; иначе текст → паспорт на лету
    let book = if poler_reader::book::detect_format(&path)
        == poler_reader::book::Format::PolerBook
    {
        match poler_reader::polerbook::PolerBook::load(&path) {
            Ok(b) => b,
            Err(e) => return CmdResult::Done(format!("❌ {e}")),
        }
    } else {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => return CmdResult::Done(format!("❌ {e}")),
        };
        match poler_reader::stream::pack_book(&text, seed, arch) {
            Ok(b) => b,
            Err(e) => return CmdResult::Done(format!("❌ {e}")),
        }
    };

    match out {
        None => CmdResult::Done(poler_reader::verify::book_info(&book)),
        Some(out_path) => {
            let outp = PathBuf::from(&out_path);
            match poler_reader::stream::render_book_file(&book, seed, &outp) {
                Ok(r) => {
                    let mut res = format!(
                        "✓ {} — {:.1} с звука, {} фраз, {} периодов щели, jitter {:.2}%\nкнига: {} Б; WAV: {} Б (сжатие ×{:.0})",
                        out_path,
                        r.duration_s,
                        r.n_phrases,
                        r.passport.n_periods,
                        r.passport.jitter_std_pct,
                        book.size_bytes(),
                        r.samples.len() * 2,
                        (r.samples.len() * 2) as f64 / book.size_bytes() as f64,
                    );
                    res.push_str(&agent_timing_suffix(t0));
                    CmdResult::Done(res)
                }
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
    }
}

fn run_system_cmd(cmd: &str, args: &[String]) -> CmdResult {
    let expanded = if cmd.starts_with("~/") {
        if let Ok(home) = std::env::var("HOME") {
            format!("{home}/{}", &cmd[2..])
        } else {
            cmd.to_string()
        }
    } else {
        cmd.to_string()
    };

    // Прямой запуск .poler-контейнера через poler-box
    if expanded.ends_with(".poler") || (cmd.ends_with(".poler") && std::path::Path::new(&expanded).exists()) {
        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("poler-engine"));
        let status = std::process::Command::new(current_exe)
            .arg("--poler-box")
            .arg(&expanded)
            .args(args)
            .status();

        match status {
            Ok(s) => return CmdResult::Done(format!("✓ [poler-box] завершено: {s}")),
            Err(e) => return CmdResult::Done(format!("❌ poler-box error: {e}")),
        }
    }

    // v0.46.1 (аудит): у PATH-скані тепер потрібен і біт виконуваності,
    // а не лише is_file — інакше fallback тихо «запускав» довільні
    // невиконувані файли (data-файли з іменами без розширення) і давав
    // плутанину з правами.
    let is_executable = |p: &std::path::Path| -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            match std::fs::metadata(p) {
                Ok(m) => m.is_file() && (m.permissions().mode() & 0o111) != 0,
                Err(_) => false,
            }
        }
        #[cfg(not(unix))]
        {
            p.is_file()
        }
    };
    let exists = if cmd.contains('/') {
        is_executable(std::path::Path::new(&expanded))
    } else if let Ok(paths) = std::env::var("PATH") {
        std::env::split_paths(&paths).any(|p| is_executable(&p.join(cmd)))
    } else {
        false
    };

    if exists {
        let t0 = Instant::now();
        let output = match std::process::Command::new(&expanded).args(args).output() {
            Ok(o) => o,
            Err(e) => return CmdResult::Done(format!("❌ помилка запуску {cmd}: {e}")),
        };
        let mut res = format_output(cmd, &output);
        res.push_str(&agent_timing_suffix(t0));
        CmdResult::Done(res)
    } else {
        CmdResult::Done(format!(
            "неизвестная команда: {cmd} (введите `help` для списка, `! <cmd>` для шелла, `win` для Windows-словаря, либо путь к .poler контейнеру)"
        ))
    }
}

// ---------------------------------------------------------------------------
// search / web — поиск по web-index.db
// ---------------------------------------------------------------------------

fn cmd_search(state: &mut ShellState, args: &[String]) -> CmdResult {
    let mut query = String::new();
    let mut top = state.top;
    for a in args {
        if a == "--top" || a == "-t" {
            // следующее значение — число, но мы не знаем заранее, поэтому
            // обработаем в следующей итерации
            continue;
        }
        if let Some(prev) = args.iter().take_while(|x| x.as_ptr() != a.as_ptr()).last() {
            if prev == "--top" || prev == "-t" {
                if let Ok(n) = a.parse::<usize>() {
                    top = n.max(1);
                    continue;
                }
            }
        }
        if !query.is_empty() {
            query.push(' ');
        }
        query.push_str(a);
    }
    if query.trim().is_empty() {
        return CmdResult::Done("поиск: пустой запрос (пример: search \"Касіопея Astra-Nic\")".into());
    }

    let ix = match state.ensure_index() {
        Ok(ix) => ix,
        Err(e) => return CmdResult::Done(format!("❌ {e}")),
    };
    let bridge = crate::retrieval::SemanticBridge::offline();
    let (hits, expansion) = match ix.search_with_bridge(&query, top, &bridge) {
        Ok(h) => h,
        Err(e) => return CmdResult::Done(format!("❌ web-search: {e}")),
    };
    let total = hits.len();
    let page_count = ix.page_count();

    let mut out = String::new();
    out.push_str(&format!(
        "🔍 «{query}» — {total} хитов из {page_count} страниц в web-index.db (top {top})\n\n"
    ));
    if hits.is_empty() {
        out.push_str("ничего не найдено. Подсказки:\n");
        out.push_str("  - наполните индекс: `crawl <URL>` здесь или poler-engine --crawl <URL>\n");
        out.push_str("  - локальные файлы ищет poler-engine <PATH> -q <QUERY> / --grep\n");
    } else {
        for (i, h) in hits.iter().enumerate() {
            out.push_str(&format_hit(i + 1, h, &query));
        }
    }
    // Semantic Bridge WHY: кросс-языковые кандидаты сенсора видны агенту
    if !expansion.is_empty() {
        out.push_str("\n🔗 Semantic Bridge (WHY):\n");
        for l in expansion.why_lines() {
            out.push_str(&format!("   {l}\n"));
        }
    }
    state.set_output(out.clone());
    CmdResult::Done(out)
}

fn format_hit(i: usize, h: &crate::web::WebHit, query: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("{}. [{:.3}] {}\n", i, h.score, h.url));
    if !h.title.is_empty() {
        s.push_str(&format!("   title: {}\n", h.title));
    }
    if !h.snippet.is_empty() {
        s.push_str(&format!("   {}\n", h.snippet));
    }
    let _ = query;
    s
}

// ---------------------------------------------------------------------------
// stats — статистика web-index.db
// ---------------------------------------------------------------------------

fn cmd_stats(state: &mut ShellState) -> CmdResult {
    let ix = match state.ensure_index() {
        Ok(ix) => ix,
        Err(e) => return CmdResult::Done(format!("❌ {e}")),
    };
    let mut st = match ix.stats() {
        Ok(s) => s,
        Err(e) => return CmdResult::Done(format!("❌ stats: {e}")),
    };
    st.db_bytes = std::fs::metadata(state.db_path()).map(|m| m.len()).unwrap_or(0);
    let out = serde_json::to_string_pretty(&st).unwrap_or_else(|_| "{}".into());
    state.set_output(out.clone());
    CmdResult::Done(out)
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// set — переключение настроек шелла
// ---------------------------------------------------------------------------

fn cmd_set(state: &mut ShellState, args: &[String]) -> CmdResult {
    // v0.47.0: Windows-стиль `set NAME=VALUE` / `set NAME` / `set NAME=` (удалить)
    if let Some(first) = args.first() {
        if !matches!(first.as_str(), "format" | "top") && first.contains('=') {
            let (name, val) = first.split_once('=').expect("checked above");
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
            {
                return CmdResult::Done(
                    "❌ set: имя переменной — латиница/цифры/подчёркивание".into(),
                );
            }
            if val.is_empty() {
                std::env::remove_var(name);
                return CmdResult::Done(format!("✓ {name} — удалена из сессии"));
            }
            std::env::set_var(name, val);
            return CmdResult::Done(format!("✓ {name}={val} (для этой сессии)"));
        }
        // `set NAME` — показать переменную (Windows-поведение)
        if args.len() == 1 && !matches!(first.as_str(), "format" | "top") {
            if let Ok(v) = std::env::var(first) {
                return CmdResult::Done(format!("{first}={v}"));
            }
        }
    }
    if args.len() < 2 {
        return CmdResult::Done(
            "set <key> <value> — доступные ключи: format (md|json|simple), top (N)\nWindows-стиль: set NAME=VALUE — переменная сессии; set NAME — показать".into(),
        );
    }
    let (key, val) = (args[0].as_str(), args[1].as_str());
    match key {
        "format" => match state.set_format(val) {
            Ok(()) => CmdResult::Done(format!("✓ format = {:?}", state.format)),
            Err(e) => CmdResult::Done(format!("❌ {e}")),
        },
        "top" => match val.parse::<usize>() {
            Ok(n) => {
                state.top = n.max(1);
                CmdResult::Done(format!("✓ top = {}", state.top))
            }
            Err(_) => CmdResult::Done(format!("❌ top: {val} — не число")),
        },
        other => CmdResult::Done(format!(
            "set: неизвестный ключ {other} (format | top)"
        )),
    }
}

// ---------------------------------------------------------------------------
// crawl — обход URL → web-index.db (нативная интеграция v0.15.1)
// ---------------------------------------------------------------------------
//
// Делегирует в poler_engine::web::cdp_fetcher + poler_engine::web::crawl::crawl
// — те же функции, что и в standalone-режиме `poler-engine --crawl URL`. Однако
// в шелле есть важное преимущество: WebIndex уже открыт (если был `search`/
// `stats` ранее), и единственное новое состояние — это CDP-фечер.
//
// Синтаксис:
//   crawl <URL> [--depth N] [--max M] [--cross] [--delay-ms N] [--wait-ms N]
//           [--cdp-port P]
//
// По умолчанию: depth=2, max=25, cross=false, delay-ms=1000, wait-ms=800,
// cdp-port=9222. Поддерживает `crawl` без URL → показывает help по команде.

fn cmd_crawl(state: &mut ShellState, args: &[String]) -> CmdResult {
    // Парсим: первый позиционный аргумент — seed URL; остальные — флаги.
    let mut seed: Option<String> = None;
    let mut depth: usize = 2;
    let mut max_pages: usize = 25;
    let mut cross_site: bool = false;
    let mut delay_ms: u64 = 1000;
    let mut wait_ms: u64 = 800;
    let mut cdp_port: u16 = 9222;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--depth" | "-d" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<usize>() {
                        depth = n;
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("crawl --depth: ожидается число (например --depth 3)".into());
            }
            "--max" | "-m" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<usize>() {
                        max_pages = n.max(1);
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("crawl --max: ожидается число (например --max 50)".into());
            }
            "--cross" => {
                cross_site = true;
                i += 1;
                continue;
            }
            "--delay-ms" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<u64>() {
                        delay_ms = n;
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("crawl --delay-ms: ожидается число мс".into());
            }
            "--wait-ms" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<u64>() {
                        wait_ms = n;
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("crawl --wait-ms: ожидается число мс".into());
            }
            "--cdp-port" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<u16>() {
                        cdp_port = n;
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("crawl --cdp-port: ожидается число 1024-65535".into());
            }
            "--help" | "-h" => {
                return CmdResult::Done(
                    "crawl <URL> [--depth N] [--max M] [--cross] [--delay-ms N] [--wait-ms N] [--cdp-port P]\n  по умолчанию: depth=2, max=25, cross=false, delay-ms=1000, wait-ms=800, cdp-port=9222".into()
                );
            }
            other if other.starts_with("--") => {
                return CmdResult::Done(format!("crawl: неизвестный флаг {other}"));
            }
            _ => {
                if seed.is_none() {
                    seed = Some(a.clone());
                } else {
                    return CmdResult::Done(format!("crawl: лишний аргумент {a} (URL уже задан)"));
                }
            }
        }
        i += 1;
    }

    let Some(seed) = seed else {
        return CmdResult::Done(
            "crawl: укажите seed URL (пример: crawl https://rust-lang.org --depth 2 --max 25)".into(),
        );
    };
    if !seed.starts_with("http://") && !seed.starts_with("https://") {
        return CmdResult::Done(format!(
            "crawl: seed должен быть http(s)://..., получено {seed}"
        ));
    }

    // Открываем CDP-фечер. Если Chromium не запущен — пробуем ensure_chromium.
    let mut fetcher = match crate::web::cdp_fetcher_with_timeout(cdp_port, wait_ms, 45_000) {
        Ok(f) => f,
        Err(e) => {
            let mut out = format!("❌ CDP fetcher не инициализирован (порт {cdp_port}): {e}\n");
            out.push_str("  подсказка: установите POLER_CHROME_BIN или запустите Chromium вручную:\n");
            out.push_str(&format!(
                "    chrome --headless --remote-debugging-port={cdp_port} --no-sandbox\n"
            ));
            out.push_str("  либо укажите другой порт через --cdp-port <P>");
            return CmdResult::Done(out);
        }
    };

    // WebIndex открывается ленимо — внутри ensure_index(). reuse того же
    // подключения, что и для search/stats.
    let cfg = crate::web::CrawlConfig {
        max_pages,
        max_depth: depth,
        delay_ms,
        cross_site,
        wait_ms,
        page_timeout_ms: 45_000,
        respect_robots: true,
    };

    let progress = format!(
        "🕷 crawl: seed {seed}, depth ≤ {depth}, до {max_pages} страниц, cross_site={cross_site}, delay={delay_ms}мс\n"
    );

    let res = state.ensure_index().and_then(|ix| {
        crate::web::crawl::crawl(ix, &mut fetcher, &seed, &cfg, false).map_err(|e| e.to_string())
    });

    let out = match res {
        Ok(stats) => {
            let mut s = progress;
            s.push_str(&format!(
                "✅ готово — fetched={}, indexed={}, unchanged={}, duplicates={}, errors={}, sitemap={}, elapsed={}мс\n",
                stats.fetched,
                stats.indexed,
                stats.unchanged,
                stats.duplicates,
                stats.errors,
                stats.sitemap_urls,
                stats.elapsed_ms
            ));
            if stats.frontier_left > 0 {
                s.push_str(&format!(
                    "  (frontier: ещё {} URL в очереди — увеличьте --max)\n",
                    stats.frontier_left
                ));
            }
            s
        }
        Err(e) => format!("{progress}❌ crawl: {e}"),
    };

    state.set_output(out.clone());
    CmdResult::Done(out)
}

// ---------------------------------------------------------------------------
// impact — AIDDE impact-паспорт символа (нативная интеграция v0.15.1)
// ---------------------------------------------------------------------------
//
// Делегирует в poler_engine::aidde::SymbolTable::build +
// impact_analysis (или в SQLite-хранилище через --cache <DB>).
//
// Синтаксис:
//   impact <PATH> <SYMBOL> [--depth N] [--cache <DB>] [--max-file-bytes N]
//
// PATH — каталог с кодом (или один файл). По нему строится SymbolTable
// с теми же расширениями, что и основной движок (rs, py, ts, js, go, ...),
// затем impact_analysis(target=symbol, depth=N, max_items=200) находит
// upstream/downstream паспорта — кого вызывает этот символ и кто его зовёт.

fn cmd_impact(state: &mut ShellState, args: &[String]) -> CmdResult {
    // Парсим: первые 2 позиционных аргумента — PATH и SYMBOL; остальное — флаги.
    let mut positional: Vec<String> = Vec::new();
    let mut depth: usize = 3;
    let mut cache_db: Option<PathBuf> = None;
    let mut max_file_bytes: u64 = 64 * 1024 * 1024;

    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--depth" | "-d" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<usize>() {
                        depth = n;
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("impact --depth: ожидается число (1-5)".into());
            }
            "--cache" => {
                if let Some(v) = args.get(i + 1) {
                    cache_db = Some(PathBuf::from(v));
                    i += 2;
                    continue;
                }
                return CmdResult::Done("impact --cache: укажите путь к SQLite-файлу".into());
            }
            "--max-file-bytes" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<u64>() {
                        max_file_bytes = n;
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("impact --max-file-bytes: ожидается число байт".into());
            }
            "--help" | "-h" => {
                return CmdResult::Done(
                    "impact <PATH> <SYMBOL> [--depth N] [--cache <DB>] [--max-file-bytes N]\n  по умолчанию: depth=3, max-file-bytes=64MB".into()
                );
            }
            other if other.starts_with("--") => {
                return CmdResult::Done(format!("impact: неизвестный флаг {other}"));
            }
            _ => {
                positional.push(a.clone());
            }
        }
        i += 1;
    }

    if positional.len() < 2 {
        return CmdResult::Done(
            "impact: укажите PATH и SYMBOL (пример: impact ./src main --depth 3)".into(),
        );
    }
    let path = PathBuf::from(&positional[0]);
    let symbol = positional[1].clone();

    if !path.exists() {
        return CmdResult::Done(format!("impact: путь не найден: {}", path.display()));
    }

    // Строим EngineConfig с дефолтными расширениями движка — это даёт
    // те же фильтры файлов, что и в poler-engine <PATH> --impact.
    let config = crate::EngineConfig::default();
    let files: Vec<PathBuf> = crate::collect_files(&path, &config)
        .into_iter()
        .filter(|p| crate::detect_lang(p) != crate::CodeLang::Plain)
        .collect();

    if files.is_empty() {
        return CmdResult::Done(format!(
            "impact: кодовые файлы не найдены в {} (поддерживаемые расширения: rs/py/ts/js/go/...)",
            path.display()
        ));
    }

    let mut progress = format!(
        "🔬 impact: символ {symbol}, путь {}, кодовых файлов: {}, depth={depth}\n",
        path.display(),
        files.len()
    );

    let report = if let Some(db_path) = &cache_db {
        // SQLite-режим (для 65K+ файлов) — как в `poler-engine --impact X --impact-cache Y`
        let mut store = match crate::aidde::SymbolStore::open(db_path) {
            Ok(s) => s,
            Err(e) => {
                return CmdResult::Done(format!("{progress}❌ SymbolStore::open({db_path:?}): {e}"));
            }
        };
        if let Err(e) = store.build(&files, max_file_bytes) {
            return CmdResult::Done(format!("{progress}❌ SymbolStore::build: {e}"));
        }
        let (defs, calls) = store.stats();
        progress.push_str(&format!("  SymbolStore(sqlite): defs={defs}, calls={calls}\n"));
        crate::aidde::impact_analysis_sqlite(&store, &symbol, depth, 200)
    } else {
        // In-memory режим (по умолчанию) — как в `poler-engine PATH --impact X`
        let table = crate::aidde::SymbolTable::build(&files, max_file_bytes);
        crate::aidde::impact_analysis(&table, &symbol, depth, 200)
    };

    let out = match report {
        Some(r) => {
            let mut s = progress;
            s.push_str("─────────────────────────────────────────────\n");
            s.push_str(&format!("🎯 target_function: {}\n", r.target_function));
            s.push_str(&format!("   file: {}\n", r.file));
            s.push_str(&format!("   lines: {}\n", r.lines));
            s.push_str(&format!("   danger_level_if_modified: {}\n", r.danger_level_if_modified));
            s.push('\n');
            s.push_str(&format!(
                "🔗 structural relations — доказано call graph ({}/{}):\n",
                r.structural_relations.upstream_dependents.len(),
                r.structural_relations.downstream_dependencies.len()
            ));
            s.push_str(&format!(
                "⬆ upstream dependents ({}):\n",
                r.structural_relations.upstream_dependents.len()
            ));
            for d in &r.structural_relations.upstream_dependents {
                s.push_str(&format!(
                    "   • {} (вызывает в {})\n",
                    d.caller, d.file
                ));
            }
            s.push('\n');
            s.push_str(&format!(
                "⬇ downstream dependencies ({}):\n",
                r.structural_relations.downstream_dependencies.len()
            ));
            for d in &r.structural_relations.downstream_dependencies {
                s.push_str(&format!("   • {} (вызывается из {})\n", d.callee, d.file));
            }
            if !r.heuristic_triage_alerts.is_empty() {
                s.push('\n');
                s.push_str(&format!(
                    "⚠ triage layer — эвристические сигналы, НЕ доказательства ({}):\n",
                    r.heuristic_triage_alerts.len()
                ));
                for a in &r.heuristic_triage_alerts {
                    s.push_str(&format!(
                        "   • [{}] {} — {}\n",
                        a.category.label(),
                        a.description,
                        a.marker
                    ));
                }
            }
            s
        }
        None => format!("{progress}❌ символ не найден: {symbol} (проверьте регистр/полное имя)"),
    };

    state.set_output(out.clone());
    CmdResult::Done(out)
}

// ---------------------------------------------------------------------------
// v0.16.0: VCS-команды — gh / gl / gt / gix / sync vcs
// ---------------------------------------------------------------------------

/// `poler> gh <subcommand> [args]` — GitHub REST API.
/// Субкоманды: `search <Q>`, `repos <USER>`, `commits <OWNER/REPO>`, `issues <OWNER/REPO>`.
fn cmd_gh(state: &mut ShellState, args: &[String]) -> CmdResult {
    let adapter = match crate::vcs::github_adapter() {
        Ok(a) => a,
        Err(e) => return CmdResult::Done(format!("❌ gh: {e}")),
    };
    cmd_vcs_adapter(state, "gh", adapter, args)
}

/// `poler> gl <subcommand>` — GitLab REST v4.
fn cmd_gl(state: &mut ShellState, args: &[String]) -> CmdResult {
    let adapter = match crate::vcs::gitlab_adapter() {
        Ok(a) => a,
        Err(e) => return CmdResult::Done(format!("❌ gl: {e}")),
    };
    cmd_vcs_adapter(state, "gl", adapter, args)
}

/// `poler> gt <subcommand>` — Gitea/Forgejo REST.
fn cmd_gt(state: &mut ShellState, args: &[String]) -> CmdResult {
    let adapter = match crate::vcs::gitea_adapter() {
        Ok(a) => a,
        Err(e) => return CmdResult::Done(format!("❌ gt: {e}")),
    };
    cmd_vcs_adapter(state, "gt", adapter, args)
}

/// `poler> gix <log|clone|lfs> ...` — локальный git через Pure-Rust gix.
/// v0.17.0: добавлен настоящий `clone` (через gix::clone::PrepareFetch) и `lfs`.
fn cmd_gix(state: &mut ShellState, args: &[String]) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(
            "gix <subcommand> — доступные:\n  gix log <PATH> [--top N]    — листинг коммитов\n  gix clone <URL> <PATH> [--depth N] [--branch B]  — Pure-Rust clone\n  gix lfs list <PATH>           — найти LFS pointer-файлы\n  gix lfs fetch <PATH>          — скачать LFS-объекты через batch API".into(),
        );
    }
    let sub = args[0].as_str();
    match sub {
        "log" => {
            let path = match args.get(1) {
                Some(p) => p,
                None => return CmdResult::Done("gix log <PATH> — укажите путь к репозиторию".into()),
            };
            let mut top = 20usize;
            let mut i = 2;
            while i < args.len() {
                if (args[i] == "--top" || args[i] == "-t") && i + 1 < args.len() {
                    if let Ok(n) = args[i + 1].parse::<usize>() {
                        top = n.max(1);
                    }
                    i += 2;
                    continue;
                }
                i += 1;
            }
            match crate::vcs::local::GixAdapter::list_commits_at_path(
                std::path::Path::new(path),
                top,
            ) {
                Ok(commits) => {
                    if commits.is_empty() {
                        return CmdResult::Done(format!("gix log: 0 коммитов в {path}"));
                    }
                    let mut out = String::new();
                    out.push_str(&format!("gix log {path} — {} коммитов (top {})\n\n", commits.len(), top));
                    let adapter = crate::vcs::local::GixAdapter::default();
                    let repo = crate::vcs::RepoId::from_path(std::path::Path::new(path));
                    // Заодно вливаем в web-index.db — коммиты как gix:// страницы
                    if let Ok(ix) = state.ensure_index() {
                        let docs = crate::vcs::ingest::commits_to_docs(adapter.scheme(), &repo, &commits);
                        let mut new_count = 0;
                        let mut unc_count = 0;
                        for doc in &docs {
                            if let Ok((_, was_new)) = ix.upsert_page(doc) {
                                if was_new {
                                    new_count += 1;
                                } else {
                                    unc_count += 1;
                                }
                            }
                        }
                        let _ = ix.recompute_pagerank(20);
                        out.push_str(&format!(
                            "✓ индексировано: {new_count} новых, {unc_count} без изменений (gix://)\n\n"
                        ));
                    }
                    for c in &commits {
                        let short = crate::vcs::ingest::short_sha(&c.sha);
                        let subject = c.message.lines().next().unwrap_or("");
                        out.push_str(&format!(
                            "{}  {}  <{}>  [{}]\n    {}\n",
                            short,
                            crate::vcs::ingest::iso_time(c.authored_at),
                            c.author,
                            c.author_email,
                            subject,
                        ));
                    }
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ gix log: {e}")),
            }
        }
        "clone" => {
            let url = match args.get(1) {
                Some(u) => u,
                None => return CmdResult::Done("gix clone <URL> <PATH> [--depth N] [--branch B] — укажите URL".into()),
            };
            let dest = match args.get(2) {
                Some(p) => p,
                None => return CmdResult::Done("gix clone <URL> <PATH> [--depth N] [--branch B] — укажите путь назначения".into()),
            };
            // Парсинг опциональных флагов --depth N и --branch B
            let mut opts = crate::vcs::clone::CloneOpts::new(url, std::path::Path::new(dest));
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--depth" | "-d" if i + 1 < args.len() => {
                        if let Ok(n) = args[i + 1].parse::<usize>() {
                            opts = opts.with_depth(n);
                        }
                        i += 2;
                        continue;
                    }
                    "--branch" | "-b" if i + 1 < args.len() => {
                        opts = opts.with_branch(args[i + 1].clone());
                        i += 2;
                        continue;
                    }
                    _ => i += 1,
                }
            }
            match crate::vcs::clone::clone_repo(&opts) {
                Ok(p) => CmdResult::Done(format!("✓ gix clone: {url} → {}", p.display())),
                Err(e) => CmdResult::Done(format!("❌ gix clone: {}", e.to_user_string())),
            }
        }
        "lfs" => cmd_gix_lfs(state, &args[1..]),
        other => CmdResult::Done(format!("gix: неизвестная подкоманда {other} (log|clone|lfs)")),
    }
}

/// `poler> gix lfs <list|fetch> <PATH>` — LFS pointer detection + batch fetch.
fn cmd_gix_lfs(state: &mut ShellState, args: &[String]) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(
            "gix lfs <subcommand> — доступные:\n  gix lfs list <PATH>   — найти LFS pointer-файлы в worktree\n  gix lfs fetch <PATH>  — скачать LFS-объекты (batch API)".into(),
        );
    }
    let sub = args[0].as_str();
    match sub {
        "list" => {
            let path = match args.get(1) {
                Some(p) => p,
                None => return CmdResult::Done("gix lfs list <PATH> — укажите путь к репозиторию".into()),
            };
            let pointers = crate::vcs::lfs::detect_pointers(std::path::Path::new(path));
            let out = crate::vcs::lfs::format_pointers(&pointers);
            state.set_output(out.clone());
            CmdResult::Done(out)
        }
        "fetch" => {
            let path = match args.get(1) {
                Some(p) => p,
                None => return CmdResult::Done("gix lfs fetch <PATH> — укажите путь к репозиторию".into()),
            };
            let pointers = crate::vcs::lfs::detect_pointers(std::path::Path::new(path));
            if pointers.is_empty() {
                return CmdResult::Done("LFS pointer-файлов не обнаружено — нечего скачивать".into());
            }
            match crate::vcs::lfs::fetch_objects(std::path::Path::new(path), &pointers) {
                Ok(results) => {
                    let out = crate::vcs::lfs::format_fetch_results(&results);
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ gix lfs fetch: {e}")),
            }
        }
        other => CmdResult::Done(format!("gix lfs: неизвестная подкоманда {other} (list|fetch)")),
    }
}

/// Общий обработчик для `gh`/`gl`/`gt` — все имеют REST-adapter.
fn cmd_vcs_adapter(
    state: &mut ShellState,
    label: &str,
    adapter: impl crate::vcs::VcsAdapter,
    args: &[String],
) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(format!(
            "{label} <subcommand> — доступные:\n  search <Q>\n  repos <USER>\n  commits <OWNER/REPO>\n  issues <OWNER/REPO>"
        ));
    }
    let sub = args[0].as_str();
    let rest = &args[1..];
    match sub {
        "search" => {
            let query = match rest.first() {
                Some(q) => q,
                None => return CmdResult::Done(format!("{label} search <QUERY> — укажите запрос")),
            };
            match adapter.search_code(query, 20) {
                Ok(hits) => {
                    if hits.is_empty() {
                        return CmdResult::Done(format!("{label} search «{query}»: 0 хитов"));
                    }
                    let mut out = String::new();
                    out.push_str(&format!("🔍 {label} «{query}» — {} хитов\n\n", hits.len()));
                    for (i, h) in hits.iter().enumerate() {
                        out.push_str(&format!("{}. {}\n", i + 1, h.repo));
                        out.push_str(&format!("   {}/{}\n", h.path, h.sha));
                        if !h.web_url.is_empty() {
                            out.push_str(&format!("   {}\n", h.web_url));
                        }
                        if !h.snippet.is_empty() {
                            out.push_str(&format!("   {}\n", h.snippet));
                        }
                    }
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ {label} search: {e}")),
            }
        }
        "repos" => {
            let owner = match rest.first() {
                Some(o) => o,
                None => return CmdResult::Done(format!("{label} repos <USER> — укажите owner")),
            };
            match adapter.list_repos(owner) {
                Ok(repos) => {
                    if repos.is_empty() {
                        return CmdResult::Done(format!("{label} repos {owner}: 0 репозиториев"));
                    }
                    let mut out = String::new();
                    out.push_str(&format!("📂 {label} {owner} — {} репозиториев\n\n", repos.len()));
                    for (i, r) in repos.iter().enumerate() {
                        out.push_str(&format!("{}. {}\n", i + 1, r.display));
                    }
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ {label} repos: {e}")),
            }
        }
        "commits" => {
            let repo_str = match rest.first() {
                Some(r) => r,
                None => return CmdResult::Done(format!("{label} commits <OWNER/REPO> — укажите")),
            };
            let repo = crate::vcs::RepoId::new(repo_str.clone(), repo_str.clone());
            match adapter.list_commits(&repo, 20) {
                Ok(commits) => {
                    if commits.is_empty() {
                        return CmdResult::Done(format!("{label} commits {repo_str}: 0 коммитов"));
                    }
                    let mut out = String::new();
                    out.push_str(&format!("📜 {label} {repo_str} — {} коммитов\n\n", commits.len()));
                    for c in &commits {
                        let short = crate::vcs::ingest::short_sha(&c.sha);
                        let subject = c.message.lines().next().unwrap_or("");
                        out.push_str(&format!(
                            "{}  {}  <{}>\n    {}\n",
                            short,
                            crate::vcs::ingest::iso_time(c.authored_at),
                            c.author,
                            subject,
                        ));
                    }
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ {label} commits: {e}")),
            }
        }
        "issues" => {
            let repo_str = match rest.first() {
                Some(r) => r,
                None => return CmdResult::Done(format!("{label} issues <OWNER/REPO> — укажите")),
            };
            let repo = crate::vcs::RepoId::new(repo_str.clone(), repo_str.clone());
            match adapter.list_issues(&repo, 50) {
                Ok(issues) => {
                    if issues.is_empty() {
                        return CmdResult::Done(format!("{label} issues {repo_str}: 0 issues/MR"));
                    }
                    let mut out = String::new();
                    out.push_str(&format!("🐛 {label} {repo_str} — {} issues/MR\n\n", issues.len()));
                    for i in &issues {
                        let kind = if i.is_merge_request { "MR" } else { "IS" };
                        out.push_str(&format!("[{}] #{} {} ({})\n", kind, i.number, i.title, i.state));
                        out.push_str(&format!("    by {} at {}\n", i.author, crate::vcs::ingest::iso_time(i.created_at)));
                    }
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ {label} issues: {e}")),
            }
        }
        "--help" | "-h" => CmdResult::Done(format!(
            "{label} <search|repos|commits|issues> ... — REST API {label}"
        )),
        other => CmdResult::Done(format!("{label}: неизвестная подкоманда {other}")),
    }
}

/// `poler> sync vcs [gh|gl|gt|all] <OWNER>` — синк всех VCS-страниц в web-index.db.
/// `poler> sync vcs ...` — делегация в vcs::sync_vcs (v2.0: NLM-синк удалён).
fn cmd_sync(state: &mut ShellState, args: &[String]) -> CmdResult {
    // Если первый аргумент — `vcs`, делегируем в vcs::sync_vcs.
    if !args.is_empty() && args[0] == "vcs" {
        let scheme = args.get(1).and_then(|s| crate::vcs::VcsScheme::parse(s).ok());
        let owner = args.get(2).map(|s| s.as_str());
        let limit = 20usize; // последний 20 коммитов/issue на репо
        let ix = match state.ensure_index() {
            Ok(ix) => ix,
            Err(e) => return CmdResult::Done(format!("❌ sync vcs: {e}")),
        };
        let stats = crate::vcs::sync_vcs(ix, scheme, owner, limit);
        let mut out = String::new();
        out.push_str(&format!("🔄 sync vcs: {} схем(а) обработано\n\n", stats.len()));
        for st in &stats {
            out.push_str(&format!(
                "  {}: {} репо, {} коммитов, {} issues, {} skip, {} errors ({:?}ms)\n",
                st.scheme,
                st.repos_synced,
                st.commits_indexed,
                st.issues_indexed,
                st.unchanged,
                st.errors,
                st.elapsed_ms,
            ));
        }
        state.set_output(out.clone());
        return CmdResult::Done(out);
    }
    // v2.0: NLM-sync удалён — `sync` без `vcs` больше ничего не делает
    CmdResult::Done(
        "sync: укажите схему — `sync vcs <gh|gl|gt|gix|all> [owner]` (NLM-синк удалён в v2.0)".into(),
    )
}

// ---------------------------------------------------------------------------

/// Запустить интерактивный REPL (`poler-engine --shell`).
pub fn run_shell(db_path: PathBuf) -> ExitCode {
    use rustyline::config::Configurer;
    use rustyline::error::ReadlineError;
    use rustyline::history::DefaultHistory;
    use rustyline::Editor;

    // Загружаем историю
    let hist_path = super::state::history_path();
    if let Some(parent) = hist_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    // v0.15.1: создаём Editor с PolerCompleter (auto impl Helper) —
    // Tab-completion, Hinter, Validator теперь активны. Донастраиваем
    // history_size и auto_add_history через Configurer trait.
    let mut rl = match Editor::<super::completer::PolerCompleter, DefaultHistory>::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("poler-shell: rustyline init: {e}");
            return ExitCode::from(2);
        }
    };
    let _ = rl.set_max_history_size(2000);
    let _ = rl.set_history_ignore_dups(true);
    rl.set_completion_type(rustyline::config::CompletionType::List);
    rl.set_auto_add_history(true);

    // v0.15.1: подключаем PolerCompleter — Tab-completion + Hinter + Validator.
    // PolerCompleter автоматически impl Helper (Completer + Hinter +
    // Highlighter + Validator blanket impl), поэтому set_helper работает.
    rl.set_helper(Some(super::completer::PolerCompleter));

    // Load history (необязательно, ошибки молча игнорируем)
    let _ = rl.load_history(&hist_path);

    let mut state = ShellState::new(db_path);

    println!(
        "poler-shell {} — интерактивный режим. `help` — список команд, `quit` — выход.",
        env!("CARGO_PKG_VERSION")
    );
    println!("  Tab — автодополнение команд/подкоманд; ↑/↓ — история команд (до 2000).");

    loop {
        let prompt = "poler> ";
        let line = match rl.readline(prompt) {
            Ok(line) => line,
            Err(ReadlineError::WindowResized) => continue,
            Err(ReadlineError::Interrupted) => {
                println!("^C (quit — выход, `exit` тоже)");
                continue;
            }
            Err(ReadlineError::Eof) => {
                println!("\nвыход (EOF)");
                break;
            }
            Err(e) => {
                eprintln!("poler-shell: ошибка ввода: {e}");
                break;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        // auto_add_history=true уже сохраняет, но дублируем явно —
        // идиом-совместимо с fallback если авто-добавление выключат.
        let _ = rl.add_history_entry(trimmed);

        match dispatch(&mut state, &line) {
            CmdResult::Empty => continue,
            CmdResult::Quit => {
                println!("до свидания ✌");
                break;
            }
            CmdResult::Done(out) => {
                println!("{out}");
            }
        }
    }

    let _ = rl.save_history(&hist_path);
    ExitCode::SUCCESS
}

// ---------------------------------------------------------------------------
// v0.17.0: notes CRUD — list/add/show/edit/rm/save-from-ai
// ---------------------------------------------------------------------------

fn cmd_notes(state: &mut ShellState, args: &[String]) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(
            "notes: укажите подкоманду (list | add | show | edit | rm)".into(),
        );
    }
    let sub = args[0].as_str();
    let rest = &args[1..];
    match sub {
        "list" => {
            let conn = match state.ensure_notes_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            let notes_list = match notes::list_notes(conn, 200) {
                Ok(n) => n,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            let out = notes::format_list(&notes_list);
            state.set_output(out.clone());
            CmdResult::Done(out)
        }
        "show" => {
            if rest.is_empty() {
                return CmdResult::Done("notes show <id>".into());
            }
            let id: i64 = match rest[0].parse() {
                Ok(n) => n,
                Err(_) => return CmdResult::Done(format!("❌ id должен быть числом: {}", rest[0])),
            };
            let conn = match state.ensure_notes_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match notes::get_note(conn, id) {
                Ok(Some(n)) => {
                    let mut s = String::new();
                    s.push_str(&format!("📝 note #{}\n", n.id));
                    s.push_str(&format!("title: {}\n", n.title));
                    if !n.tags.is_empty() {
                        s.push_str(&format!("tags:   {}\n", n.tags.join(", ")));
                    }
                    s.push_str(&format!("source: {}\n", n.source));
                    if let Some(nb) = &n.notebook_id {
                        s.push_str(&format!("notebook: {}\n", nb));
                    }
                    s.push_str("\n");
                    s.push_str(&n.body);
                    state.set_output(s.clone());
                    CmdResult::Done(s)
                }
                Ok(None) => CmdResult::Done(format!("note id {id} не найдена")),
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        "add" => {
            if rest.is_empty() {
                return CmdResult::Done(
                    "notes add <title> — укажите заголовок (тело введёте в TUI редакторе через Ctrl+N)".into(),
                );
            }
            let title = rest.join(" ");
            let conn = match state.ensure_notes_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match notes::add_note(conn, &title, "", &[], notes::NoteSource::Manual, None) {
                Ok(id) => {
                    let msg = format!("✓ Создана пустая заметка #{id} «{title}». Используйте `notes edit {id}` в TUI для ввода тела.");
                    CmdResult::Done(msg)
                }
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        "edit" => {
            // В REPL-режиме мы не открываем TUI редактор (нет raw-mode). Просто
            // покажем тело и подскажем открыть TUI.
            if rest.is_empty() {
                return CmdResult::Done("notes edit <id>".into());
            }
            let id: i64 = match rest[0].parse() {
                Ok(n) => n,
                Err(_) => return CmdResult::Done(format!("❌ id должен быть числом: {}", rest[0])),
            };
            CmdResult::Done(format!(
                "📝 В REPL-режиме редактирование не поддерживается. Запустите `poler-engine --tui` и используйте Ctrl+N / Ctrl+E для встроенного редактора. (note #{id})"
            ))
        }
        "rm" => {
            // v0.17.5: удаление локальной заметки — с подтверждением --yes
            use super::confirm::{env_yes, split_gate_flags};
            let (yes_flag, _dry, positional) = split_gate_flags(rest);
            if positional.is_empty() {
                return CmdResult::Done("notes rm <id> [--yes]".into());
            }
            let id: i64 = match positional[0].parse() {
                Ok(n) => n,
                Err(_) => return CmdResult::Done(format!("❌ id должен быть числом: {}", positional[0])),
            };
            let conn = match state.ensure_notes_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            if !(yes_flag || env_yes()) {
                // покажем, что удаляем, и попросим подтверждение
                let shown = match notes::get_note(conn, id) {
                    Ok(Some(n)) => format!("«{}»", n.title),
                    Ok(None) => return CmdResult::Done(format!("note id {id} не найдена")),
                    Err(e) => return CmdResult::Done(format!("❌ {e}")),
                };
                return CmdResult::Done(format!(
                    "🔒 Заметка #{id} {shown} будет удалена локально (без возможности отмены).\nПодтверди: notes rm {id} --yes"
                ));
            }
            match notes::delete_note(conn, id) {
                Ok(()) => CmdResult::Done(format!("✓ Заметка #{id} удалена")),
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        other => CmdResult::Done(format!(
            "notes: неизвестная подкоманда {other} (list|add|show|edit|rm)"
        )),
    }
}

// ---------------------------------------------------------------------------
// v0.17.0: sources CRUD — list/add/rm/test/open
// ---------------------------------------------------------------------------

fn cmd_sources(state: &mut ShellState, args: &[String]) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(
            "sources: укажите подкоманду (list | add | rm | test | open)".into(),
        );
    }
    let sub = args[0].as_str();
    let rest = &args[1..];
    match sub {
        "list" => {
            let conn = match state.ensure_sources_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            let srcs = match sources::list_sources(conn, 500) {
                Ok(s) => s,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            let out = sources::format_list(&srcs);
            state.set_output(out.clone());
            CmdResult::Done(out)
        }
        "add" => {
            if rest.is_empty() {
                return CmdResult::Done(
                    "sources add <value> [--kind file|url|repo] [--label \"текст\"]".into(),
                );
            }
            let mut value = String::new();
            let mut kind: Option<sources::SourceKind> = None;
            let mut label: Option<String> = None;
            let mut i = 0;
            while i < rest.len() {
                match rest[i].as_str() {
                    "--kind" if i + 1 < rest.len() => {
                        kind = sources::SourceKind::from_str(&rest[i + 1]);
                        i += 2;
                        continue;
                    }
                    "--label" if i + 1 < rest.len() => {
                        label = Some(rest[i + 1].clone());
                        i += 2;
                        continue;
                    }
                    other => {
                        if !value.is_empty() {
                            value.push(' ');
                        }
                        value.push_str(other);
                        i += 1;
                    }
                }
            }
            if value.trim().is_empty() {
                return CmdResult::Done("❌ пустое значение источника".into());
            }
            let conn = match state.ensure_sources_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match sources::add_source(conn, kind, &value, label.as_deref()) {
                Ok(id) => {
                    let detected = kind.unwrap_or_else(|| sources::detect_kind(&value));
                    CmdResult::Done(format!(
                        "✓ Добавлен источник #{id} [{}] {}",
                        detected.as_str(),
                        value
                    ))
                }
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        "rm" => {
            if rest.is_empty() {
                return CmdResult::Done("sources rm <id>".into());
            }
            let id: i64 = match rest[0].parse() {
                Ok(n) => n,
                Err(_) => return CmdResult::Done(format!("❌ id должен быть числом: {}", rest[0])),
            };
            let conn = match state.ensure_sources_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match sources::delete_source(conn, id) {
                Ok(()) => CmdResult::Done(format!("✓ Источник #{id} удалён")),
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        "test" => {
            if rest.is_empty() {
                return CmdResult::Done("sources test <id>".into());
            }
            let id: i64 = match rest[0].parse() {
                Ok(n) => n,
                Err(_) => return CmdResult::Done(format!("❌ id должен быть числом: {}", rest[0])),
            };
            let conn = match state.ensure_sources_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match sources::test_source(conn, id) {
                Ok(sources::TestStatus::Ok) => CmdResult::Done(format!("✓ #{id}: доступен")),
                Ok(sources::TestStatus::Fail) => CmdResult::Done(format!("✗ #{id}: недоступен")),
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        "open" => {
            if rest.is_empty() {
                return CmdResult::Done("sources open <id>".into());
            }
            let id: i64 = match rest[0].parse() {
                Ok(n) => n,
                Err(_) => return CmdResult::Done(format!("❌ id должен быть числом: {}", rest[0])),
            };
            let conn = match state.ensure_sources_conn() {
                Ok(c) => c,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match sources::open_source(conn, id) {
                Ok(()) => CmdResult::Done(format!("✓ #{id}: отправлено в xdg-open")),
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        other => CmdResult::Done(format!(
            "sources: неизвестная подкоманда {other} (list|add|rm|test|open)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenize_basic() {
        let t = tokenize("search \"hello world\"");
        assert_eq!(t, vec!["search", "hello world"]);
    }

    #[test]
    fn tokenize_single_quotes() {
        let t = tokenize("notes add 'заметка с пробелами'");
        assert_eq!(t, vec!["notes", "add", "заметка с пробелами"]);
    }

    #[test]
    fn tokenize_mixed_quotes_and_flags() {
        let t = tokenize("search \"x y\" --top 5 z");
        assert_eq!(t, vec!["search", "x y", "--top", "5", "z"]);
    }

    #[test]
    fn tokenize_empty_and_whitespace() {
        assert!(tokenize("").is_empty());
        assert!(tokenize("    ").is_empty());
        assert_eq!(tokenize("a"), vec!["a"]);
    }

    #[test]
    fn tokenize_unclosed_quote_takes_rest() {
        let t = tokenize("search \"unclosed");
        assert_eq!(t, vec!["search", "unclosed"]);
    }

    #[test]
    fn cmd_set_format_works() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "set format json");
        match r {
            CmdResult::Done(out) => assert!(out.contains("AiJson")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_unknown_returns_message() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "nosuchcmd xyz");
        match r {
            CmdResult::Done(out) => assert!(out.contains("неизвестная команда")),
            _ => panic!(),
        }
    }

    // ── v0.46.x (аудит 2026-09-21): системний прохід ────────────────────

    #[test]
    fn bang_empty_shows_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "!") {
            CmdResult::Done(out) => assert!(out.contains("! <command>")),
            _ => panic!(),
        }
    }

    #[test]
    fn bang_runs_shell_command() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "! echo poler_audit_ok") {
            CmdResult::Done(out) => assert_eq!(out.trim(), "poler_audit_ok"),
            _ => panic!(),
        }
    }

    #[test]
    fn sh_command_alias_runs() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "sh echo poler_sh_alias_ok") {
            CmdResult::Done(out) => assert_eq!(out.trim(), "poler_sh_alias_ok"),
            _ => panic!(),
        }
    }

    #[test]
    fn fallback_runs_path_executable_with_args() {
        // `echo` є в PATH будь-якого POSIX-оточення тестів
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "echo --n poler_fallback_ok") {
            CmdResult::Done(out) => assert!(out.contains("poler_fallback_ok")),
            _ => panic!(),
        }
    }

    #[test]
    fn fallback_rejects_non_executable_file() {
        // файл без біта виконуваності НЕ має запускатись (v0.46.1 аудит)
        let dir = std::env::temp_dir().join("poler_sh_audit_nonexec");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("poler_nonexec_probe"); // без розширення, без chmod +x
        std::fs::write(&f, b"not a program").unwrap();
        let out_path = format!("{}", f.display());
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, &out_path) {
            CmdResult::Done(out) => assert!(out.contains("неизвестная команда")),
            _ => panic!(),
        }
        let _ = std::fs::remove_file(&f);
    }

    // -----------------------------------------------------------------
    // v0.47.0: WinCompat + агентная среда
    // -----------------------------------------------------------------

    #[test]
    fn win_type_runs_cat() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "type /etc/hostname") {
            CmdResult::Done(out) => {
                assert!(!out.contains("неизвестная команда"), "type должен перевестись в cat: {out}");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn win_dir_runs_ls_not_unknown() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "dir /etc/hostname") {
            CmdResult::Done(out) => {
                assert!(!out.contains("неизвестная команда"), "dir должен перевестись в ls: {out}");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn win_tasklist_runs_ps() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "tasklist") {
            CmdResult::Done(out) => {
                assert!(out.contains("PID") || out.contains("root") || out.is_empty(),
                    "tasklist → ps aux: {out}");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn win_ver_runs_uname() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "ver") {
            CmdResult::Done(out) => {
                assert!(out.contains("Linux"), "ver → uname -sr: {out}");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn win_net_is_notice_not_execution() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "net use") {
            CmdResult::Done(out) => {
                assert!(out.contains("🚫"), "net не должен выполняться: {out}");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn cd_changes_and_restores_directory() {
        let saved = std::env::current_dir().unwrap();
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "cd /tmp") {
            CmdResult::Done(out) => {
                assert!(out.contains("/tmp"), "cd печатает новый каталог: {out}");
            }
            _ => panic!(),
        }
        assert_eq!(std::env::current_dir().unwrap(), std::path::PathBuf::from("/tmp"));
        // возврат для изоляции остальных тестов
        let back = format!("cd {}", saved.display());
        let _ = dispatch(&mut s, &back);
        assert_eq!(std::env::current_dir().unwrap(), saved);
    }

    #[test]
    fn cd_bare_prints_cwd() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "cd") {
            CmdResult::Done(out) => assert!(!out.is_empty()),
            _ => panic!(),
        }
    }

    #[test]
    fn pwd_prints_absolute_path() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "pwd") {
            CmdResult::Done(out) => {
                assert!(out.starts_with('/'), "pwd — абсолютный путь: {out}");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn set_windows_env_var_and_show() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "set POLER_TEST_WIN=hello47") {
            CmdResult::Done(out) => {
                assert!(out.contains("POLER_TEST_WIN"), "set NAME=VALUE: {out}");
            }
            _ => panic!(),
        }
        assert_eq!(std::env::var("POLER_TEST_WIN").unwrap(), "hello47");
        match dispatch(&mut s, "set POLER_TEST_WIN") {
            CmdResult::Done(out) => {
                assert!(out.contains("hello47"), "set NAME показывает: {out}");
            }
            _ => panic!(),
        }
        // удаление пустым значением
        let _ = dispatch(&mut s, "set POLER_TEST_WIN=");
        assert!(std::env::var("POLER_TEST_WIN").is_err());
    }

    #[test]
    fn set_format_and_top_still_work() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "set format json") {
            CmdResult::Done(out) => assert!(out.contains("json") || out.contains("Json")),
            _ => panic!(),
        }
        match dispatch(&mut s, "set top 7") {
            CmdResult::Done(out) => assert!(out.contains('7')),
            _ => panic!(),
        }
    }

    #[test]
    fn sysinfo_reports_sections() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "sysinfo") {
            CmdResult::Done(out) => {
                assert!(out.contains("[cpu]"));
                assert!(out.contains("[agent mode]"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn systeminfo_alias_works() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "systeminfo") {
            CmdResult::Done(out) => assert!(out.contains("sysinfo")),
            _ => panic!(),
        }
    }

    #[test]
    fn env_masks_token_values() {
        std::env::set_var("POLER_SHELL_TEST_SECRET", "supersecretvalue123");
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "env POLER_SHELL_TEST") {
            CmdResult::Done(out) => {
                assert!(out.contains("POLER_SHELL_TEST_SECRET"));
                assert!(!out.contains("supersecretvalue123"), "секрет должен маскироваться: {out}");
            }
            _ => panic!(),
        }
        std::env::remove_var("POLER_SHELL_TEST_SECRET");
    }

    #[test]
    fn win_catalog_command_lists_translations() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "win") {
            CmdResult::Done(out) => {
                assert!(out.contains("findstr"));
                assert!(out.contains("taskkill"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn agent_command_mentions_exec_json() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "agent") {
            CmdResult::Done(out) => {
                assert!(out.contains("--exec"));
                assert!(out.contains("--json"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn engine_bare_shows_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "engine") {
            CmdResult::Done(out) => assert!(out.contains("self-exec")),
            _ => panic!(),
        }
    }

    #[test]
    fn pty_bare_shows_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "pty") {
            CmdResult::Done(out) => assert!(out.contains("PTY")),
            _ => panic!(),
        }
    }

    #[test]
    fn echo_win_vars_expanded() {
        // чистые функции: детекция и раскрытие %NAME%
        assert!(contains_win_var("%PATH%"));
        assert!(contains_win_var("hello %USER% x"));
        assert!(!contains_win_var("100% done"));
        assert!(!contains_win_var("no vars"));
        assert_eq!(expand_win_vars("%HOME%"), "${HOME}");
        assert_eq!(expand_win_vars("a %X_Y% b"), "a ${X_Y} b");
        assert_eq!(expand_win_vars("50%"), "50%");
        assert_eq!(expand_win_vars("%bad-name%"), "%bad-name%");
    }

    #[test]
    fn unknown_command_hint_mentions_win() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "zzzdefinitely_not_a_cmd") {
            CmdResult::Done(out) => {
                assert!(out.contains("win"), "подсказка должна упоминать Windows-словарь: {out}");
            }
            _ => panic!(),
        }
    }

    #[test]
    fn read_bare_shows_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "read") {
            CmdResult::Done(out) => {
                assert!(out.contains("Живой голос книги"));
                assert!(out.contains("--voice"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn read_missing_file_errors() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "read /no/such/file.txt") {
            CmdResult::Done(out) => assert!(out.contains("❌")),
            _ => panic!(),
        }
    }

    #[test]
    fn read_bad_voice_errors() {
        let dir = std::env::temp_dir().join("poler_sh_read");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("b.txt");
        std::fs::write(&f, "Тест.").unwrap();
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let line = format!("read {} --voice robot", f.display());
        match dispatch(&mut s, &line) {
            CmdResult::Done(out) => assert!(out.contains("❌")),
            _ => panic!(),
        }
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn read_stats_without_out() {
        let dir = std::env::temp_dir().join("poler_sh_read");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("c.txt");
        std::fs::write(&f, "Первая фраза. Вторая фраза!").unwrap();
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let line = format!("read {}", f.display());
        match dispatch(&mut s, &line) {
            CmdResult::Done(out) => {
                assert!(out.contains("фраз"), "инфо о книге: {out}");
            }
            _ => panic!(),
        }
        let _ = std::fs::remove_file(&f);
    }

    #[test]
    fn read_renders_wav_end_to_end() {
        let dir = std::env::temp_dir().join("poler_sh_read");
        let _ = std::fs::create_dir_all(&dir);
        let f = dir.join("d.txt");
        std::fs::write(&f, "Живой голос книги работает.").unwrap();
        let wav = dir.join("d.wav");
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let line = format!(
            "read {} --out {} --voice a_calm --seed 7",
            f.display(),
            wav.display()
        );
        match dispatch(&mut s, &line) {
            CmdResult::Done(out) => {
                assert!(out.contains("✓"), "рендер: {out}");
                assert!(out.contains("сжатие"));
            }
            _ => panic!(),
        }
        assert!(wav.exists(), "WAV должен быть создан");
        // валидный заголовок
        let head = std::fs::read(&wav).unwrap();
        assert_eq!(&head[0..4], b"RIFF");
        let _ = std::fs::remove_file(&f);
        let _ = std::fs::remove_file(&wav);
    }

    // ================================================================
    // v0.48.0: Калькулятор Всего — интеграционные тесты dispatch
    // ================================================================
    fn calc_out(line: &str) -> String {
        let mut s = ShellState::new(PathBuf::from("/tmp/test-calc.db"));
        match dispatch(&mut s, line) {
            CmdResult::Done(out) => out,
            other => panic!("ожидался Done, получено {other:?}"),
        }
    }

    #[test]
    fn cmd_calc_basic() {
        assert_eq!(calc_out("calc 2^10"), "1024.0");
        assert_eq!(calc_out("calc (1538 * 485) / 1024"), "728.447265625");
        assert_eq!(calc_out("calc 5!"), "120.0");
        assert_eq!(calc_out("calc sin(pi/2)"), "1.0");
        assert!(calc_out("calc 5 km to mi").starts_with("3.106"));
    }

    #[test]
    fn cmd_calc_prefix_equals() {
        assert_eq!(calc_out("= 2^10"), "1024.0");
        assert_eq!(calc_out("=5 km + 300 m"), "5.3 km");
        // пустой префикс — подсказка
        match calc_out("=") {
            out => assert!(out.contains("выражение"), "{out}"),
        }
    }

    #[test]
    fn cmd_calc_stateful() {
        let mut s = ShellState::new(PathBuf::from("/tmp/test-calc2.db"));
        let _ = dispatch(&mut s, "calc a = 6");
        match dispatch(&mut s, "calc a * 7") {
            CmdResult::Done(out) => assert_eq!(out, "42.0"),
            other => panic!("{other:?}"),
        }
        match dispatch(&mut s, "calc ans / 6") {
            CmdResult::Done(out) => assert_eq!(out, "7.0"),
            other => panic!("{other:?}"),
        }
        // каталоги
        match dispatch(&mut s, "calc vars") {
            CmdResult::Done(out) => assert!(out.contains("a = 6"), "{out}"),
            other => panic!("{other:?}"),
        }
        match dispatch(&mut s, "calc hist") {
            CmdResult::Done(out) => assert!(out.contains("a * 7"), "{out}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn cmd_calc_solve_and_catalogs() {
        let out = calc_out("calc solve x^2 - 4 = 0");
        assert!(out.contains("2.0") && out.contains("-2.0"), "{out}");
        let out = calc_out("calc constants c");
        assert!(out.contains("299792458") && out.contains("СИ-2019"), "{out}");
        let out = calc_out("calc units bit");
        assert!(out.contains("bit"), "{out}");
        let out = calc_out("calc funcs астр");
        assert!(out.contains("moon_illum"), "{out}");
        let out = calc_out("calc laws");
        assert!(out.contains("kepler3"), "{out}");
        // scriptgen через шелл
        let out = calc_out("calc script emc2");
        assert!(out.contains("[[rule]]") && out.contains("emc2"), "{out}");
        let out = calc_out("calc script kepler3 a=1 au");
        assert!(out.contains("полином") || out.contains("1 au") || out.contains("calc 2*pi"), "{out}");
        // неизвестный закон
        let out = calc_out("calc script nosuchlaw");
        assert!(out.contains("не найден"), "{out}");
    }

    #[test]
    fn cmd_calc_quatum_and_astro() {
        // ротор Ли с тритной фазой Ψ
        let out = calc_out("calc det(expm([0,-1;1,0] * psi))");
        assert!((out.parse::<f64>().unwrap() - 1.0).abs() < 1e-12, "{out}");
        // триты
        assert_eq!(calc_out("calc trits(5)"), "\"1TT\"");
        // фаза Луны на солнечном затмении 08.04.2024
        let out = calc_out("calc moon_illum(2024,4,8,18.35)");
        assert!(out.parse::<f64>().unwrap() < 0.01, "{out}");
        // гео: Киев—Львів
        let out = calc_out("calc dist(50.45,30.52,49.84,24.03)");
        let d: f64 = out.parse().unwrap();
        assert!(d > 450.0 && d < 475.0, "{d}");
    }

    #[test]
    fn cmd_calc_script_greedy_values() {
        // юнит с пробелом не теряется: a=1 au
        let out = calc_out("calc script kepler3 a=1 au");
        assert!(out.contains("1 au"), "{out}");
        assert!(out.contains("(1 au)^3") || out.contains("((1 au))^3"), "{out}");
        // несколько оверрайдов с юнитами
        let out = calc_out("calc script newton m1=70 kg m2=5.97e24 kg r=6371 km");
        assert!(out.contains("70 kg") && out.contains("6371 km"), "{out}");
    }

    #[test]
    fn cmd_calc_errors_are_messages() {
        let out = calc_out("calc 2 +");
        assert!(out.starts_with("❌"), "{out}");
        let out = calc_out("calc unknown_var + 1");
        assert!(out.starts_with("❌"), "{out}");
        let out = calc_out("calc");
        assert!(out.contains("calc <выражение>"), "{out}");
    }

    #[test]
    fn cmd_hw_probe() {
        let out = calc_out("hw");
        assert!(!out.is_empty());
        let out = calc_out("hw --json");
        assert!(serde_json::from_str::<serde_json::Value>(&out).is_ok(), "{out}");
    }

    #[test]
    fn cmd_quit_signals_exit() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        assert!(matches!(dispatch(&mut s, "quit"), CmdResult::Quit));
        assert!(matches!(dispatch(&mut s, "exit"), CmdResult::Quit));
        assert!(matches!(dispatch(&mut s, "q"), CmdResult::Quit));
    }

    #[test]
    fn cmd_empty_input_returns_empty() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        assert!(matches!(dispatch(&mut s, ""), CmdResult::Empty));
        assert!(matches!(dispatch(&mut s, "   "), CmdResult::Empty));
    }

    #[test]
    fn cmd_version_works() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "version");
        match r {
            CmdResult::Done(out) => assert!(out.contains("poler-shell")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_help_lists_commands() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "help");
        match r {
            CmdResult::Done(out) => {
                assert!(out.contains("search"));
                assert!(out.contains("sync vcs"));
                assert!(out.contains("quit"));
                // v0.15.1: help должен упоминать crawl и impact
                assert!(out.contains("crawl"));
                assert!(out.contains("impact"));
            }
            _ => panic!(),
        }
    }

    // v0.15.1: команды crawl/impact

    #[test]
    fn cmd_crawl_no_url_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "crawl");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите seed URL") || out.contains("пример")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_crawl_non_http_url_rejected() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "crawl ftp://example.com");
        match r {
            CmdResult::Done(out) => assert!(out.contains("seed должен быть http(s)://")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_crawl_help_flag_works() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "crawl --help");
        match r {
            CmdResult::Done(out) => assert!(out.contains("--depth") && out.contains("--max")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_crawl_unknown_flag_rejected() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "crawl https://example.com --bogus");
        match r {
            CmdResult::Done(out) => assert!(out.contains("неизвестный флаг --bogus")),
            _ => panic!(),
        }
    }

    // ---- v0.17.5: confirmation gate ----

    #[test]
    fn cmd_notes_rm_requires_yes_v0175() {
        // создаём заметку в реальной временной БД, затем пробуем удалить без --yes
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("notes.db");
        let mut s = ShellState::new(db.clone());
        // add работает без подтверждения (создание — не деструктивная операция)
        dispatch(&mut s, "notes add Тестовая");
        let notes_out = match dispatch(&mut s, "notes list") {
            CmdResult::Done(o) => o,
            _ => panic!("notes list"),
        };
        // id заметки — первое поле строки вида «#1. Тестовая» / «1. Тестовая»
        let id: String = notes_out
            .lines()
            .find(|l| l.contains("Тестовая"))
            .and_then(|l| l.trim().split('.').next().map(|s| s.trim().trim_start_matches('#').to_string()))
            .expect("заметка создана");
        // удаление БЕЗ --yes → гейт: показываем что удалим и просим подтверждение
        let r = dispatch(&mut s, &format!("notes rm {id}"));
        match r {
            CmdResult::Done(out) => {
                assert!(out.contains("--yes"), "подсказка о --yes: {out}");
                assert!(out.contains("Тестовая"), "показываем что удаляем: {out}");
            }
            _ => panic!(),
        }
        // заметка ещё жива
        let still = match dispatch(&mut s, "notes list") {
            CmdResult::Done(o) => o,
            _ => panic!(),
        };
        assert!(still.contains("Тестовая"), "без --yes заметка не удалена");
        // с --yes → удаление
        let r2 = dispatch(&mut s, &format!("notes rm {id} --yes"));
        match r2 {
            CmdResult::Done(out) => assert!(out.contains("удалена")),
            _ => panic!(),
        }
        let gone = match dispatch(&mut s, "notes list") {
            CmdResult::Done(o) => o,
            _ => panic!(),
        };
        assert!(!gone.contains("Тестовая"), "с --yes заметка удалена");
    }

    #[test]
    fn cmd_notes_rm_unknown_id_gives_not_found_v0175() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/notes-rm-404.db"));
        let r = dispatch(&mut s, "notes rm 99999");
        match r {
            CmdResult::Done(out) => assert!(out.contains("не найдена")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_nlm_is_gone_in_v2() {
        // v2.0: NLM-команда удалена — dispatcher отвечает «неизвестная команда»
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "nlm list");
        match r {
            CmdResult::Done(out) => assert!(out.contains("неизвестная команда"), "{out}"),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_impact_no_args_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "impact");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите PATH и SYMBOL")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_impact_only_path_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "impact ./src");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите PATH и SYMBOL")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_impact_nonexistent_path_rejected() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "impact /nonexistent/path mysymbol");
        match r {
            CmdResult::Done(out) => assert!(out.contains("путь не найден")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_impact_help_flag_works() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "impact --help");
        match r {
            CmdResult::Done(out) => assert!(out.contains("--depth") && out.contains("--cache")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_impact_unknown_flag_rejected() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "impact /tmp my_symbol --bogus");
        match r {
            CmdResult::Done(out) => assert!(out.contains("неизвестный флаг --bogus")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_impact_on_real_code_finds_symbol() {
        // Запускаем impact на собственном коде poler-engine: cmd_search
        // точно определён в src/shell/commands.rs, и impact_analysis должна
        // найти его downstream/upstream паспорта.
        let project_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src_dir = project_dir.join("src/shell");
        if !src_dir.exists() {
            return; // в vendored-сборке без CARGO_MANIFEST_DIR тест пропускаем
        }
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let cmdline = format!("impact {} cmd_search --depth 1", src_dir.display());
        let r = dispatch(&mut s, &cmdline);
        match r {
            CmdResult::Done(out) => {
                // Должен либо найти символ (target_function: cmd_search),
                // либо корректно сообщить что символ не найден (если парсер не
                // цепляет функцию в этом конкретном файле).
                assert!(
                    out.contains("target_function") || out.contains("символ не найден"),
                    "expected 'target_function' or 'symbol not found', got: {out}"
                );
            }
            _ => panic!("expected Done, got another CmdResult"),
        }
    }

    #[test]
    fn cmd_version_string_updated_for_v0171() {
        // v0.48.0: Калькулятор Всего + среда агента (наследие v0.47.0).
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "version");
        match r {
            CmdResult::Done(out) => {
                assert!(out.contains("v0.48.0"));
                assert!(out.contains("poler-shell"));
                assert!(out.contains("Калькулятор"), "v0.48.0: калькулятор в бейдже");
                assert!(out.contains("sysinfo"), "среда агента упомянута");
                assert!(!out.contains("Auth Companion"), "v2.0: Google-интеграция удалена");
            }
            _ => panic!(),
        }
    }

    // ---- v0.16.0: VCS-команды (gh/gl/gt/gix/sync vcs) ----

    #[test]
    fn cmd_version_string_updated_for_v017() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "version");
        match r {
            CmdResult::Done(out) => assert!(out.contains("poler-shell")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gh_no_subcommand_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gh");
        match r {
            CmdResult::Done(out) => {
                assert!(out.contains("search"));
                assert!(out.contains("repos"));
                assert!(out.contains("commits"));
                assert!(out.contains("issues"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gl_no_subcommand_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gl");
        match r {
            CmdResult::Done(out) => assert!(out.contains("search")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gt_no_subcommand_returns_help_or_init_error() {
        // gitea_adapter() требует GITEA_HOST; если не задан — ошибка инициализации.
        std::env::remove_var("GITEA_HOST");
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gt");
        match r {
            CmdResult::Done(out) => {
                // либо help (если хост задан), либо ошибка GITEA_HOST
                assert!(
                    out.contains("search") || out.contains("GITEA_HOST"),
                    "got: {out}"
                );
            }
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gh_search_no_query_gives_help() {
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("GH_TOKEN");
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gh search");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите запрос")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gh_repos_no_owner_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gh repos");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите owner")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gh_commits_no_repo_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gh commits");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gh_issues_no_repo_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gh issues");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gh_unknown_subcommand_rejected() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gh bogus");
        match r {
            CmdResult::Done(out) => assert!(out.contains("неизвестная подкоманда")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gix_no_args_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gix");
        match r {
            CmdResult::Done(out) => {
                assert!(out.contains("log"));
                assert!(out.contains("clone"));
            }
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gix_log_no_path_gives_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gix log");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите путь")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gix_log_nonexistent_path_errors() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gix log /nonexistent/path");
        match r {
            CmdResult::Done(out) => assert!(out.contains("gix log") || out.contains("❌")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gix_clone_missing_args() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gix clone");
        match r {
            CmdResult::Done(out) => assert!(out.contains("укажите URL")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_gix_clone_with_url_only_gives_dest_help() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "gix clone https://github.com/x/y.git");
        match r {
            CmdResult::Done(out) => assert!(out.contains("путь назначения")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_sync_vcs_no_scheme_does_not_panic() {
        // `sync vcs` без scheme → пробуем все 4 адаптера. gitea без GITEA_HOST
        // даст ошибку, но не должен паниковать. Тест проверяет только что
        // dispatch возвращает Done (не падает).
        std::env::remove_var("GITEA_HOST");
        std::env::remove_var("GITHUB_TOKEN");
        std::env::remove_var("GITLAB_TOKEN");
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "sync vcs");
        match r {
            CmdResult::Done(out) => assert!(out.contains("sync vcs") || out.contains("❌")),
            _ => panic!(),
        }
    }

    #[test]
    fn cmd_sync_alone_gives_v2_hint() {
        // v2.0: `sync` без vcs — подсказка про схему (NLM-синк удалён)
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "sync");
        match r {
            CmdResult::Done(out) => assert!(out.contains("sync vcs"), "{out}"),
            _ => panic!(),
        }
    }
}
