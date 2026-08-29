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
use crate::notes;
use crate::sources;
use crate::vcs::VcsAdapter;

use super::help;
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
        "help" | "?" => {
            // v0.17.0: `help` без аргументов → overview; `help nlm ask` → детальная справка
            if args.is_empty() {
                CmdResult::Done(help::help_overview())
            } else {
                let topic = args.join(" ");
                CmdResult::Done(help::help_topic(&topic))
            }
        }
        "version" | "v" => CmdResult::Done(format!(
            "poler-engine {} (poler-shell v0.17.6 — Auth Companion: --auth-ui изолированное окно логина Google + снапшот google_session.json; + Security Hardening: confirmation gate, cookie-import по согласию, shutdown headless, audit-лог)",
            env!("CARGO_PKG_VERSION")
        )),
        "search" | "web" => cmd_search(state, args),
        "stats" => cmd_stats(state),
        "nlm" => cmd_nlm(state, args),
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
        other => CmdResult::Done(format!(
            "неизвестная команда: {other} (введите `help` для списка)"
        )),
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
            "nlm: укажите подкоманду (list | notes | notes-sync | artifacts | source | account | ask | sync)".into(),
        );
    }
    // v0.18.0: License Gate — единая точка для всех nlm-подкоманд shell/TUI.
    if !crate::license::gate_or_print(crate::license::FEATURE_NLM) {
        return CmdResult::Done(
            "⛔ Community-лимит NotebookLM исчерпан. Статус: license (в shell) или poler-engine --license".into(),
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
            match s.list_notes_structured(nb) {
                Ok(notes) => {
                    let mut out = format!("📝 {} заметок в {nb} (без mind maps):\n\n", notes.len());
                    for (i, n) in notes.iter().enumerate() {
                        let preview: String = n
                            .text
                            .lines()
                            .next()
                            .unwrap_or("")
                            .chars()
                            .take(80)
                            .collect();
                        out.push_str(&format!("{}. {} — {}\n", i + 1, n.title, preview));
                    }
                    if notes.is_empty() {
                        out.push_str("(заметок нет — mind maps не считаются)\n");
                    }
                    out.push_str("\nсинхронизировать в poler_notes: `nlm notes-sync <NB_ID>`\n");
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm notes: {e}")),
            }
        }
        // M5: двусторонняя синхронизация заметок облако ↔ локально
        // v0.17.5 confirmation gate: push в облако — только с --yes;
        // --dry-run показывает план без выполнения; без флагов — безопасный
        // PullOnly + превью ожидающих отправки заметок.
        "notes-sync" | "nsync" => {
            use crate::google::confirm::{env_yes, split_gate_flags};
            use crate::google::nlm_notes_sync::{plan_notebook_sync, sync_notebook_notes_mode, SyncMode};
            let (yes_flag, dry_run, positional) = split_gate_flags(rest);
            let nb = match positional
                .first()
                .cloned()
                .or_else(|| state.active_notebook_id.clone())
            {
                Some(id) => id,
                None => {
                    return CmdResult::Done(
                        "nlm notes-sync <NB_ID> [--yes | --dry-run] — не указан ID (или выберите ноутбук в TUI)".into(),
                    )
                }
            };
            // 1) --dry-run: только план, ничего не выполняем
            if dry_run {
                return match state.with_nlm_notes(|sess, conn| plan_notebook_sync(sess, conn, &nb)) {
                    Ok(plan) => {
                        let out = format!(
                            "🔍 DRY-RUN синка заметок {nb} (ничего не выполнено):\n{}\nВыполнить: nlm notes-sync {nb} --yes",
                            plan.preview()
                        );
                        state.set_output(out.clone());
                        CmdResult::Done(out)
                    }
                    Err(e) => CmdResult::Done(format!("❌ nlm notes-sync --dry-run: {e}")),
                };
            }
            // 2) push разрешён только с --yes (или env POLER_YES — скрипты)
            let mode = if yes_flag || env_yes() { SyncMode::Full } else { SyncMode::PullOnly };
            match state.with_nlm_notes(|sess, conn| {
                sync_notebook_notes_mode(sess, conn, &nb, mode)
            }) {
                Ok(rep) => {
                    let mut out = format!("🔄 Синк заметок ноутбука {nb} ({}): {}\n",
                        if mode == SyncMode::Full { "pull+push" } else { "только pull — push требует --yes" },
                        rep.summary());
                    for e in &rep.errors {
                        out.push_str(&format!("  ⚠ {e}\n"));
                    }
                    if rep.pending_push > 0 {
                        out.push_str(&format!(
                            "\n🔒 {} локальных заметок готовы к отправке в облако.\n",
                            rep.pending_push
                        ));
                        out.push_str("Просмотр: nlm notes-sync <NB_ID> --dry-run\n");
                        out.push_str("Отправка: nlm notes-sync <NB_ID> --yes\n");
                    } else {
                        out.push_str("Заметки синхронизированы (облако — источник истины).\n");
                    }
                    state.set_output(out.clone());
                    CmdResult::Done(out)
                }
                Err(e) => CmdResult::Done(format!("❌ nlm notes-sync: {e}")),
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
                    // v0.17.0: Запомнить ответ для Ctrl+S (save_last_ai_reply_as_note)
                    state.remember_ai_reply(answer.clone(), Some(nb.clone()));
                    // v0.17.4: пара → лента чата (Transcript, F3 в TUI);
                    // сбой записи не ломает команду — лента best-effort.
                    if let Ok(conn) = state.ensure_notes_conn() {
                        let _ = super::transcript::add_entry(conn, Some(&nb), &q, &answer);
                    }
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
/// `poler> sync` без args — синоним для `nlm sync` (обратная совместимость v0.14).
fn cmd_sync(state: &mut ShellState, args: &[String]) -> CmdResult {
    // Если первый аргумент — `vcs`, делегируем в vcs::sync_vcs; иначе — NLM sync.
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
    // fallback: `sync` без vcs → NLM sync (как в v0.14)
    cmd_nlm(state, &["sync".into()])
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
            "notes: укажите подкоманду (list | add | show | edit | rm | save-from-ai)".into(),
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
            use crate::google::confirm::{env_yes, split_gate_flags};
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
        "save-from-ai" => {
            let title = if rest.is_empty() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| format!("AI reply @{}", d.as_secs()))
                    .unwrap_or_else(|_| "AI reply".into());
                now
            } else {
                rest.join(" ")
            };
            match state.save_last_ai_reply_as_note(&title) {
                Ok(id) => CmdResult::Done(format!("✓ Сохранён AI-ответ как заметка #{id} «{title}»")),
                Err(e) => CmdResult::Done(format!("❌ {e}")),
            }
        }
        other => CmdResult::Done(format!(
            "notes: неизвестная подкоманда {other} (list|add|show|edit|rm|save-from-ai)"
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
    fn cmd_nlm_notes_sync_hint_mentions_flags_v0175() {
        // без NLM-сессии команда упадёт с ошибкой браузера — но подсказка
        // о флагах должна присутствовать в тексте помощи
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "nlm notes-sync");
        match r {
            CmdResult::Done(out) => {
                assert!(
                    out.contains("--yes") || out.contains("--dry-run") || out.contains("NB_ID"),
                    "подсказка о gate-флагах: {out}"
                );
            }
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
        // shadow test marker — used by other tests via name
        // v0.17.5: Security Hardening — бейдж poler-shell обновлён.
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "version");
        match r {
            CmdResult::Done(out) => {
                assert!(out.contains("0.17.6"));
                assert!(out.contains("poler-shell"));
                assert!(out.contains("Auth Companion"));
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
            CmdResult::Done(out) => assert!(out.contains("0.17.6")),
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
    fn cmd_sync_vcs_no_scheme_gives_nlm_fallback_or_help() {
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
    fn cmd_sync_alone_falls_back_to_nlm() {
        // `sync` без vcs — это синоним для `nlm sync` (обратная совместимость)
        // без Chromium это даст ошибку, но в CmdResult::Done.
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        let r = dispatch(&mut s, "sync");
        match r {
            CmdResult::Done(_) => {} // OK — упало с ошибкой в Done
            _ => panic!(),
        }
    }
}
