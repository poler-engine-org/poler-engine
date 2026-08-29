//! MCP-сервер (Model Context Protocol) поверх stdio: poler-engine как
//! нативный инструмент LLM-агентов — без обёрток, без прокладок,
//! один бинарь `poler-engine --mcp`.
//!
//! Транспорт: JSON-RPC 2.0, одно сообщение — одна строка (stdio-транспорт
//! MCP). Протокольные сообщения ТОЛЬКО в stdout; вся диагностика — stderr
//! (иначе клиент парсит мусор).
//!
//! Инструменты (агент сам выбирает цели — ни сайт, ни тема не фиксированы):
//!
//! * `poler_web_search` — поиск по постоянному веб-индексу (WebRank:
//!   BM25 + PageRank + title + ε-плотность, кириллический стемминг);
//! * `poler_crawl`      — обход выбранного агентом сайта в постоянный
//!   индекс (robots.txt, sitemap, SimHash-дедуп, PageRank, инкрементально);
//! * `poler_fetch`      — открыть любой URL в реальном Chromium (CDP):
//!   JS/SPA/Shadow DOM рендерятся, скрытые JSON API перехватываются;
//! * `poler_search`     — локальный резонансный POLER-поиск (ε/R/сцены);
//! * `poler_gmail`      — поиск в Gmail владельца через OAuth-токен
//!   (readonly, одноразовый `--google-auth`, пароль не проходит через движок);
//! * `poler_drive`      — файлы Google Drive через тот же OAuth-токен;
//! * `poler_nlm`        — ноутбуки NotebookLM владельца через персистентный
//!   профиль (протокол batchexecute, разведан из расширения NLMTools):
//!   ноутбуки/источники/заметки/Studio, чат с моделью ноутбука и
//!   медиа-канал (то, что API NLMTools не отдаёт — скачивает профильный
//!   Chromium: картинки слайдов, скриншоты страниц).
//!
//! Chromium поднимается автоматически при первом poler_crawl/poler_fetch
//! (см. `web::ensure_chromium`).

use std::io::{BufRead, Write};
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::web::{self, CdpFetcher, WebIndex};
use crate::{Engine, EngineConfig};

/// Cap рендер-текста в ответе poler_fetch: больше — бессмысленно для
/// контекстного окна агента (полный текст лежит в кэш-файле).
const FETCH_TEXT_CAP: usize = 24 * 1024;

/// Cap raw-JSON (заметки/аккаунт NotebookLM) в ответе poler_nlm.
const NLM_JSON_CAP: usize = 16 * 1024;

/// Безопасно усечь строку по границе символов.
fn cap_chars(s: &str, max: usize) -> (String, bool) {
    if s.chars().count() <= max {
        (s.to_string(), false)
    } else {
        (s.chars().take(max).collect(), true)
    }
}

pub struct McpServer {
    cdp_port: u16,
    wait_ms: u64,
    db_path: PathBuf,
}

impl McpServer {
    /// Конструктор для альтернативных транспортов (mcp_http: Streamable HTTP).
    pub fn new(cdp_port: u16, wait_ms: u64, db_path: PathBuf) -> Self {
        Self {
            cdp_port,
            wait_ms,
            db_path,
        }
    }
}

/// Запуск MCP-сервера (stdio). Возвращает код процесса (0 = чистый EOF stdin).
pub fn run(cdp_port: u16, wait_ms: u64, db_path: PathBuf) -> i32 {
    let server = McpServer::new(cdp_port, wait_ms, db_path);
    eprintln!(
        "poler-mcp: stdio JSON-RPC, db={:?}, cdp_port={}, wait_ms={}",
        server.db_path, server.cdp_port, server.wait_ms
    );
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                let _ = write_line(
                    &mut out,
                    &json!({"jsonrpc":"2.0","id":null,"error":{
                        "code":-32700,"message":format!("parse error: {e}")
                    }}),
                );
                continue;
            }
        };
        server.handle(&mut out, msg);
    }
    0
}

fn write_line(out: &mut impl Write, v: &Value) -> std::io::Result<()> {
    out.write_all(serde_json::to_string(v).unwrap_or_default().as_bytes())?;
    out.write_all(b"\n")?;
    out.flush()
}

impl McpServer {
    /// Диспетчер одного JSON-RPC-сообщения: возвращает ответ (None —
    /// уведомление без id, ответ не нужен). Общий для stdio и HTTP.
    pub fn dispatch(&self, msg: &Value) -> Option<Value> {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // уведомления (без id) не требуют ответа
        let Some(id) = id else {
            return None;
        };

        let result: Result<Value, (i64, String)> = match method {
            "initialize" => Ok(self.handle_initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools_manifest()})),
            "tools/call" => self.handle_tools_call(&params),
            "resources/list" => Ok(json!({"resources": []})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            _ => Err((-32601, format!("method not found: {method}"))),
        };

        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": code, "message": message}
            }),
        })
    }

    fn handle(&self, out: &mut impl Write, msg: Value) {
        if let Some(resp) = self.dispatch(&msg) {
            let _ = write_line(out, &resp);
        }
    }

    fn handle_initialize(&self, params: &Value) -> Value {
        // отвечаем версией клиента, если она известна, иначе свежей
        let client_pv = params
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("2024-11-05");
        json!({
            "protocolVersion": client_pv,
            "capabilities": {
                "tools": {}
            },
            "serverInfo": {
                "name": "poler-engine",
                "version": env!("CARGO_PKG_VERSION"),
                "title": "POLER Engine — AI-Native Resonant Web & Local Search"
            }
        })
    }

    fn handle_tools_call(&self, params: &Value) -> Result<Value, (i64, String)> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or((-32602, "params.name обязателен".into()))?;
        let args = params.get("arguments").cloned().unwrap_or(json!({}));
        let call = match name {
            "poler_web_search" => self.tool_web_search(&args),
            "poler_crawl" => self.tool_crawl(&args),
            "poler_fetch" => self.tool_fetch(&args),
            "poler_search" => self.tool_local_search(&args),
            "poler_gmail" => self.tool_gmail(&args),
            "poler_drive" => self.tool_drive(&args),
            "poler_nlm" => self.tool_nlm(&args),
            other => Err(format!("неизвестный инструмент: {other}")),
        };
        match call {
            Ok(text) => Ok(json!({
                "content": [{"type": "text", "text": text}],
                "isError": false
            })),
            Err(e) => Ok(json!({
                "content": [{"type": "text", "text": format!("ошибка: {e}")}],
                "isError": true
            })),
        }
    }

    // -----------------------------------------------------------------
    // poler_web_search: запрос → постоянный индекс → WebRank
    // -----------------------------------------------------------------
    fn tool_web_search(&self, args: &Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("аргумент query (строка) обязателен")?;
        let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let top = top.clamp(1, 50);
        let mut ix = WebIndex::open(&self.db_path).map_err(|e| e.to_string())?;
        if ix.page_count() == 0 {
            return Ok(format!(
                "Веб-индекс пуст ({:?}). Сначала вызови poler_crawl с seed_url — \
                 агент сам выбирает, какой сайт обходить.",
                self.db_path
            ));
        }
        let hits = ix.search(query, top).map_err(|e| e.to_string())?;
        if hits.is_empty() {
            return Ok(format!(
                "Ничего не найдено по «{query}» (индекс: {} страниц). \
                 Попробуй другие термы или докрауль релевантный сайт.",
                ix.page_count()
            ));
        }
        let mut out = format!("poler web-search «{query}»: {} из {} страниц\n\n", hits.len(), ix.page_count());
        for (i, h) in hits.iter().enumerate() {
            out.push_str(&format!(
                "{}. [{:.4}] {} — {}\n   {}\n\n",
                i + 1,
                h.score,
                h.url,
                if h.title.is_empty() { "-" } else { &h.title },
                h.snippet
            ));
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // poler_crawl: seed URL → обход → постоянный индекс
    // -----------------------------------------------------------------
    fn tool_crawl(&self, args: &Value) -> Result<String, String> {
        let seed = args
            .get("seed_url")
            .and_then(|v| v.as_str())
            .ok_or("аргумент seed_url (http(s)://…) обязателен")?;
        if !seed.starts_with("http://") && !seed.starts_with("https://") {
            return Err(format!("seed_url должен быть http(s)://…, получено: {seed}"));
        }
        let depth = args.get("depth").and_then(|v| v.as_u64()).unwrap_or(2) as usize;
        let max_pages = args.get("max_pages").and_then(|v| v.as_u64()).unwrap_or(25) as usize;
        let delay_ms = args.get("delay_ms").and_then(|v| v.as_u64()).unwrap_or(1000);
        let cross_site = args
            .get("cross_site")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let wait_ms = args.get("wait_ms").and_then(|v| v.as_u64()).unwrap_or(self.wait_ms);
        // пер-страничный таймаут и robots — управляесы вызовом (WebLens
        // передаёт respect_robots=false для кнопки «индексировать ЭТУ страницу»)
        let page_timeout_ms = args
            .get("page_timeout_ms")
            .and_then(|v| v.as_u64())
            .unwrap_or(45_000)
            .clamp(1_000, 300_000);
        let respect_robots = args
            .get("respect_robots")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        web::ensure_chromium(self.cdp_port)?;
        let mut fetcher = CdpFetcher::new(self.cdp_port, wait_ms, page_timeout_ms)?;
        let mut ix = WebIndex::open(&self.db_path).map_err(|e| e.to_string())?;
        let cfg = web::CrawlConfig {
            max_pages: max_pages.clamp(1, 500),
            max_depth: depth.min(5),
            delay_ms,
            cross_site,
            wait_ms,
            page_timeout_ms,
            respect_robots,
        };
        eprintln!("poler-mcp: crawl seed={seed} depth={} max={}", cfg.max_depth, cfg.max_pages);
        let stats = web::crawl::crawl(&mut ix, &mut fetcher, seed, &cfg, false)
            .map_err(|e| e.to_string())?;
        Ok(format!(
            "Краулинг завершён: {} загружено, {} проиндексировано, {} без изменений, \
             {} дубликатов (SimHash), {} отклонено robots.txt, {} ошибок, {} мс. \
             Индекс: {} страниц в {:?}. Теперь доступен poler_web_search.",
            stats.fetched,
            stats.indexed,
            stats.unchanged,
            stats.duplicates,
            stats.skipped_robots,
            stats.errors,
            stats.elapsed_ms,
            ix.page_count(),
            self.db_path
        ))
    }

    // -----------------------------------------------------------------
    // poler_fetch: URL → реальный Chromium → рендер-текст + JSON API
    // -----------------------------------------------------------------
    fn tool_fetch(&self, args: &Value) -> Result<String, String> {
        let url = args
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or("аргумент url (http(s)://…) обязателен")?;
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!("url должен быть http(s)://…, получено: {url}"));
        }
        let wait_ms = args.get("wait_ms").and_then(|v| v.as_u64()).unwrap_or(self.wait_ms);

        web::ensure_chromium(self.cdp_port)?;
        let mut session = web::CdpSession::connect(self.cdp_port)?;
        let page = session.load_page_full(url, wait_ms)?;
        if page.text.trim().is_empty() {
            return Err(format!(
                "пустой рендер-текст: {url} (JS-ошибка, таймаут или доступ требует аутентификации)"
            ));
        }
        let text = web::extract::clean_text(&page.text, 256 * 1024);

        // кэш: полный текст — как .md (доступен локальному POLER-поиску),
        // перехваченные JSON API — отдельными файлами
        let dir = web::web_cache_dir();
        let slug = web::url_slug(&page.final_url);
        let title: String = if page.title.trim().is_empty() {
            text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().chars().take(120).collect()
        } else {
            page.title.trim().chars().take(120).collect()
        };
        let text_file = dir.join(format!("{slug}.md"));
        let _ = std::fs::write(
            &text_file,
            format!("# {title}\n\n> source: {}\n\n{}\n", page.final_url, text),
        );
        let mut json_files = Vec::new();
        for (i, (jurl, body)) in page.json_responses.iter().enumerate() {
            let f = dir.join(format!("{}_api{}.json", web::url_slug(jurl), i));
            let pretty = serde_json::from_str::<Value>(body)
                .and_then(|v| serde_json::to_string_pretty(&v))
                .unwrap_or_else(|_| body.clone());
            let _ = std::fs::write(&f, pretty);
            json_files.push(f);
        }

        let preview: String = text.chars().take(FETCH_TEXT_CAP).collect();
        let truncated = text.len() > FETCH_TEXT_CAP;
        let mut out = format!(
            "poler-fetch: {url}\nфинальный URL: {} (редиректы учтены)\nзаголовок: {}\n\
             язык: {}, ссылок: {}, перехвачено JSON API: {}\nполный текст: {} байт{}\n\n",
            page.final_url,
            title,
            if page.lang.is_empty() { "-" } else { &page.lang },
            page.links.len(),
            json_files.len(),
            text.len(),
            if truncated { " (в ответе усечён, полный — в кэше)" } else { "" },
        );
        out.push_str(&preview);
        out.push_str(&format!("\n\n---\nкэш-текст: {}\n", text_file.display()));
        for f in &json_files {
            out.push_str(&format!("кэш-json: {}\n", f.display()));
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // poler_search: локальный резонансный поиск (ε/R/сцены/K-hop)
    // -----------------------------------------------------------------
    fn tool_local_search(&self, args: &Value) -> Result<String, String> {
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("аргумент path (файл или директория) обязателен")?;
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("аргумент query обязателен")?;
        let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(5) as usize;
        let target = PathBuf::from(path);
        if !target.exists() {
            return Err(format!("путь не найден: {path}"));
        }
        let config = EngineConfig {
            top_n: top.clamp(1, 20),
            ..Default::default()
        };
        let mut engine = Engine::new(config, false);
        let (res, _stats) = engine.scan(&target, query);
        if res.anchors.is_empty() {
            return Ok(format!("0 хитов по «{query}» в {path}"));
        }
        let mut out = format!(
            "poler local-search «{query}» в {path}: {} хитов (показано {})\n\n",
            res.total_hits,
            res.anchors.len()
        );
        for (i, a) in res.anchors.iter().enumerate() {
            let head: String = a.scene.enclosing_scope.chars().take(200).collect();
            out.push_str(&format!(
                "{}. R={:.1} ε={:.1} | {} | {}\n",
                i + 1,
                a.resonance,
                a.epsilon,
                a.file,
                head.replace('\n', " ")
            ));
        }
        out.push_str("\n(полные сцены/K-hop граф — CLI: poler-engine <path> -q «…» --format ai-json)");
        Ok(out)
    }

    // -----------------------------------------------------------------
    // poler_gmail: Gmail владельца (OAuth readonly-токен)
    // -----------------------------------------------------------------
    fn tool_gmail(&self, args: &Value) -> Result<String, String> {
        // v0.18.0: License Gate (Community-квота на интеграции).
        if !crate::license::gate_or_print(crate::license::FEATURE_GMAIL) {
            return Err("Community-лимит Gmail исчерпан (50/24ч); локальный поиск без лимитов".into());
        }
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let max = args.get("max").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let max = max.clamp(1, 50);
        let hits = crate::google::api::gmail_search(query, max)?;
        if hits.is_empty() {
            return Ok(format!(
                "0 писем по «{query}». Синтаксис Gmail: from:, subject:, \
                 has:attachment, newer_than:7d, is:unread…"
            ));
        }
        Ok(crate::google::api::format_mail_hits(&hits, query))
    }

    // -----------------------------------------------------------------
    // poler_drive: файлы Google Drive (OAuth readonly-токен)
    // -----------------------------------------------------------------
    fn tool_drive(&self, args: &Value) -> Result<String, String> {
        // v0.18.0: License Gate (Community-квота на интеграции).
        if !crate::license::gate_or_print(crate::license::FEATURE_DRIVE) {
            return Err("Community-лимит Drive исчерпан (50/24ч); локальный поиск без лимитов".into());
        }
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let max = args.get("max").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
        let max = max.clamp(1, 50);
        let hits = crate::google::api::drive_list(query, max)?;
        if hits.is_empty() {
            return Ok(format!(
                "0 файлов по «{query}» (пустой запрос — недавние файлы)"
            ));
        }
        Ok(crate::google::api::format_drive_hits(&hits, query))
    }

    // -----------------------------------------------------------------
    // poler_nlm: NotebookLM владельца (персистентный профиль, без пароля)
    // -----------------------------------------------------------------
    fn tool_nlm(&self, args: &Value) -> Result<String, String> {
        use crate::google::nlm::{self, NlmSession};

        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or("аргумент action обязателен: notebooks | source | notes | artifacts | account | chat | media | shot | sync")?
            .to_string();
        let get = |k: &str| args.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        let notebook_id = get("notebook_id");
        let source_id = get("source_id");
        let question = get("question");
        let url = get("url");

        match action.as_str() {
            // ---- медиа-канал: профильный Chromium видит то, чего нет в API ----
            "media" | "shot" => {
                let url = url.ok_or_else(|| format!("action {action} требует аргумент url"))?;
                if !url.starts_with("http://") && !url.starts_with("https://") {
                    return Err(format!("url должен быть http(s)://…, получено: {url}"));
                }
                let mut s = NlmSession::open()?;
                if action == "media" {
                    let (bytes, ct) = s.fetch_media(&url)?;
                    let path = nlm::save_media("poler-media", nlm::mime_ext(&ct), &bytes)?;
                    Ok(format!(
                        "медиа скачано профильным Chromium: {} ({} КБ, {})\nисточник: {url}\n\n\
                         URL картинок слайдов приходит в action source (у контента с images).",
                        path.display(),
                        bytes.len() / 1024,
                        ct
                    ))
                } else {
                    s.load_page_raw(&url)?;
                    let png = s.screenshot()?;
                    let path = nlm::save_media("poler-shot", "png", &png)?;
                    Ok(format!(
                        "скриншот страницы: {} ({} КБ)\nстраница: {url}\n\n\
                         PNG можно открыть или приложить к ответу владельцу — это канал \
                         для контента, который текстовый batchexecute не отдаёт.",
                        path.display(),
                        png.len() / 1024
                    ))
                }
            }

            "notebooks" => {
                let mut s = NlmSession::open()?;
                let nbs = s.list_notebooks()?;
                if nbs.is_empty() {
                    return Ok(
                        "0 ноутбуков у этого Google-аккаунта (создай на notebook.google.com)".to_string()
                    );
                }
                Ok(format!(
                    "NotebookLM: {} ноутбуков (аккаунт {}).\n\n{}\n\n\
                     notebook_id нужен для action source/notes/artifacts/chat; source_id \
                     и URL картинок — в списках источников выше.",
                    nbs.len(),
                    s.email.as_deref().unwrap_or("?"),
                    nlm::format_notebooks(&nbs)
                ))
            }

            "source" => {
                let nb = notebook_id
                    .ok_or("action source требует notebook_id (см. action notebooks)")?;
                let src = source_id
                    .ok_or("action source требует source_id (см. источники в action notebooks)")?;
                let mut s = NlmSession::open()?;
                let sc = s.load_source(&nb, &src)?;
                Ok(nlm::format_source_content(&sc, &nb))
            }

            "notes" => {
                let nb = notebook_id.ok_or("action notes требует notebook_id")?;
                let mut s = NlmSession::open()?;
                let v = s.notes(&nb)?;
                let pretty = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
                let (out, truncated) = cap_chars(&pretty, NLM_JSON_CAP);
                Ok(format!(
                    "Заметки ноутбука {nb} (raw-JSON):\n\n{out}{}",
                    if truncated { "\n…(обрезано для контекстного окна)" } else { "" }
                ))
            }

            "artifacts" => {
                let nb = notebook_id.ok_or("action artifacts требует notebook_id")?;
                let mut s = NlmSession::open()?;
                let arts = s.artifacts(&nb)?;
                if arts.is_empty() {
                    return Ok(
                        "Studio пусто: у ноутбука нет аудио-обзоров/отчётов/квизов/миндмэпов".to_string()
                    );
                }
                Ok(format!(
                    "Studio-объекты ноутбука {nb}:\n\n{}",
                    nlm::format_artifacts(&arts)
                ))
            }

            "account" => {
                let mut s = NlmSession::open()?;
                let v = s.account()?;
                let pretty = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
                let (out, truncated) = cap_chars(&pretty, NLM_JSON_CAP);
                Ok(format!(
                    "Аккаунт NotebookLM (raw-JSON):\n\n{out}{}",
                    if truncated { "\n…(обрезано)" } else { "" }
                ))
            }

            "chat" => {
                let nb = notebook_id.ok_or("action chat требует notebook_id")?;
                let q = question.ok_or("action chat требует question")?;
                eprintln!("poler-mcp: nlm chat notebook={nb} q={q:?} (до 90 с)");
                let mut s = NlmSession::open()?;
                let answer = s.chat(&nb, &q)?;
                Ok(format!("NotebookLM-ответ по источникам ноутбука {nb}:\n\n{answer}"))
            }

            "sync" => {
                // Синк NotebookLM в web-index: заметки/источники/артефакты
                // становятся страницами в общем индексе; --web-search после
                // sync пробивает NLM-корпус наравне с проползенным вебом.
                use crate::google::nlm_ingest;
                use crate::web::{WebIndex, default_db_path};
                let db_path = std::env::var("POLER_WEB_DB")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|_| default_db_path());
                let mut ix = WebIndex::open(&db_path)
                    .map_err(|e| format!("web-index {db_path:?}: {e}"))?;
                let mut s = NlmSession::open()?;
                Ok(match notebook_id {
                    Some(nb) if !nb.is_empty() => {
                        let nbs = s.list_notebooks().unwrap_or_default();
                        let target = nbs.iter().find(|x| x.id == nb).cloned();
                        match target {
                            Some(nb_meta) => {
                                let mut stats = nlm_ingest::IngestStats {
                                    notebooks: 1,
                                    ..Default::default()
                                };
                                if let Err(e) = nlm_ingest::ingest_notebook(&mut ix, &mut s, &nb_meta, &mut stats) {
                                    stats.errors.push(format!("ingest_notebook {nb}: {e}"));
                                }
                                let _ = ix.recompute_pagerank(20);
                                format!(
                                    "NLM sync (notebook {nb}): {} страниц ({} new/changed, {} unchanged).                                      Ошибок: {}. После sync используй poler_web_search — NLM-корпус                                      влит в общий индекс (URL вида nlm://notebook/{nb}/…).",
                                    stats.total_pages(),
                                    stats.total_reindexed(),
                                    stats.total_unchanged(),
                                    stats.errors.len(),
                                )
                            }
                            None => format!("ноутбук {nb} не найден в аккаунте (см. action notebooks)"),
                        }
                    }
                    _ => {
                        let stats = nlm_ingest::sync_all(&mut ix, &mut s)?;
                        format!(
                            "NLM sync (all): {} notebooks processed, {} страниц ({} new/changed,                              {} unchanged by Percolator-lite). Ошибок: {}.                              После sync используй poler_web_search — NLM-корпус влит в общий индекс.",
                            stats.notebooks,
                            stats.total_pages(),
                            stats.total_reindexed(),
                            stats.total_unchanged(),
                            stats.errors.len(),
                        )
                    }
                })
            }

            other => Err(format!(
                "неизвестный action: {other} (доступны notebooks | source | notes | artifacts | account | chat | media | shot | sync)"
            )),
        }
    }
}

fn tools_manifest() -> Vec<Value> {
    vec![
        json!({
            "name": "poler_web_search",
            "description": "Веб-поиск по постоянному индексу poler-engine (собирается poler_crawl). \
Ранжирование WebRank: 0.55·BM25 + 0.15·PageRank + 0.20·title + 0.10·ε-плотность. \
Кириллица стеммингуется (укр/рос падежи унифицируются). Возвращает score, URL, заголовок, сниппет.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Поисковый запрос"},
                    "top": {"type": "integer", "default": 10, "minimum": 1, "maximum": 50}
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "poler_crawl",
            "description": "Обойти сайт (seed_url) и проиндексировать в постоянную БД: \
real-Chromium рендер (SPA/JS), robots.txt + sitemap, SimHash-дедуп, PageRank, \
инкрементальность (повторный обход переиндексирует только изменившееся). \
Агент сам выбирает цель. После краула используй poler_web_search.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "seed_url": {"type": "string", "description": "http(s)://… стартовый URL"},
                    "depth": {"type": "integer", "default": 2, "maximum": 5, "description": "Глубина обхода от seed"},
                    "max_pages": {"type": "integer", "default": 25, "minimum": 1, "maximum": 500},
                    "delay_ms": {"type": "integer", "default": 1000, "description": "Пауза между запросами к хосту"},
                    "cross_site": {"type": "boolean", "default": false, "description": "Переходить на другие хосты"},
                    "wait_ms": {"type": "integer", "default": 1200, "description": "Пауза на XHR после load"}
                },
                "required": ["seed_url"]
            }
        }),
        json!({
            "name": "poler_fetch",
            "description": "Открыть URL в реальном Chromium (CDP) и вернуть рендер-текст: \
JS исполнен, SPA/Shadow DOM отрендерены, скрытые JSON API перехвачены. \
Инструмент «прочитать любую страницу сейчас». Полный текст и JSON пишутся в кэш \
и доступны локальному POLER-поиску. Аутентификационные стены (логины, платный \
контент) движком не обходятся — возвращается то, что видит анонимный браузер.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string", "description": "http(s)://… страница"},
                    "wait_ms": {"type": "integer", "default": 1200, "description": "Пауза на дочерние XHR"}
                },
                "required": ["url"]
            }
        }),
        json!({
            "name": "poler_search",
            "description": "Локальный резонансный POLER-поиск по файлу/директории: \
информационная плотность ε, резонанс R, полные логические сцены (не строки grep), \
K-hop связи сущностей. Работает и по веб-кэшу poler_fetch (/tmp/poler-web).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Файл или директория"},
                    "query": {"type": "string", "description": "Запрос: слово или фраза"},
                    "top": {"type": "integer", "default": 5, "minimum": 1, "maximum": 20}
                },
                "required": ["path", "query"]
            }
        }),
        json!({
            "name": "poler_gmail",
            "description": "Поиск в Gmail владельца (OAuth 2.0, скоуп gmail.readonly — \
только чтение). Запрос — нативный синтаксис Gmail: from:vasya, subject:отчёт, \
has:attachment, newer_than:7d, is:unread, слова И-комбинируются. \
Требует одноразовой авторизации владельца: poler-engine --google-auth \
(пароль остаётся в браузере владельца, движок видит только токен).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Gmail-запрос; пусто — недавняя почта"},
                    "max": {"type": "integer", "default": 10, "minimum": 1, "maximum": 50}
                },
                "required": []
            }
        }),
        json!({
            "name": "poler_drive",
            "description": "Файлы Google Drive владельца (OAuth 2.0, drive.readonly): \
поиск по имени (пустой запрос — недавние). Возвращает имя, тип, размер, \
дату изменения, прямую ссылку и file-id. Требует одноразовой авторизации \
владельца: poler-engine --google-auth.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Часть имени файла; пусто — недавние"},
                    "max": {"type": "integer", "default": 10, "minimum": 1, "maximum": 50}
                },
                "required": []
            }
        }),
        json!({
            "name": "poler_nlm",
            "description": "Ноутбуки NotebookLM владельца — БЕЗ пароля: движок говорит \
на внутреннем протоколе NotebookLM (batchexecute, разведан из расширения \
NLMTools) через персистентный профиль Chromium (одноразовый ручной логин: \
poler-engine --google-browse https://notebook.google.com/). Действия (action): \
notebooks — все ноутбуки с источниками (id, заголовки, типы, URL); source — \
текст источника или URL картинок слайдов (медиа!); notes — сохранённые \
заметки; artifacts — Studio-объекты (аудио-обзоры, отчёты, квизы, миндмэпы); \
account — профиль аккаунта; chat — вопрос к модели ноутбука ПО ЕГО \
ИСТОЧНИКАМ (RAG владельца, не общая модель); media — скачать медиа-URL \
(картинки из source) в файл; shot — скриншот любой страницы NotebookLM \
(медиа-канал: то, что текстовый API не отдаёт).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["notebooks", "source", "notes", "artifacts", "account", "chat", "media", "shot", "sync"],
                        "description": "Режим работы"
                    },
                    "notebook_id": {"type": "string", "description": "id ноутбука — из action notebooks"},
                    "source_id": {"type": "string", "description": "id источника — из списка источников ноутбука"},
                    "question": {"type": "string", "description": "Вопрос для action chat (по источникам ноутбука)"},
                    "url": {"type": "string", "description": "http(s)://… — медиа-URL (action media) или страница (action shot)"}
                },
                "required": ["action"]
            }
        }),
    ]
}
