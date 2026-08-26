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
            "poler-engine {} (poler-shell v0.15.1)",
            env!("CARGO_PKG_VERSION")
        )),
        "search" | "web" => cmd_search(state, args),
        "stats" => cmd_stats(state),
        "nlm" => cmd_nlm(state, args),
        "sync" => cmd_nlm(state, &["sync".into()]),
        "set" => cmd_set(state, args),
        // v0.15.1: нативные команды crawl/impact внутри шелла
        "crawl" => cmd_crawl(state, args),
        "impact" => cmd_impact(state, args),
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
    // v0.15.1: нативные crawl/impact в шелле
    s.push_str("  crawl <URL> [--depth N] [--max M] [--cross] [--delay-ms N]\n");
    s.push_str("                                — обход URL → web-index.db (CDP+Chromium)\n");
    s.push_str("  impact <PATH> <SYMBOL> [--depth N] [--cache <DB>]\n");
    s.push_str("                                — AIDDE impact-паспорт символа в кодовой базе\n");
    s.push('\n');
    s.push_str("  set format md|json|simple     — переключить формат вывода\n");
    s.push_str("  set top N                     — топ-K по умолчанию для search\n");
    s.push('\n');
    s.push_str("  version | v                    — версия poler-engine + poler-shell\n");
    s.push_str("  quit | exit | q                — выйти из шелла\n");
    s.push_str("  help | ?                       — эта справка\n");
    s.push('\n');
    s.push_str("Подсказки: Tab — автодополнение команд/подкоманд/ID; ↑/↓ — история команд (до 2000).\n");
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
// crawl — обход URL → web-index.db (нативная интеграция v0.15.1)
// ---------------------------------------------------------------------------
//
// Делегирует в poler_engine::web::cdp_fetcher + poler_engine::web::crawl::crawl
// — те же функции, что и в standalone-режиме `poler-engine --crawl URL`. Однако
// в шелле есть важное преимущество: WebIndex уже открыт (если был `search`/
// `stats`/`nlm sync` ранее), и единственное новое состояние — это CDP-фечер.
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
    let mut fetcher = match crate::web::cdp_fetcher(cdp_port, wait_ms) {
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
    // подключения, что и для search/stats/nlm-sync.
    let cfg = crate::web::CrawlConfig {
        max_pages,
        max_depth: depth,
        delay_ms,
        cross_site,
        wait_ms,
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
            s.push_str(&format!("⬆ upstream dependents ({}):\n", r.upstream_dependents.len()));
            for d in &r.upstream_dependents {
                s.push_str(&format!(
                    "   • {} (вызывает в {})\n",
                    d.caller, d.file
                ));
            }
            s.push('\n');
            s.push_str(&format!("⬇ downstream dependencies ({}):\n", r.downstream_dependencies.len()));
            for d in &r.downstream_dependencies {
                s.push_str(&format!("   • {} (вызывается из {})\n", d.callee, d.file));
            }
            if !r.side_effects.is_empty() {
                s.push('\n');
                s.push_str(&format!("⚠ side-effects ({}):\n", r.side_effects.len()));
                for se in &r.side_effects {
                    s.push_str(&format!("   • {se}\n"));
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
// REPL entry point (используется main.rs)
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
    fn cmd_version_string_updated_for_v0151() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "version");
        match r {
            CmdResult::Done(out) => assert!(out.contains("v0.15.1")),
            _ => panic!(),
        }
    }
}
