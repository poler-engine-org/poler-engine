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
//! * `poler_box_exec` / `poler_box_status` — контейнерный jail (gateway).
//!
//! v2.0 (sovereign stack): poler_gmail / poler_drive / poler_nlm удалены
//! вместе с Google/NotebookLM-интеграциями.
//!
//! Chromium поднимается автоматически при первом poler_crawl/poler_fetch
//! (см. `web::ensure_chromium`).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::Mutex;
#[cfg(feature = "pnd-ffi")]
use std::sync::Arc;

use serde_json::{json, Value};

use crate::web::{self, CdpFetcher, WebIndex};
use crate::{Engine, EngineConfig};

/// Cap рендер-текста в ответе poler_fetch: больше — бессмысленно для
/// контекстного окна агента (полный текст лежит в кэш-файле).
const FETCH_TEXT_CAP: usize = 24 * 1024;

// ---------------------------------------------------------------------------
// Аудит-фикс v0.21.0 (security hardening): ограждение MCP-инструментов.
// Угроза: токен Bearer (туннель/логи/конфиг) даёт УДАЛЁННОМУ держателю
// читать ЛЮБЫЕ локальные файлы (секреты, ключи, конфиги) и ходить на
// внутренние адреса (SSRF).
// ---------------------------------------------------------------------------

/// Чистая (без env) проверка: лежит ли канонический `path` внутри одного из
/// `roots`? Несуществующий путь — разрешён (ошибку отчётит сам инструмент,
/// канонизации нет). Символические ссылки резолвятся — escape через symlink
/// невозможен.
pub fn path_allowed_under(path: &str, roots: &[PathBuf]) -> bool {
    let Ok(target) = std::fs::canonicalize(path) else {
        return true; // нет файла — пусть инструмент честно скажет «не найден»
    };
    roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .any(|r| target.starts_with(r))
}

/// Разрешён ли локальный путь для poler_grep / poler_chunk / poler_search
/// по умолчанию: рабочая директория, веб-кэш и POLER_MCP_EXTRA_ROOTS.
/// `POLER_MCP_ALLOW_ANY_PATH=1` возвращает прежнее поведение (доверенный
/// локальный агент).
pub fn mcp_path_allowed(path: &str) -> bool {
    if std::env::var("POLER_MCP_ALLOW_ANY_PATH")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return true;
    }
    let mut roots: Vec<PathBuf> = vec![
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        web::web_cache_dir(),
    ];
    if let Ok(extra) = std::env::var("POLER_MCP_EXTRA_ROOTS") {
        for r in extra.split(':').filter(|s| !s.is_empty()) {
            roots.push(PathBuf::from(r));
        }
    }
    path_allowed_under(path, &roots)
}

/// Чистая (без env) проверка хоста: блокируемый ли это адрес?
/// Link-local 169.254.0.0/16 (вкл. cloud-metadata 169.254.169.254),
/// 0.0.0.0/8, IPv6 fe80::/10 (link-local), fc00::/7 (ULA), ::
/// и известные имена metadata-сервисов. RFC1918 и loopback НЕ блокируем:
/// WebLens и локальная разработка ходят на 127.0.0.1.
pub fn host_blocked_by_default(host: &str) -> bool {
    const META_HOSTS: [&str; 3] = ["metadata.google.internal", "metadata.goog", "instance-data"];
    let h = host.trim().trim_matches(|c| c == '[' || c == ']');
    if META_HOSTS.contains(&h.to_ascii_lowercase().as_str()) {
        return true;
    }
    if let Ok(ip) = h.parse::<std::net::Ipv4Addr>() {
        let o = ip.octets();
        return (o[0] == 169 && o[1] == 254) || o[0] == 0;
    }
    if let Ok(ip) = h.parse::<std::net::Ipv6Addr>() {
        let s = ip.segments();
        return (s[0] & 0xffc0) == 0xfe80 || (s[0] & 0xfe00) == 0xfc00 || ip.is_unspecified();
    }
    false // обычные домены — решение за Chromium (резолв и запрос)
}

/// Анти-SSRF-гейт для poler_fetch / poler_crawl. Отключение (наш собственный
/// агент на доверенной машине): `POLER_MCP_ALLOW_PRIVATE_NET=1`.
pub fn mcp_url_allowed(url: &str) -> Result<(), String> {
    if std::env::var("POLER_MCP_ALLOW_PRIVATE_NET")
        .map(|v| v == "1")
        .unwrap_or(false)
    {
        return Ok(());
    }
    let host = url
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(url)
        .split(['/', '?'])
        .next()
        .unwrap_or("")
        .to_string();
    // порт отрезаем только если он есть (rsplit_once без ':' даёт None)
    let host = host
        .rsplit_once(':')
        .map(|(h, _)| h.to_string())
        .unwrap_or(host);
    if host_blocked_by_default(&host) {
        return Err(format!(
            "SSRF-ограждение: хост {host} (link-local/metadata) заблокирован; \
             POLER_MCP_ALLOW_PRIVATE_NET=1 снимает ограничение"
        ));
    }
    Ok(())
}

/// Ограждение локального пути (общая ошибка для инструментов чтения файлов).
fn guard_path(path: &str) -> Result<(), String> {
    if mcp_path_allowed(path) {
        return Ok(());
    }
    Err(format!(
        "путь вне разрешённых корней MCP (cwd, веб-кэш, POLER_MCP_EXTRA_ROOTS); \
         POLER_MCP_ALLOW_ANY_PATH=1 снимает ограничение: {path}"
    ))
}

pub struct McpServer {
    cdp_port: u16,
    wait_ms: u64,
    db_path: PathBuf,
    /// БД знаний Суверенного Гиппокампа (v0.29): отдельная от веб-индекса,
    /// строится `--knowledge-ingest`.
    knowledge_db: PathBuf,
    // ── M6: резидентное (тёплое) состояние ──────────────────────────
    /// Открытый веб-индекс — переиспользуется между вызовами
    /// (холодный старт открывал SQLite на КАЖДЫЙ web_search/crawl).
    warm_web: Mutex<Option<WebIndex>>,
    /// Тёплый Гиппокамп: SQLite + RaBitQ + HNSW + эмбеддер.
    warm_knowledge: Mutex<Option<crate::sources::knowledge::WarmKnowledge>>,
    /// RAM-кэш файлов для poler_grep (mtime-инвалидация, LRU).
    file_cache: crate::retrieval::filecache::FileCache,
    /// Шифропоток журнала операций (стриминг логов в Vault, --vault-log).
    #[cfg(feature = "pnd-ffi")]
    vault_log: Option<Arc<Mutex<crate::crypto::vault::VaultAppender>>>,
}

impl McpServer {
    /// Конструктор для альтернативных транспортов (mcp_http: Streamable HTTP).
    pub fn new(cdp_port: u16, wait_ms: u64, db_path: PathBuf) -> Self {
        Self {
            cdp_port,
            wait_ms,
            db_path,
            knowledge_db: crate::sources::knowledge::default_db_path(),
            warm_web: Mutex::new(None),
            warm_knowledge: Mutex::new(None),
            file_cache: crate::retrieval::filecache::FileCache::new(DEFAULT_RAM_BUDGET),
            #[cfg(feature = "pnd-ffi")]
            vault_log: None,
        }
    }

    /// Переопределить БД знаний (Суверенный Гиппокамп, v0.29).
    pub fn with_knowledge_db(mut self, path: PathBuf) -> Self {
        self.knowledge_db = path;
        self
    }

    /// M6: RAM-бюджет файл-кэша grep (байт).
    pub fn with_ram_budget(mut self, bytes: usize) -> Self {
        self.file_cache = crate::retrieval::filecache::FileCache::new(bytes);
        self
    }

    /// M6: подключить шифропоток журнала операций (Vault .pvt).
    #[cfg(feature = "pnd-ffi")]
    pub fn with_vault_log(mut self, app: crate::crypto::vault::VaultAppender) -> Self {
        self.vault_log = Some(Arc::new(Mutex::new(app)));
        self
    }

    /// M6: снапшот статистики резидентного состояния (бенч/диагностика).
    pub fn resident_stats(&self) -> serde_json::Value {
        let cs = self.file_cache.stats();
        let warm_k = self.warm_knowledge.lock().map(|g| g.is_some()).unwrap_or(false);
        let warm_w = self.warm_web.lock().map(|g| g.is_some()).unwrap_or(false);
        serde_json::json!({
            "file_cache": {
                "files": cs.files,
                "bytes": cs.bytes,
                "hits": cs.hits,
                "misses": cs.misses,
                "invalidated": cs.invalidated,
                "evictions": cs.evictions,
                "budget_bytes": self.file_cache.cap_bytes(),
            },
            "warm_knowledge_open": warm_k,
            "warm_web_open": warm_w,
        })
    }

    /// M6: выполнить операцию над тёплым веб-индексом (get-or-open).
    fn with_web_index<T>(
        &self,
        f: impl FnOnce(&mut WebIndex) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut guard = self.warm_web.lock().expect("poler-mcp: отравленный веб-лок");
        if guard.is_none() {
            *guard = Some(WebIndex::open(&self.db_path).map_err(|e| e.to_string())?);
        }
        f(guard.as_mut().expect("только что открыли"))
    }

    /// M6: событие журнала — шифропоток Vault (+ мягкий коммит):
    /// {"ts":мс,"method":"tools/call","tool":…,"us":мкс,"ok":bool}.
    /// Ошибка журнала НЕ роняет запрос — лог не может ломать сервис.
    #[cfg(feature = "pnd-ffi")]
    fn log_event(&self, method: &str, tool: Option<&str>, us: u64, ok: bool) {
        let Some(vl) = &self.vault_log else { return };
        let Ok(mut g) = vl.lock() else { return };
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let tool_js = match tool {
            Some(t) => format!(",\"tool\":\"{t}\""),
            None => String::new(),
        };
        let _ = g.append_line(&format!(
            "{{\"ts\":{ts},\"method\":\"{method}\"{tool_js},\"us\":{us},\"ok\":{}}}",
            ok
        ));
        let _ = g.commit_soft();
    }

    #[cfg(not(feature = "pnd-ffi"))]
    fn log_event(&self, _method: &str, _tool: Option<&str>, _us: u64, _ok: bool) {}
}

/// RAM-бюджет файл-кэша по умолчанию (48 МиБ — в рамках лимита M6 ≤64 МиБ).
pub const DEFAULT_RAM_BUDGET: usize = 48 * 1024 * 1024;

/// M6: опции резидентного MCP-сервера (общие для stdio и HTTP).
#[derive(Default)]
pub struct McpServerOptions {
    /// RAM-бюджет файл-кэша grep (байт; 0 = default 48 МиБ).
    pub ram_budget: usize,
    /// Шифропоток журнала операций (Vault .pvt, --vault-log).
    #[cfg(feature = "pnd-ffi")]
    pub vault_log: Option<crate::crypto::vault::VaultAppender>,
}

impl McpServerOptions {
    /// Применить к серверу (builder-стиль).
    pub fn apply(self, srv: McpServer) -> McpServer {
        let mut srv = srv;
        if self.ram_budget > 0 {
            srv = srv.with_ram_budget(self.ram_budget);
        }
        #[cfg(feature = "pnd-ffi")]
        if let Some(app) = self.vault_log {
            srv = srv.with_vault_log(app);
        }
        srv
    }
}

/// Запуск MCP-сервера (stdio). Возвращает код процесса (0 = чистый EOF stdin).
pub fn run(
    cdp_port: u16,
    wait_ms: u64,
    db_path: PathBuf,
    knowledge_db: Option<PathBuf>,
    opts: McpServerOptions,
) -> i32 {
    let mut server = McpServer::new(cdp_port, wait_ms, db_path);
    if let Some(kdb) = knowledge_db {
        server = server.with_knowledge_db(kdb);
    }
    server = opts.apply(server);
    eprintln!(
        "poler-mcp: stdio JSON-RPC, db={:?}, knowledge={:?}, cdp_port={}, wait_ms={}",
        server.db_path, server.knowledge_db, server.cdp_port, server.wait_ms
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
    /// M6: каждое сообщение таймируется и (если подключён --vault-log)
    /// попадает в шифропоток журнала с латентностью в микросекундах.
    pub fn dispatch(&self, msg: &Value) -> Option<Value> {
        let t0 = std::time::Instant::now();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("").to_string();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // уведомления (без id) не требуют ответа
        let Some(id) = id else { return None };

        let result: Result<Value, (i64, String)> = match method.as_str() {
            "initialize" => Ok(self.handle_initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools_manifest()})),
            "tools/call" => self.handle_tools_call(&params),
            "resources/list" => Ok(json!({"resources": []})),
            "resources/templates/list" => Ok(json!({"resourceTemplates": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            _ => Err((-32601, format!("method not found: {method}"))),
        };

        let ok = result.is_ok();
        let tool = params.get("name").and_then(|n| n.as_str()).map(str::to_string);
        let us = t0.elapsed().as_micros() as u64;
        self.log_event(&method, tool.as_deref(), us, ok);

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
            "poler_grep" => self.tool_grep(&args),
            "poler_chunk" => self.tool_chunk(&args),
            "poler_box_exec" => self.tool_box_exec(&args),
            "poler_box_status" => self.tool_box_status(&args),
            "query_poler_knowledge" => self.tool_knowledge_query(&args),
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
        // M6: тёплый веб-индекс — SQLite открыт резидентно.
        self.with_web_index(|ix| {
        if ix.page_count() == 0 {
            return Ok(format!(
                "Веб-индекс пуст ({:?}). Сначала вызови poler_crawl с seed_url — \
                 агент сам выбирает, какой сайт обходить.",
                self.db_path
            ));
        }
        let bridge = crate::retrieval::SemanticBridge::offline();
        let (hits, expansion) = ix
            .search_with_bridge(query, top, &bridge)
            .map_err(|e| e.to_string())?;
        if hits.is_empty() {
            return Ok(format!(
                "Ничего не найдено по «{query}» (индекс: {} страниц). \
                 Попробуй другие термы или докрауль релевантный сайт.",
                ix.page_count()
            ));
        }
        let mut out = format!("poler web-search «{query}»: {} из {} страниц\n\n", hits.len(), ix.page_count());
        // Semantic Bridge WHY: агент обязан видеть, какие кандидаты сенсора
        // подмешаны в выдачу (объяснимость — часть контракта инструмента)
        if !expansion.is_empty() {
            out.push_str("Semantic Bridge (кросс-языковое расширение, WHY):\n");
            for l in expansion.why_lines() {
                out.push_str(&format!("  {l}\n"));
            }
            out.push('\n');
        }
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
        })
    }

    // -----------------------------------------------------------------
    // query_poler_knowledge: Суверенный Гиппокамп (v0.29)
    // -----------------------------------------------------------------
    /// Поиск по библиотеке POLER с эпистемической градацией доверия.
    ///
    /// Хиты несут: статус верификации (MVR-паспорт / первоисточник /
    /// нарратив), файл, номера строк, байтовый диапазон и прямую цитату —
    /// агент цитирует заземлённо и никогда не путает доказанное с нарративом.
    /// Библиотека не построена → инструкция (isError=false), не сбой.
    fn tool_knowledge_query(&self, args: &Value) -> Result<String, String> {
        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or("аргумент query (строка) обязателен")?;
        if query.trim().is_empty() {
            return Err("query пустой — укажи поисковую фразу".into());
        }
        let top = args.get("top").and_then(|v| v.as_u64()).unwrap_or(8) as usize;
        let top = top.clamp(1, 30);
        let min_provenance = match args.get("min_provenance").and_then(|v| v.as_str()) {
            None => None,
            Some(s) => Some(crate::retrieval::Provenance::parse(s).ok_or_else(|| {
                format!("неизвестный min_provenance {s:?} (доступно: mvr | source | narrative)")
            })?),
        };
        // Векторное русло (если индекс построен с ним): .pqw-модель из
        // POLER_KNOWLEDGE_MODEL; без переменной — честная деградация до BM25.
        // M6: тёплый Гиппокамп — SQLite/RaBitQ/HNSW/эмбеддер открываются
        // один раз и живут в резидентном сервере (get-or-open ниже).
        let model = std::env::var("POLER_KNOWLEDGE_MODEL").ok().map(PathBuf::from);
        let opts = crate::sources::knowledge::QueryOptions {
            top,
            min_provenance,
            ..Default::default()
        };
        let mut guard = self
            .warm_knowledge
            .lock()
            .expect("poler-mcp: отравленный лок Гиппокампа");
        if guard.is_none() {
            let embedder =
                match crate::sources::knowledge::query_embedder(&self.knowledge_db, model.as_deref()) {
                    Ok(e) => e,
                    Err(e) => {
                        eprintln!("poler-mcp: векторное русло пропущено: {e}");
                        None
                    }
                };
            match crate::sources::knowledge::WarmKnowledge::open(&self.knowledge_db, embedder) {
                Ok(wk) => *guard = Some(wk),
                // Библиотека не построена — состояние, не сбой инструмента
                Err(e) if e.contains("--knowledge-ingest") => {
                    return Ok(format!("Библиотека знаний ещё не проиндексирована: {e}"))
                }
                Err(e) => return Err(e),
            }
        }
        let wk = guard
            .as_mut()
            .expect("тёплый Гиппокамп только что открыт");
        match crate::sources::knowledge::query_warm(wk, query, &opts) {
            Ok(out) => Ok(crate::sources::knowledge::render_query_text(query, &out)),
            Err(e) if e.contains("--knowledge-ingest") => {
                Ok(format!("Библиотека знаний ещё не проиндексирована: {e}"))
            }
            Err(e) => Err(e),
        }
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
        // Аудит-фикс: анти-SSRF (169.254.169.254 и пр.) до подъёма Chromium
        mcp_url_allowed(seed)?;
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
        // M6: тёплый веб-индекс — краулинг дописывает в резидентное
        // соединение (виден последующим web_search без пере-открытия).
        // Лок индекса держится на весь обход: SQLite-соединение одно,
        // параллельный web_search честно ждёт (защита от SQLITE_BUSY).
        let db_path = self.db_path.clone();
        let cdp_port = self.cdp_port;
        let stats = self.with_web_index(|ix| {
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
            let mut fetcher = CdpFetcher::new(cdp_port, wait_ms, page_timeout_ms)?;
            web::crawl::crawl(ix, &mut fetcher, seed, &cfg, false).map_err(|e| e.to_string())
        })?;
        Ok(format!(
            "Краулинг завершён: {} загружено, {} проиндексировано, {} без изменений, \
             {} дубликатов (SimHash), {} отклонено robots.txt, {} ошибок, {} мс. \
             Индекс: {:?}.",
            stats.fetched,
            stats.indexed,
            stats.unchanged,
            stats.duplicates,
            stats.skipped_robots,
            stats.errors,
            stats.elapsed_ms,
            db_path
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
        // Аудит-фикс: анти-SSRF (link-local/metadata) до подъёма Chromium
        mcp_url_allowed(url)?;
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
        // Аудит-фикс: ограждение локальных путей (анти-экфильтрация)
        guard_path(path)?;
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
    // poler_grep: точный поиск в семантике grep (слой 0 Native Retrieval)
    // -----------------------------------------------------------------
    fn tool_grep(&self, args: &Value) -> Result<String, String> {
        use crate::retrieval as nr;
        let pattern = args
            .get("pattern")
            .and_then(|v| v.as_str())
            .ok_or("аргумент pattern обязателен")?
            .to_string();
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        // Аудит-фикс: ограждение локальных путей (анти-экфильтрация)
        guard_path(path)?;
        let mode = if args.get("regex").and_then(|v| v.as_bool()).unwrap_or(false) {
            nr::GrepMode::Regex
        } else {
            nr::GrepMode::Literal
        };
        let output = match args.get("output").and_then(|v| v.as_str()).unwrap_or("content") {
            "count" => nr::GrepOutput::Count,
            "list" => nr::GrepOutput::ListMatching,
            "list_nonmatching" => nr::GrepOutput::ListNonMatching,
            _ => nr::GrepOutput::Content,
        };
        let config = nr::GrepConfig {
            pattern,
            mode,
            case_insensitive: args
                .get("ignore_case")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            before: args.get("before").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            after: args.get("after").and_then(|v| v.as_u64()).unwrap_or(0) as usize,
            max_count: args
                .get("max_count")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize),
            output,
            include_hidden: false,
            respect_ignore: true,
            // v0.28.1: MCP-агент может включить скан архивов без распаковки
            // аргументами tools: {"archives": true, "archive_password": "…"}.
            scan_archives: args.get("archives").and_then(|v| v.as_bool()).unwrap_or(false),
            archive_password: args
                .get("archive_password")
                .and_then(|v| v.as_str())
                .map(String::from),
            archive_max_entry_bytes: 0,
        };
        let report = nr::grep_run_cached(&[PathBuf::from(path)], &config, &self.file_cache)
            .map_err(|e| format!("grep: {e}"))?;
        if args.get("json").and_then(|v| v.as_bool()).unwrap_or(false) {
            return serde_json::to_string_pretty(&report)
                .map_err(|e| format!("сериализация: {e}"));
        }
        let mut out = nr::render_text(&report, false);
        out.push_str(&format!(
            "\n[poler-grep] файлов: {}, с совпадениями: {}, строк: {}, время: {} мс\n",
            report.stats.files_scanned,
            report.stats.files_matched,
            report.stats.lines_matched,
            report.stats.elapsed_ms
        ));
        if !report.stats.errors.is_empty() {
            out.push_str(&format!("\nошибки обхода: {}\n", report.stats.errors.len()));
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // poler_chunk: RAG-нарезка документа на чанки с якорями (слой B)
    // -----------------------------------------------------------------
    fn tool_chunk(&self, args: &Value) -> Result<String, String> {
        use crate::retrieval as nr;
        let path = args
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("аргумент path (файл) обязателен")?;
        // Аудит-фикс: ограждение локальных путей (анти-экфильтрация)
        guard_path(path)?;
        let target = PathBuf::from(path);
        if !target.is_file() {
            return Err(format!("не файл или не найден: {path}"));
        }
        let text = std::fs::read_to_string(&target)
            .map_err(|e| format!("прочитать {path}: {e}"))?;
        let config = nr::ChunkConfig {
            target_tokens: args
                .get("target_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(nr::DEFAULT_TARGET_TOKENS as u64) as usize,
            overlap_tokens: args
                .get("overlap_tokens")
                .and_then(|v| v.as_u64())
                .unwrap_or(nr::DEFAULT_OVERLAP_TOKENS as u64) as usize,
            ..Default::default()
        };
        let format = nr::ChunkFormat::detect(&target);
        let report = nr::chunk_document(&text, format, &config);
        if args.get("json").and_then(|v| v.as_bool()).unwrap_or(false) {
            return serde_json::to_string_pretty(&report)
                .map_err(|e| format!("сериализация: {e}"));
        }
        Ok(nr::render_chunks_text(&report, path))
    }

    // -----------------------------------------------------------------
    // poler_box_exec (v0.26.0): двухконтурный брокер — команда агента
    // судится sandbox-судьёй и исполняется ВНУТРИ изолированного
    // контейнера (runner → box), результат возвращается текстом.
    // -----------------------------------------------------------------
    fn tool_box_exec(&self, args: &Value) -> Result<String, String> {
        use crate::gateway::{containers, hostexec, pipeline, sandbox};

        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("аргумент command обязателен (одиночная команда; кавычки — как в шлюзе)")?
            .to_string();
        if command.trim().is_empty() {
            return Err("command пуст".into());
        }
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(120)
            .clamp(1, 600);
        let target = args
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("auto")
            .to_string();
        if !matches!(target.as_str(), "auto" | "runner" | "box") {
            return Err(format!("target={target}? (auto | runner | box)"));
        }

        // Разбор тем же лексером, что и REPL шлюза: кавычки раскрываются,
        // /bin/sh НЕ участвует (argv насквозь — нет класса shell-инъекций)
        let pl = pipeline::parse_line(&command)
            .map_err(|e| format!("не удалось разобрать команду: {e}"))?;

        // Вердикт ДО исполнения (контур 2 схемы) — по ВСЕМУ конвейеру:
        // exec-плоскость в контейнере судится без логической границы (её
        // держит контейнер — та же семантика, что у шлюза в jail), но
        // Block-инварианты (деструктив, привилегии, форк-бомбы, remote-exec)
        // действуют ВСЕГДА; Confirm из MCP не подтверждается в принципе
        // (Zero Silent Escalation — подтверждает только владелец).
        let all_host: Vec<usize> = (0..pl.segments.len()).collect();
        match sandbox::judge_pipeline_ws(&pl, &all_host, None) {
            sandbox::Policy::Allow => {}
            sandbox::Policy::Block(why) => {
                return Err(format!("⛔ блокировка sandbox: {why}"));
            }
            sandbox::Policy::Confirm(why) => {
                return Err(format!(
                    "⛔ {why} — подтверждение доступно только владельцу в шлюзе \
                     (allow <путь> / grant sudo); MCP-агент подтвердить не может"
                ));
            }
        }

        // Исполнение — только одиночная команда (без конвейера/редиректа):
        // брокер возвращает stdout/stderr целиком, конвейеры собираются
        // агентом из нескольких вызовов (bash -c '…' допустим — payload
        // судится рекурсивно тем же судьёй выше).
        if pl.segments.len() > 1 {
            return Err(
                "конвейеры не поддерживаются: вызывайте части отдельными вызовами poler_box_exec \
                 (bash -c '…' допустим — payload судится рекурсивно тем же судьёй)"
                    .into(),
            );
        }
        if pl.redirect.is_some() {
            return Err(
                "редиректы не поддерживаются: stdout/stderr и так возвращаются целиком".into(),
            );
        }
        let tokens = pl
            .segments
            .first()
            .map(|s| s.tokens.clone())
            .unwrap_or_default();
        if tokens.is_empty() {
            return Err("пустая команда".into());
        }

        // Discovery по workspace: сервис mcp НЕ разделяет память с gateway —
        // корень приходит через POLER_WORKSPACE (шлюз выставляет при
        // `service start mcp`), контейнеры находятся по docker-labels.
        let ws = std::env::var("POLER_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            });
        containers::docker_probe().map_err(|e| {
            format!("docker недоступен: {e} (box runner on / box on — поднять контур исполнения в шлюзе)")
        })?;

        let runner = containers::runner_state_for_ws(&ws);
        let boxj = containers::box_state_for_ws(&ws);
        let (wrapped, target_name, target_kind) = match target.as_str() {
            "runner" => {
                let r = runner.ok_or(
                    "runner-контейнер не работает — box runner on (net=none, только /workspace)",
                )?;
                let name = r.name.clone();
                (containers::wrap_runner_exec(&r, &tokens, &ws), name, "runner")
            }
            "box" => {
                let j = boxj.ok_or("box-контейнер не работает — box on")?;
                let name = j.name.clone();
                (containers::wrap_host_exec(&j, &tokens, &ws), name, "box")
            }
            _ => {
                // auto: runner (жёстче: без сети, без home) → box → отказ
                if let Some(r) = runner {
                    let name = r.name.clone();
                    (containers::wrap_runner_exec(&r, &tokens, &ws), name, "runner")
                } else if let Some(j) = boxj {
                    let name = j.name.clone();
                    (containers::wrap_host_exec(&j, &tokens, &ws), name, "box")
                } else {
                    return Err(
                        "нет работающего контура исполнения: box runner on (предпочтительно) или box on"
                            .into(),
                    );
                }
            }
        };

        let limits = hostexec::HostLimits {
            timeout_secs,
            ..Default::default()
        };
        let outcome = hostexec::run(&wrapped, None, &limits, &ws, &[]);
        if let Some(e) = &outcome.spawn_error {
            return Err(format!("не удалось запустить docker-клиент: {e}"));
        }
        let code = outcome
            .code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "убит сигналом".into());
        let mut out = format!(
            "двухконтурный брокер: исполнено в {target_kind} {target_name} · exit {code}\n"
        );
        out.push_str(&format!("--- stdout ---\n{}", outcome.stdout));
        if !outcome.stderr.is_empty() {
            out.push_str(&format!("--- stderr ---\n{}", outcome.stderr));
        }
        if outcome.truncated {
            out.push_str("(вывод обрезан по капу)\n");
        }
        if outcome.timed_out {
            out.push_str(&format!("(таймаут {timeout_secs}s — процесс убит)\n"));
        }
        if outcome.interrupted {
            out.push_str("(прервано)\n");
        }
        Ok(out)
    }

    // -----------------------------------------------------------------
    // poler_box_status (v0.26.0): состояние jail-стека для workspace
    // -----------------------------------------------------------------
    fn tool_box_status(&self, _args: &Value) -> Result<String, String> {
        use crate::gateway::containers;
        let ws = std::env::var("POLER_WORKSPACE")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            });
        let mut s = String::new();
        match containers::docker_probe() {
            Ok(v) => s.push_str(&format!("docker: сервер {v}\n")),
            Err(e) => {
                return Ok(format!(
                    "docker: недоступен ({e})\nконтуры исполнения не подняты: box on / box runner on — в шлюзе (poler_box_exec требует контура исполнения)\n"
                ))
            }
        }
        match containers::box_state_for_ws(&ws) {
            Some(j) => {
                let agents = if j.cfg.agent_names.is_empty() {
                    "—".to_string()
                } else {
                    j.cfg.agent_names.join(",")
                };
                s.push_str(&format!(
                    "box {}: работает · образ {} · агенты (ro-проброс): {agents}\n",
                    j.name, j.cfg.image
                ));
            }
            None => s.push_str(&format!(
                "box {}: не работает (box on в шлюзе — контуры 2/3 в контейнере)\n",
                containers::container_name(&ws)
            )),
        }
        match containers::runner_state_for_ws(&ws) {
            Some(r) => s.push_str(&format!(
                "runner {}: работает · образ {} · net {} · только /workspace\n",
                r.name, r.cfg.image, r.cfg.net
            )),
            None => s.push_str(&format!(
                "runner {}: не работает (box runner on — сюда poler_box_exec исполняет команды; net=none)\n",
                containers::runner_name(&ws)
            )),
        }
        s.push_str("poler_box_exec: target=auto → runner, при его отсутствии box; деструктив блокируется судьёй до исполнения\n");
        Ok(s)
    }
}

fn tools_manifest() -> Vec<Value> {
    vec![
        json!({
            "name": "poler_web_search",
            "description": "Веб-поиск по постоянному индексу poler-engine (собирается poler_crawl). \
Ранжирование WebRank: 0.55·BM25 + 0.15·PageRank + 0.20·title + 0.10·ε-плотность. \
Кириллица стеммингуется (укр/рос падежи унифицируются). Semantic Bridge (v0.21): \
кросс-языковые запросы (рус→англ корпус и обратно) расширяются офлайн-словарём, \
кандидаты подмешиваются с весом ≤0.85, ранжирование остаётся детерминированным, \
WHY-объяснение включается в ответ. Возвращает score, URL, заголовок, сниппет.",
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
            "name": "poler_grep",
            "description": "Точный поиск в семантике grep: ВСЕ совпадения по файлам, \
без индекса, гарантия полноты («ноль значит ноль»). Обход с .gitignore (ripgrep-класс), \
контекст -A/-B, счётчики, exit-статус в тексте. Замена внешнего grep/ripgrep: \
используй для точных строк, имён функций, конфигов — когда нужна ПОЛНОТА, \
а не релевантность (для релевантности бери poler_search/poler_web_search). \
С json=true возвращает машинный отчёт: byte_offset строк, байтовые диапазоны \
вхождений, статистика — для верификации и навигации по файлу.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Искомая строка или regex (см. флаг regex)"},
                    "path": {"type": "string", "default": ".", "description": "Файл или корень обхода"},
                    "regex": {"type": "boolean", "default": false, "description": "PATTERN — регулярное выражение (grep -E)"},
                    "ignore_case": {"type": "boolean", "default": false, "description": "Регистронезависимость, Unicode-fold"},
                    "before": {"type": "integer", "default": 0, "minimum": 0, "maximum": 50, "description": "Строк контекста ДО (grep -B)"},
                    "after": {"type": "integer", "default": 0, "minimum": 0, "maximum": 50, "description": "Строк контекста ПОСЛЕ (grep -A)"},
                    "max_count": {"type": "integer", "minimum": 1, "description": "Останов после N совпавших строк на файл (grep -m)"},
                    "output": {"type": "string", "enum": ["content", "count", "list", "list_nonmatching"], "default": "content"},
                    "json": {"type": "boolean", "default": false, "description": "Машинно-читаемый отчёт вместо текста"}
                },
                "required": ["pattern"]
            }
        }),
        json!({
            "name": "poler_chunk",
            "description": "RAG-нарезка документа на чанки с якорями: passage-уровень \
вместо чтения файла целиком. Возвращает фрагменты с byte range, номерами строк, \
breadcrumb заголовков и числом токенов. Формат определяется автоматически: \
markdown (секции заголовков, code-fence не режется), код (границы строк, \
никогда внутри строки), текст (абзацы → предложения → слова). Замена внешнего \
RAG-конвейера chunking→retrieve: нарежь документ, прочитай релевантные куски \
(найди их через poler_search/poler_grep), цитируй по byte range.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Файл документа (md/txt/код)"},
                    "target_tokens": {"type": "integer", "default": 384, "minimum": 16, "maximum": 8192, "description": "Целевой размер чанка в токенах POLER"},
                    "overlap_tokens": {"type": "integer", "default": 48, "minimum": 0, "maximum": 2048, "description": "Перекрытие соседних чанков"},
                    "json": {"type": "boolean", "default": false, "description": "Машинно-читаемый отчёт вместо текста"}
                },
                "required": ["path"]
            }
        }),
        json!({
            "name": "query_poler_knowledge",
            "description": "Поиск по библиотеке POLER «Суверенный Гиппокамп»: 300 PTS-спек, \
6-томный математический трактат, ядро шифра PND, Шнайер (укр), 194 транскрипта. \
Гибрид BM25/WebRank + векторы (RaBitQ), кросс-языковый Semantic Bridge (рус↔англ). \
Эпистемическая градация: каждый хит несёт статус достоверности — mvr_verified \
(машинно доказано, ×1.5) / source_document (первоисточник, ×1.0) / narrative \
(нарратив, ×0.7), файл, номера строк, байтовый диапазон и прямую цитату. \
min_provenance отсекает недостоверное ('mvr' — только машинно доказанное). \
Цитаты точны: text == файл[byte_start..byte_end] — проверяемо.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Поисковая фраза (рус/укр/англ)"},
                    "top": {"type": "integer", "default": 8, "minimum": 1, "maximum": 30},
                    "min_provenance": {
                        "type": "string",
                        "enum": ["mvr", "source", "narrative"],
                        "description": "Минимальный эпистемический статус хитов"
                    }
                },
                "required": ["query"]
            }
        }),
        json!({
            "name": "poler_box_exec",
            "description": "Двухконтурный брокер (v0.26.0): исполняет команду В ИЗОЛИРОВАННОМ \
Docker-контейнере и возвращает stdout/stderr/exit-код. Схема: мозг агента \
(ты) → шлюз POLER (sandbox-судья: деструктив — Block ДО исполнения, \
подтверждения из MCP не принимаются в принципе) → контур исполнения \
(runner: net=none, только /workspace, без home; при его отсутствии — box). \
Одна команда без конвейеров и редиректов (кавычки — как в шлюзе; \
bash -c '…' допустим — payload судится рекурсивно). Путь к хост-системе \
НЕ существует физически: хост виден только как /workspace.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Команда (например: cargo build или python3 script.py)"},
                    "timeout_secs": {"type": "integer", "default": 120, "minimum": 1, "maximum": 600},
                    "target": {
                        "type": "string",
                        "enum": ["auto", "runner", "box"],
                        "default": "auto",
                        "description": "auto: runner (жёстче) → box"
                    }
                },
                "required": ["command"]
            }
        }),
        json!({
            "name": "poler_box_status",
            "description": "Состояние Container Jail для текущего workspace: \
box-контейнер (контуры 2/3, проброшенные агенты) и runner (контур \
исполнения poler_box_exec: net=none, только /workspace). Вызывай перед \
poler_box_exec, чтобы понять, поднят ли контур исполнения.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "required": []
            }
        }),
    ]
}

#[cfg(test)]
mod audit_tests {
    use super::*;

    #[test]
    fn path_guard_allows_root_and_blocks_outside() {
        let dir = std::env::temp_dir().join("poler-audit-guard");
        let _ = std::fs::create_dir_all(&dir);
        let inside = dir.join("secret.txt");
        std::fs::write(&inside, b"x").unwrap();
        // внутри корня — можно
        assert!(path_allowed_under(
            inside.to_str().unwrap(),
            &[dir.clone()]
        ));
        // /etc/passwd — нельзя
        assert!(!path_allowed_under("/etc/passwd", &[dir.clone()]));
        // symlink-escape: ссылка внутри корня, цель снаружи — нельзя
        let link = dir.join("escape-link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink("/etc/passwd", &link).unwrap();
        assert!(!path_allowed_under(link.to_str().unwrap(), &[dir]));
    }

    #[test]
    fn net_guard_blocks_metadata_and_link_local() {
        assert!(host_blocked_by_default("169.254.169.254"));
        assert!(host_blocked_by_default("metadata.google.internal"));
        assert!(host_blocked_by_default("metadata.goog"));
        assert!(host_blocked_by_default("0.0.0.0"));
        assert!(host_blocked_by_default("fe80::1"));
        assert!(host_blocked_by_default("fd00::1"));
        // легитимные цели — не блокируем
        assert!(!host_blocked_by_default("litnet.com"));
        assert!(!host_blocked_by_default("127.0.0.1")); // WebLens/локальная разработка
        assert!(!host_blocked_by_default("192.168.1.1")); // домашняя сеть
        assert!(!host_blocked_by_default("::1"));
        assert!(!host_blocked_by_default("docs.rs"));
    }

    #[test]
    fn net_guard_url_extraction_with_and_without_port() {
        // регрессия: rsplit_once(':') без порта раньше давал пустой host
        assert!(mcp_url_allowed("http://169.254.169.254/latest/meta-data/").is_err());
        assert!(mcp_url_allowed("http://169.254.169.254:8080/x?y=1").is_err());
        assert!(mcp_url_allowed("https://metadata.google.internal/computeMetadata/").is_err());
        assert!(mcp_url_allowed("https://litnet.com/").is_ok());
        assert!(mcp_url_allowed("http://127.0.0.1:8765/").is_ok());
    }
}

#[cfg(test)]
mod box_broker_tests {
    use super::*;

    /// Сериализация env-мутаций (POLER_BOX_DOCKER / POLER_WORKSPACE) —
    /// общий лок с containers/dispatch-тестами.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::gateway::containers::docker_env_test_lock()
    }

    fn server() -> McpServer {
        McpServer::new(9222, 10, PathBuf::from("/nonexistent-poler-test.db"))
    }

    /// Фейковый docker-клиент: пишет argv в лог-файл (env POLER_FAKE_LOG),
    /// отвечает на version/inspect/exec. Эмулирует РАБОТАЮЩИЙ daemon,
    /// у которого подняты и box, и runner (inspect → true).
    fn fake_docker(tag: &str) -> (PathBuf, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let base = std::env::temp_dir().join(format!("poler-mcp-fake-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let log = base.join("docker-calls.log");
        let script = base.join("fake-docker");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$POLER_FAKE_LOG\"\ncase \"$1\" in\n  version) echo '31.0.0-fake'; exit 0;;\n  inspect) echo 'true'; exit 0;;\n  exec) echo 'fake-exec-output'; exit 0;;\nesac\nexit 0\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        (script, log)
    }

    #[test]
    fn manifest_contains_box_broker_tools() {
        let m = tools_manifest();
        let names: Vec<&str> = m
            .iter()
            .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
            .collect();
        assert!(names.contains(&"poler_box_exec"), "нет poler_box_exec: {names:?}");
        assert!(names.contains(&"poler_box_status"), "нет poler_box_status: {names:?}");
        // входы описаны
        let exec = m.iter().find(|t| t.get("name").and_then(|v| v.as_str()) == Some("poler_box_exec")).unwrap();
        let req = exec.pointer("/inputSchema/required").unwrap();
        assert!(req.to_string().contains("command"), "required: {req}");
    }

    #[test]
    fn tools_call_routing_unknown_and_known() {
        let srv = server();
        let r = srv.dispatch(&serde_json::json!({
            "jsonrpc": "2.0", "id": 7,
            "method": "tools/call",
            "params": {"name": "poler_box_status", "arguments": {}}
        }));
        let r = r.expect("ответ есть");
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains("poler_box_exec"), "статус упоминает брокера: {text}");
    }

    #[test]
    fn box_exec_without_docker_honest_error() {
        let _g = env_lock();
        std::env::set_var("POLER_BOX_DOCKER", "/bin/false");
        let r = server().tool_box_exec(&json!({"command": "ls"}));
        std::env::remove_var("POLER_BOX_DOCKER");
        assert!(r.is_err(), "без docker — отказ: {r:?}");
        let e = r.unwrap_err();
        assert!(e.contains("docker"), "ошибка упоминает docker: {e}");
        assert!(e.contains("box"), "подсказка box runner on / box on: {e}");
    }

    #[test]
    fn box_exec_args_validation() {
        let srv = server();
        // нет command
        let e = srv.tool_box_exec(&json!({})).unwrap_err();
        assert!(e.contains("command"), "{e}");
        // пустой
        assert!(srv.tool_box_exec(&json!({"command": "   "})).is_err());
        // мусорный target
        let e = srv.tool_box_exec(&json!({"command": "ls", "target": "host"})).unwrap_err();
        assert!(e.contains("target"), "{e}");
        // конвейер не поддерживается
        let e = srv.tool_box_exec(&json!({"command": "ls | wc -l"})).unwrap_err();
        assert!(e.contains("конвейеры"), "{e}");
        // редирект не поддерживается
        let e = srv.tool_box_exec(&json!({"command": "ls > out.txt"})).unwrap_err();
        assert!(e.contains("редирект"), "{e}");
    }

    #[test]
    fn box_exec_destructive_blocked_before_any_docker_exec() {
        let _g = env_lock();
        let (docker, log) = fake_docker("destructive");
        let ws = log.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::env::set_var("POLER_BOX_DOCKER", &docker);
        std::env::set_var("POLER_FAKE_LOG", &log);
        std::env::set_var("POLER_WORKSPACE", &ws);

        // деструктив: Block ДО исполнения — docker exec не вызывается вовсе
        for destructive in ["rm -rf /", "dd if=/dev/zero of=/dev/sda", ":(){ :|:& };:"] {
            let r = server().tool_box_exec(&json!({"command": destructive}));
            assert!(r.is_err(), "{destructive} — обязан быть Block: {r:?}");
            let e = r.unwrap_err();
            assert!(e.contains("блокировка"), "{destructive}: {e}");
        }
        let log_content = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!log_content.contains("exec"), "docker exec не должен был вызываться: {log_content}");

        std::env::remove_var("POLER_BOX_DOCKER");
        std::env::remove_var("POLER_FAKE_LOG");
        std::env::remove_var("POLER_WORKSPACE");
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
    }

    #[test]
    fn box_exec_confirm_denied_zero_silent_escalation() {
        let _g = env_lock();
        let (docker, log) = fake_docker("confirm");
        let ws = log.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::env::set_var("POLER_BOX_DOCKER", &docker);
        std::env::set_var("POLER_FAKE_LOG", &log);
        std::env::set_var("POLER_WORKSPACE", &ws);

        // sudo: Confirm — MCP-агент подтвердить не может → отказ (не исполнение!)
        let r = server().tool_box_exec(&json!({"command": "sudo apt update"}));
        assert!(r.is_err(), "sudo из MCP — отказ: {r:?}");
        let e = r.unwrap_err();
        assert!(e.contains("владельцу"), "подсказка о владельце: {e}");
        let log_content = std::fs::read_to_string(&log).unwrap_or_default();
        assert!(!log_content.contains("exec"), "sudo не исполнялся: {log_content}");

        std::env::remove_var("POLER_BOX_DOCKER");
        std::env::remove_var("POLER_FAKE_LOG");
        std::env::remove_var("POLER_WORKSPACE");
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
    }

    #[test]
    fn box_exec_allowed_runs_in_runner_and_returns_output() {
        let _g = env_lock();
        let (docker, log) = fake_docker("allow");
        let ws = log.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::env::set_var("POLER_BOX_DOCKER", &docker);
        std::env::set_var("POLER_FAKE_LOG", &log);
        std::env::set_var("POLER_WORKSPACE", &ws);

        // target=auto: runner поднят (inspect → true) → исполнение в runner
        let r = server().tool_box_exec(&json!({"command": "python3 build.py --flag"}));
        assert!(r.is_ok(), "разрешённая команда: {r:?}");
        let out = r.unwrap();
        assert!(out.contains("runner"), "цель — runner: {out}");
        assert!(out.contains("poler-runner-"), "имя runner: {out}");
        assert!(out.contains("fake-exec-output"), "stdout вернулся: {out}");
        assert!(out.contains("exit 0"), "exit-код: {out}");

        // argv дошёл насквозь (кавычки раскрыты лексером шлюза)
        let log_content = std::fs::read_to_string(&log).unwrap();
        let exec_line = log_content
            .lines()
            .find(|l| l.starts_with("exec"))
            .expect("docker exec вызван");
        assert!(exec_line.contains("-i"), "пайповый режим: {exec_line}");
        assert!(exec_line.contains("POLER_RUNNER=1"), "брокер-контекст: {exec_line}");
        assert!(exec_line.contains("python3"), "argv насквозь: {exec_line}");
        assert!(exec_line.contains("build.py"), "argv насквозь: {exec_line}");
        assert!(exec_line.contains("--flag"), "argv насквозь: {exec_line}");
        assert!(!exec_line.contains("-it"), "без TTY: {exec_line}");

        // явный target=box — тоже работает (inspect=true у фейка)
        let r = server().tool_box_exec(&json!({"command": "ls", "target": "box"}));
        assert!(r.is_ok());
        assert!(r.unwrap().contains("box"));

        std::env::remove_var("POLER_BOX_DOCKER");
        std::env::remove_var("POLER_FAKE_LOG");
        std::env::remove_var("POLER_WORKSPACE");
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
    }

    #[test]
    fn box_status_reports_containers_with_fake_docker() {
        let _g = env_lock();
        let (docker, log) = fake_docker("status");
        let ws = log.parent().unwrap().join("ws");
        std::fs::create_dir_all(&ws).unwrap();
        std::env::set_var("POLER_BOX_DOCKER", &docker);
        std::env::set_var("POLER_WORKSPACE", &ws);

        let out = server().tool_box_status(&json!({})).unwrap();
        assert!(out.contains("docker"), "{out}");
        assert!(out.contains("box poler-box-"), "box-строка: {out}");
        assert!(out.contains("работает"), "inspect=true → работает: {out}");
        assert!(out.contains("runner poler-runner-"), "runner-строка: {out}");
        assert!(out.contains("poler_box_exec"), "подсказка брокера: {out}");

        std::env::remove_var("POLER_BOX_DOCKER");
        std::env::remove_var("POLER_WORKSPACE");
        let _ = std::fs::remove_dir_all(log.parent().unwrap());
    }
}

#[cfg(test)]
mod knowledge_tool_tests {
    use super::*;

    /// Сквозной тест Суверенного Гиппокампа: мини-библиотека → инжест →
    /// tools/list → tools/call query_poler_knowledge (провенанс, цитата,
    /// фильтр min_provenance, инструкция при отсутствии БД).
    #[test]
    fn knowledge_tool_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let d1 = dir.path().join("01_SPECS");
        std::fs::create_dir_all(&d1).unwrap();
        std::fs::write(
            d1.join("PTS-042.md"),
            "# PTS-042: Спека\n\n## 1. ОБЗОР\nнарративный обзор механики.\n\n\
             ## 4. ПОЛНЫЙ ТЕХНИЧЕСКИЙ ТЕКСТ\noriginal source text про диффузионный слой pndMix.\n",
        )
        .unwrap();
        let d3 = dir.path().join("03_TREATISE");
        std::fs::create_dir_all(&d3).unwrap();
        std::fs::write(
            d3.join("VOLUME_V_PND.md"),
            "# Том V\n\n## Теорема\nдиффузионный слой pndMix доказан золотым вектором.\n",
        )
        .unwrap();

        let kdb = dir.path().join("knowledge.db");
        let mut emb = crate::sources::knowledge::KnowledgeEmbedder::None;
        crate::sources::knowledge::ingest(
            dir.path(),
            &kdb,
            &mut emb,
            &crate::sources::knowledge::IngestOptions::default(),
        )
        .unwrap();

        let srv = McpServer::new(9222, 10, PathBuf::from("/nonexistent-web.db"))
            .with_knowledge_db(kdb.clone());

        // tools/list содержит новый инструмент
        let r = srv
            .dispatch(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}))
            .unwrap();
        assert!(serde_json::to_string(&r).unwrap().contains("query_poler_knowledge"));

        // tools/call: хиты с провенансом и цитатой
        let r = srv
            .dispatch(&json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "query_poler_knowledge",
                           "arguments": {"query": "pndMix диффузионный"}}
            }))
            .unwrap();
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains("MVR-VERIFIED"), "бейдж MVR: {text}");
        assert!(text.contains("TREATISE-V"), "док-ключ: {text}");
        assert!(text.contains("pndMix"), "цитата: {text}");

        // min_provenance=mvr: нарративные PTS-секции отсечены
        let r = srv
            .dispatch(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "query_poler_knowledge",
                           "arguments": {"query": "обзор механики", "min_provenance": "mvr"}}
            }))
            .unwrap();
        let text = serde_json::to_string(&r).unwrap();
        assert!(
            text.contains("0 хитов") || text.contains("ничего не найдено"),
            "mvr-фильтр жёсткий: {text}"
        );

        // без БД — инструкция, не isError-сбой
        let srv2 = McpServer::new(9222, 10, PathBuf::from("/nonexistent-web.db"))
            .with_knowledge_db(dir.path().join("missing.db"));
        let r = srv2
            .dispatch(&json!({
                "jsonrpc": "2.0", "id": 4, "method": "tools/call",
                "params": {"name": "query_poler_knowledge", "arguments": {"query": "x"}}
            }))
            .unwrap();
        let text = serde_json::to_string(&r).unwrap();
        assert!(text.contains("--knowledge-ingest"), "инструкция инжеста: {text}");
        assert!(!text.contains("\"isError\":true"), "состояние, не сбой: {text}");
    }
    // ── M6: резидентное состояние + стриминг логов ────────────────────

    /// Тёплый Гиппокамп: вторая dispatch переиспользует открытый хэндл
    /// (resident_stats) и даёт идентичный результат.
    #[test]
    fn warm_knowledge_reused_across_dispatches() {
        let dir = tempfile::tempdir().unwrap();
        let spec = dir.path().join("01_SPECS");
        std::fs::create_dir_all(&spec).unwrap();
        std::fs::write(
            spec.join("PTS-777.md"),
            "# PTS-777\n\n## 4. ПОЛНЫЙ ТЕХНИЧЕСКИЙ ТЕКСТ\nрезонансный аттрактор H Psi нулевой.\n",
        )
        .unwrap();
        let kdb = dir.path().join("knowledge.db");
        let mut emb = crate::sources::knowledge::KnowledgeEmbedder::None;
        crate::sources::knowledge::ingest(
            dir.path(),
            &kdb,
            &mut emb,
            &crate::sources::knowledge::IngestOptions::default(),
        )
        .unwrap();

        let srv = McpServer::new(9222, 10, PathBuf::from("/nonexistent-web.db"))
            .with_knowledge_db(kdb.clone());
        let call = |srv: &McpServer, id: i64| {
            srv.dispatch(&json!({
                "jsonrpc": "2.0", "id": id, "method": "tools/call",
                "params": {"name": "query_poler_knowledge",
                           "arguments": {"query": "резонансный аттрактор"}}
            }))
            .unwrap()
        };
        let text_of = |v: serde_json::Value| {
            v["result"]["content"][0]["text"].as_str().unwrap_or("").to_string()
        };
        let r1 = text_of(call(&srv, 1));
        assert!(
            serde_json::to_value(&srv.resident_stats()).unwrap()["warm_knowledge_open"]
                == serde_json::json!(true),
            "хэндл Гиппокампа обязан быть тёплым после первого вызова"
        );
        let r2 = text_of(call(&srv, 2));
        // первая строка содержит рендер латентности («N мс») — она не
        // часть контракта эквивалентности; сравниваем тело выдачи.
        let body = |t: &str| t.split('\n').skip(1).collect::<Vec<_>>().join("\n");
        assert_eq!(body(&r1), body(&r2), "повторный тёплый вызов идентичен (кроме id и мс)");
        assert!(!r1.is_empty(), "выдача не пуста: {r1}");
    }

    /// poler_grep через резидентный кэш: второй вызов без чтения диска.
    #[test]
    fn grep_dispatch_uses_warm_file_cache() {
        let _env = crate::gateway::containers::docker_env_test_lock();
        std::env::set_var("POLER_MCP_ALLOW_ANY_PATH", "1");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("canon.md"),
            "канон: резонанс семантический повторяется\n".repeat(64),
        )
        .unwrap();
        let srv = McpServer::new(9222, 10, PathBuf::from("/nonexistent-web.db"));
        let call = || {
            srv.dispatch(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "tools/call",
                "params": {"name": "poler_grep",
                           "arguments": {"pattern": "резонанс", "path": dir.path().to_str().unwrap()}}
            }))
            .unwrap()
        };
        call();
        let s1 = srv.resident_stats();
        assert!(s1["file_cache"]["misses"].as_u64().unwrap() >= 1, "первый вызов читает диск");
        call();
        let s2 = srv.resident_stats();
        assert!(
            s2["file_cache"]["hits"].as_u64().unwrap() >= 1,
            "второй вызов обязан идти из RAM: {s2}"
        );
        assert_eq!(
            s2["file_cache"]["misses"], s1["file_cache"]["misses"],
            "повторных чтений диска нет"
        );
        std::env::remove_var("POLER_MCP_ALLOW_ANY_PATH");
    }

    /// Стриминг логов в Vault: dispatch-события попадают в шифропоток,
    /// файл читается как обычный .pvt после Drop (финальный коммит).
    #[cfg(feature = "pnd-ffi")]
    #[test]
    fn vault_log_tee_records_dispatch() {
        use crate::crypto::vault::{open as vault_open, SealOptions, VaultAppender};

        let _env = crate::gateway::containers::docker_env_test_lock();
        std::env::set_var("POLER_MCP_ALLOW_ANY_PATH", "1");
        let dir = tempfile::tempdir().unwrap();
        let stream = dir.path().join("mcp-session.pvt");
        let app = VaultAppender::create(
            &stream,
            "сессионная-фраза",
            &SealOptions { iterations: crate::crypto::kdf::MIN_ITERATIONS, ..Default::default() },
        )
        .unwrap();
        let srv = McpServer::new(9222, 10, PathBuf::from("/nonexistent-web.db"))
            .with_vault_log(app);

        srv.dispatch(&json!({"jsonrpc": "2.0", "id": 1, "method": "ping"})).unwrap();
        srv.dispatch(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).unwrap();
        let r = srv
            .dispatch(&json!({
                "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                "params": {"name": "poler_grep",
                           "arguments": {"pattern": "x", "path": dir.path().to_str().unwrap()}}
            }))
            .unwrap();
        assert!(serde_json::to_string(&r).unwrap().contains("poler-grep"));
        drop(srv); // Drop аппендера = финальный коммит

        let out = dir.path().join("session.log");
        vault_open(&stream, &out, "сессионная-фраза").unwrap();
        let text = std::fs::read_to_string(&out).unwrap();
        assert!(text.contains("\"method\":\"ping\""), "ping в журнале: {text}");
        assert!(text.contains("\"method\":\"tools/list\""), "tools/list в журнале");
        assert!(text.contains("\"tool\":\"poler_grep\""), "tool-имя в журнале");
        assert!(text.contains("\"us\":"), "латентность в микросекундах в журнале");
        assert!(text.contains("\"ok\":true"), "статус в журнале");
        std::env::remove_var("POLER_MCP_ALLOW_ANY_PATH");
    }

}
