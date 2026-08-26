//! Command parser + dispatcher для REPL/TUI. Принимает строку ввода
//! пользователя, парсит на команды и аргументы (с поддержкой кавычек),
//! вызывает соответствующий обработчик `ShellState`.
//!
//! Синтаксис:
//! ```text
//! poler> search "Касіопея Astra-Nic Complex" --top 5
//! poler> nlm list
//! poler> nlm notes 704f2610-c02b-4ec1-9fc7-a3b72dde2af1
//! poler> nlm ask 704f2610... "вопрос"
//! poler> nlm sync                       # синк всех ноутбуков
//! poler> nlm sync 704f2610...           # синк одного
//! poler> stats                          # статистика web-index
//! poler> set format json                # переключить формат
//! poler> set top 20                     # топ-K по умолчанию
//! poler> help                           # список команд
//! poler> quit | exit                    # выход
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use crate::google::nlm;
use crate::google::nlm_ingest;

use super::state::ShellState;

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
    let tokens = tokenize(line);
    if tokens.is_empty() {
        return CmdResult::Empty;
    }
    let cmd = tokens[0].as_str();
    let args = &tokens[1..];

    match cmd {
        "" => CmdResult::Empty,
        "quit" | "exit" | "q" => CmdResult::Quit,
        "help" | "?" => CmdResult::Done(help_text(args)),
        "version" | "v" => CmdResult::Done(format!(
            "poler-engine {} (poler-shell v0.15.0)",
            env!("CARGO_PKG_VERSION")
        )),
        "search" | "web" => cmd_search(state, args),
        "stats" => cmd_stats(state),
        "nlm" => cmd_nlm(state, args),
        "sync" => cmd_nlm(state, &["sync".into()]),
        "set" => cmd_set(state, args),
        // v0.15.1: placeholders для команд, требующих отдельной реализации
        "crawl" | "impact" => CmdResult::Done(format!(
            "⚠ {cmd}: эта команда доступна в v0.15.1; сейчас используй `poler-engine --crawl ...` или `--impact ...` в соседнем окне."
        )),
        other => CmdResult::Done(format!(
            "неизвестная команда: {other} (введите `help` для списка)"
        )),
    }
}

fn help_text(_args: &[String]) -> String {
    let mut s = String::new();
    s.push_str("poler-shell — доступные команды:\n\n");
    s.push_str("  search \"<query>\" [--top N]   — поиск по web-index.db (NLM+веб+локал)\n");
    s.push_str("  web \"<query>\"                — алиас для search\n");
    s.push_str("  stats                         — статистика web-index.db (страниц, байт, PageRank)\n");
    s.push('\n');
    s.push_str("  nlm list                      — список 87 ноутбуков аккаунта\n");
    s.push_str("  nlm notes <NB_ID>             — заметки/чат ноутбука (JSON)\n");
    s.push_str("  nlm artifacts <NB_ID>         — Studio-артефакты (Audio/Slide/Report/Video/Quiz)\n");
    s.push_str("  nlm source <NB_ID> <SRC_ID>   — контент источника + URL слайдов\n");
    s.push_str("  nlm account                    — email/настройки сессии\n");
    s.push_str("  nlm ask <NB_ID> \"<question>\" — ответ модели ПО ИСТОЧНИКАМ ноутбука\n");
    s.push_str("  nlm sync [<NB_ID>]            — синк NLM в web-index.db (без арг = все)\n");
    s.push('\n');
    s.push_str("  set format md|json|simple     — переключить формат вывода\n");
    s.push_str("  set top N                     — топ-K по умолчанию для search\n");
    s.push('\n');
    s.push_str("  version | v                    — версия poler-engine + poler-shell\n");
    s.push_str("  quit | exit | q                — выйти из шелла\n");
    s.push_str("  help | ?                       — эта справка\n");
    s.push('\n');
    s.push_str("Команды crawl/impact появятся в v0.15.1 (используйте `poler-engine --crawl/--impact` в соседнем окне).\n");
    s
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
    let hits = match ix.search(&query, top) {
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
        out.push_str("  - выполните `nlm sync` чтобы влить NotebookLM-корпус\n");
        out.push_str("  - или `poler-engine --crawl <URL>` в соседнем окне чтобы наполнить вебом\n");
    } else {
        for (i, h) in hits.iter().enumerate() {
            out.push_str(&format_hit(i + 1, h, &query));
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
// nlm ... — делегация в NlmSession + nlm_ingest
// ---------------------------------------------------------------------------

fn cmd_nlm(state: &mut ShellState, args: &[String]) -> CmdResult {
    if args.is_empty() {
        return CmdResult::Done(
            "nlm: укажите подкоманду (list | notes | artifacts | source | account | ask | sync)".into(),
        );
    }
    let sub = args[0].as_str();
    let rest = &args[1..];

    match sub {
        "list" | "notebooks" => {
            let s = match state.ensure_nlm() {
                Ok(s) => s,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match s.list_notebooks() {
                Ok(nbs) => {
                    let mut out = String::new();
                    out.push_str(&format!("📚 {} ноутбуков аккаунта:\n\n", nbs.len()));
                    out.push_str(&nlm::format_notebooks(&nbs));
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm list: {e}")),
            }
        }
        "notes" => {
            let Some(nb) = rest.first() else {
                return CmdResult::Done("nlm notes <NB_ID> — не указан ID ноутбука".into());
            };
            let s = match state.ensure_nlm() {
                Ok(s) => s,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match s.notes(nb) {
                Ok(v) => {
                    let out = serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into());
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm notes: {e}")),
            }
        }
        "artifacts" => {
            let Some(nb) = rest.first() else {
                return CmdResult::Done("nlm artifacts <NB_ID> — не указан ID ноутбука".into());
            };
            let s = match state.ensure_nlm() {
                Ok(s) => s,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match s.artifacts(nb) {
                Ok(arts) => {
                    let mut out = String::new();
                    out.push_str(&format!("🎨 {} Studio-артефактов в {nb}:\n\n", arts.len()));
                    out.push_str(&nlm::format_artifacts(&arts));
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm artifacts: {e}")),
            }
        }
        "source" => {
            if rest.len() < 2 {
                return CmdResult::Done(
                    "nlm source <NB_ID> <SRC_ID> — нужно 2 аргумента".into(),
                );
            }
            let (nb, src) = (rest[0].clone(), rest[1].clone());
            let s = match state.ensure_nlm() {
                Ok(s) => s,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match s.load_source(&nb, &src) {
                Ok(sc) => {
                    let out = nlm::format_source_content(&sc, &nb);
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm source: {e}")),
            }
        }
        "account" => {
            let s = match state.ensure_nlm() {
                Ok(s) => s,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match s.account() {
                Ok(v) => {
                    let out = serde_json::to_string_pretty(&v).unwrap_or_else(|_| "{}".into());
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm account: {e}")),
            }
        }
        "ask" => {
            if rest.len() < 2 {
                return CmdResult::Done(
                    "nlm ask <NB_ID> \"<question>\" — нужно 2 аргумента".into(),
                );
            }
            let (nb, q) = (rest[0].clone(), rest[1..].join(" "));
            let s = match state.ensure_nlm() {
                Ok(s) => s,
                Err(e) => return CmdResult::Done(format!("❌ {e}")),
            };
            match s.chat(&nb, &q) {
                Ok(answer) => {
                    let out = format!("💬 вопрос: {q}\n→ {answer}");
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm ask: {e}")),
            }
        }
        "sync" => {
            cmd_nlm_sync(state, rest)
        }
        "shot" | "media" => CmdResult::Done(format!(
            "⚠ nlm {sub}: в шелле используйте `poler-engine --nlm-{sub} <URL>` — команда требует отдельной сессии"
        )),
        other => CmdResult::Done(format!(
            "nlm: неизвестная подкоманда {other} (list|notes|artifacts|source|account|ask|sync)"
        )),
    }
}

fn cmd_nlm_sync(state: &mut ShellState, rest: &[String]) -> CmdResult {
    // Два `&mut` из одного `state` нельзя借用 одновременно — берём both
    // через closure-обёртку (см. `ShellState::with_nlm_index`).
    if rest.is_empty() {
        // синк всех ноутбуков
        let res = state.with_nlm_index(nlm_ingest::sync_all);
        match res {
            Ok(stats) => {
                let out = format_sync_stats(&stats, "all");
                state.set_output(out.clone());
                CmdResult::Done(out)
            }
            Err(e) => CmdResult::Done(format!("❌ nlm sync: {e}")),
        }
    } else {
        // синк одного ноутбука
        let nb_id = rest[0].clone();
        let res = state.with_nlm_index(|ix, s| -> Result<_, String> {
            let nbs = s.list_notebooks()?;
            let target = nbs.iter().find(|x| x.id == nb_id).cloned();
            let Some(nb_meta) = target else {
                return Err(format!("ноутбук {nb_id} не найден в аккаунте"));
            };
            let mut stats = nlm_ingest::IngestStats {
                notebooks: 1,
                ..Default::default()
            };
            if let Err(e) = nlm_ingest::ingest_notebook(ix, s, &nb_meta, &mut stats) {
                stats.errors.push(format!("ingest_notebook {nb_id}: {e}"));
            }
            let _ = ix.recompute_pagerank(20);
            Ok(stats)
        });
        match res {
            Ok(stats) => {
                let out = format_sync_stats(&stats, "single");
                state.set_output(out.clone());
                CmdResult::Done(out)
            }
            Err(e) => CmdResult::Done(format!("❌ nlm sync: {e}")),
        }
    }
}

fn format_sync_stats(s: &nlm_ingest::IngestStats, scope: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("🔄 NLM sync ({scope}): {} notebooks processed\n", s.notebooks));
    out.push_str(&format!(
        "  passports:   {} reindexed, {} unchanged\n",
        s.notebooks_reindexed, s.notebooks_unchanged
    ));
    out.push_str(&format!(
        "  sources:      {} reindexed, {} unchanged\n",
        s.sources_reindexed, s.sources_unchanged
    ));
    out.push_str(&format!(
        "  notes:        {} reindexed, {} unchanged\n",
        s.notes_reindexed, s.notes_unchanged
    ));
    out.push_str(&format!(
        "  artifacts:    {} reindexed, {} unchanged\n",
        s.artifacts_reindexed, s.artifacts_unchanged
    ));
    out.push_str(&format!(
        "  TOTAL:        {} pages ({} new/changed, {} skipped by Percolator-lite)\n",
        s.total_pages(),
        s.total_reindexed(),
        s.total_unchanged()
    ));
    if !s.errors.is_empty() {
        out.push_str(&format!("\n  errors ({}):\n", s.errors.len()));
        for e in &s.errors {
            out.push_str(&format!("    - {e}\n"));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// set — переключение настроек шелла
// ---------------------------------------------------------------------------

fn cmd_set(state: &mut ShellState, args: &[String]) -> CmdResult {
    if args.len() < 2 {
        return CmdResult::Done(
            "set <key> <value> — доступные ключи: format (md|json|simple), top (N)".into(),
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
// REPL entry point (используется main.rs)
// ---------------------------------------------------------------------------

/// Запустить интерактивный REPL (`poler-engine --shell`).
pub fn run_shell(db_path: PathBuf) -> ExitCode {
    use rustyline::error::ReadlineError;
    use rustyline::history::DefaultHistory;
    use rustyline::Editor;

    // Загружаем историю
    let hist_path = super::state::history_path();
    if let Some(parent) = hist_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    let mut rl = match Editor::<(), DefaultHistory>::new() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("poler-shell: rustyline init: {e}");
            return ExitCode::from(2);
        }
    };

    // Load history (необязательно, ошибки молча игнорируем)
    let _ = rl.load_history(&hist_path);

    let mut state = ShellState::new(db_path);

    println!(
        "poler-shell {} — интерактивный режим. `help` — список команд, `quit` — выход.",
        env!("CARGO_PKG_VERSION")
    );

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
        let t = tokenize("nlm ask abc 'вопрос с пробелами'");
        assert_eq!(t, vec!["nlm", "ask", "abc", "вопрос с пробелами"]);
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
                assert!(out.contains("nlm sync"));
                assert!(out.contains("quit"));
            }
            _ => panic!(),
        }
    }
}
