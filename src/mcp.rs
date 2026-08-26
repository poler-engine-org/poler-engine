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
//! * `poler_search`     — локальный резонансный POLER-поиск (ε/R/сцены).
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

pub struct McpServer {
    cdp_port: u16,
    wait_ms: u64,
    db_path: PathBuf,
}

/// Запуск MCP-сервера. Возвращает код процесса (0 = чистый EOF stdin).
pub fn run(cdp_port: u16, wait_ms: u64, db_path: PathBuf) -> i32 {
    let server = McpServer {
        cdp_port,
        wait_ms,
        db_path,
    };
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
    fn handle(&self, out: &mut impl Write, msg: Value) {
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // уведомления (без id) не требуют ответа
        let Some(id) = id else {
            return;
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

        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, message)) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": {"code": code, "message": message}
            }),
        };
        let _ = write_line(out, &resp);
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

        web::ensure_chromium(self.cdp_port)?;
        let mut fetcher = CdpFetcher::new(self.cdp_port, wait_ms)?;
        let mut ix = WebIndex::open(&self.db_path).map_err(|e| e.to_string())?;
        let cfg = web::CrawlConfig {
            max_pages: max_pages.clamp(1, 500),
            max_depth: depth.min(5),
            delay_ms,
            cross_site,
            wait_ms,
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
    ]
}
