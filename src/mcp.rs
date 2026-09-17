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
//! C2/v0.33.0 «Живая муха»: семейство poler_fly_* — резидентный
//! коннектом FLYCSR1 (мозг мухи FlyWire v783) как матрица A: паспорт
//! нейрона, ребро/ротор, K-hop, кратчайший путь, общие партнёры,
//! центральность/PageRank, глобальный топ циркуляции J, мотивы и
//! симуляция распространения сигнала. Артефакт грузится ОДИН раз
//! и живёт в RAM между вызовами (WarmFly); тяжёлые запросы уходят
//! в пул воркеров — агент может допросить муху параллельно.
//!
//! Chromium поднимается автоматически при первом poler_crawl/poler_fetch
//! (см. `web::ensure_chromium`).

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::graph::connectome::{Connectome, ConnectomeNodes, InEdges, SignFilter};
use crate::graph::flyops::Direction;
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
    /// E2/v0.32.0: реестр фоновых задач poler_exec_async (отмена/опрос).
    #[cfg(feature = "pnd-ffi")]
    exec_tasks: Arc<crate::exec::TaskRegistry>,
    /// C2/v0.33.0: резидентная «живая муха» — коннектом + CSC в RAM,
    /// переиспользуется между вызовами poler_fly_* (холодный старт был
    /// ~60–300 мс загрузки + ~43 мс CSC на КАЖДЫЙ CLI-вызов агента).
    warm_fly: Mutex<Option<Arc<WarmFly>>>,
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
            #[cfg(feature = "pnd-ffi")]
            exec_tasks: Arc::new(crate::exec::TaskRegistry::default()),
            warm_fly: Mutex::new(None),
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

/// E2/v0.32.0: инструменты исполнителя уходят в пул воркеров — агент может
/// запускать команды ПАРАЛЛЕЛЬНО (в E1 один poler_exec блокировал весь
/// stdio-цикл: 20 стресс-тестов вставали в очередь).
#[cfg(feature = "pnd-ffi")]
fn is_exec_tool(msg: &Value) -> bool {
    msg.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && matches!(
            msg.pointer("/params/name").and_then(|v| v.as_str()),
            Some(
                "poler_exec"
                    | "poler_exec_async"
                    | "poler_exec_task"
                    | "poler_exec_kill"
                    | "poler_exec_list"
            )
        )
}

/// C2/v0.33.0: семейство «живой мухи» уходит в пул воркеров — PageRank
/// полного коннектома и симуляция занимают сотни миллисекунд, а агент
/// часто допросает муху пачкой независимых запросов.
fn is_fly_tool(msg: &Value) -> bool {
    msg.get("method").and_then(|m| m.as_str()) == Some("tools/call")
        && matches!(
            msg.pointer("/params/name").and_then(|v| v.as_str()),
            Some(
                "poler_fly"
                    | "poler_fly_node"
                    | "poler_fly_edge"
                    | "poler_fly_khop"
                    | "poler_fly_path"
                    | "poler_fly_common"
                    | "poler_fly_centrality"
                    | "poler_fly_rotor"
                    | "poler_fly_motifs"
                    | "poler_fly_propagate"
            )
        )
}

/// Резидентная «живая муха»: коннектом FLYCSR1 + таблица root_id + CSC
/// (входящие рёбра) в RAM. CSC строится лениво один раз — первый
/// impact/центральность запрос платит ~43 мс, остальные — ноль.
pub struct WarmFly {
    /// Сам коннектом (CSR в RAM).
    con: Arc<Connectome>,
    /// Таблица 138 639 root_id (опциональна — для перевода индекс ↔ FlyWire).
    nodes: Option<ConnectomeNodes>,
    /// Путь загруженного артефакта (сверка при повторном csr).
    csr_path: PathBuf,
    /// Время загрузки артефакта, мс.
    load_ms: u128,
    /// CSC-транспонирование (ленивое, один раз).
    csc: std::sync::OnceLock<InEdges>,
}

impl WarmFly {
    /// CSC «кто управляет нейроном» — строится при первом обращении.
    fn csc(&self) -> &InEdges {
        self.csc.get_or_init(|| self.con.build_in_edges())
    }

    /// Построен ли уже CSC (диагностика теплоты).
    fn csc_ready(&self) -> bool {
        self.csc.get().is_some()
    }
}

/// Загрузка таблицы root_id с проверкой соответствия коннектому.
fn fly_load_nodes(path: &str, con: &Connectome) -> Result<ConnectomeNodes, String> {
    let tbl = ConnectomeNodes::load(std::path::Path::new(path))?;
    if tbl.len() != con.n_nodes() {
        return Err(format!(
            "{} содержит {} root_id, а коннектом ждёт {} узлов",
            path,
            tbl.len(),
            con.n_nodes()
        ));
    }
    Ok(tbl)
}

/// Запуск MCP-сервера (stdio). Возвращает код процесса (0 = чистый EOF stdin).
/// E2: tools/call poler_exec* исполняются в пуле воркеров параллельно;
/// ответы пишутся по мере готовности (JSON-RPC допускает внеочередность —
/// клиент сопоставляет по id).
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
    let server = Arc::new(server);

    // Пул воркеров: перекладывает долгие tools/call (exec-семейство) из
    // читающего потока. Лок на recv держится только на время ОЖИДАНИЯ —
    // полученные задачи исполняются параллельно.
    let (tx, rx) = std::sync::mpsc::channel::<Box<dyn FnOnce() + Send + 'static>>();
    let rx = Arc::new(Mutex::new(rx));
    let workers = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(2)
        .clamp(2, 8);
    let mut handles = Vec::with_capacity(workers);
    for _ in 0..workers {
        let rx = rx.clone();
        handles.push(std::thread::spawn(move || loop {
            let job = {
                let guard = rx.lock().expect("poler-mcp: отравленный лок пула");
                guard.recv()
            };
            let Ok(job) = job else { break };
            job();
        }));
    }

    eprintln!(
        "poler-mcp: stdio JSON-RPC, db={:?}, knowledge={:?}, cdp_port={}, wait_ms={}, workers={} (параллельный poler_exec)",
        server.db_path, server.knowledge_db, server.cdp_port, server.wait_ms, workers
    );
    let stdin = std::io::stdin();
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
                    &mut std::io::stdout().lock(),
                    &json!({"jsonrpc":"2.0","id":null,"error":{
                        "code":-32700,"message":format!("parse error: {e}")
                    }}),
                );
                continue;
            }
        };
        #[cfg(feature = "pnd-ffi")]
        if is_exec_tool(&msg) || is_fly_tool(&msg) {
            let server = server.clone();
            let _ = tx.send(Box::new(move || {
                if let Some(resp) = server.dispatch(&msg) {
                    let _ = write_line(&mut std::io::stdout().lock(), &resp);
                }
            }));
            continue;
        }
        #[cfg(not(feature = "pnd-ffi"))]
        if is_fly_tool(&msg) {
            let server = server.clone();
            let _ = tx.send(Box::new(move || {
                if let Some(resp) = server.dispatch(&msg) {
                    let _ = write_line(&mut std::io::stdout().lock(), &resp);
                }
            }));
            continue;
        }
        if let Some(resp) = server.dispatch(&msg) {
            let _ = write_line(&mut std::io::stdout().lock(), &resp);
        }
    }
    drop(tx);
    for h in handles {
        let _ = h.join();
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
            #[cfg(feature = "pnd-ffi")]
            "poler_exec" => self.tool_exec(&args),
            #[cfg(feature = "pnd-ffi")]
            "poler_exec_async" => self.tool_exec_async(&args),
            #[cfg(feature = "pnd-ffi")]
            "poler_exec_task" => self.tool_exec_task(&args),
            #[cfg(feature = "pnd-ffi")]
            "poler_exec_kill" => self.tool_exec_kill(&args),
            #[cfg(feature = "pnd-ffi")]
            "poler_exec_list" => self.tool_exec_list(&args),
            "poler_box_exec" => self.tool_box_exec(&args),
            "poler_box_status" => self.tool_box_status(&args),
            "query_poler_knowledge" => self.tool_knowledge_query(&args),
            "poler_fly" => self.tool_fly(&args),
            "poler_fly_node" => self.tool_fly_node(&args),
            "poler_fly_edge" => self.tool_fly_edge(&args),
            "poler_fly_khop" => self.tool_fly_khop(&args),
            "poler_fly_path" => self.tool_fly_path(&args),
            "poler_fly_common" => self.tool_fly_common(&args),
            "poler_fly_centrality" => self.tool_fly_centrality(&args),
            "poler_fly_rotor" => self.tool_fly_rotor(&args),
            "poler_fly_motifs" => self.tool_fly_motifs(&args),
            "poler_fly_propagate" => self.tool_fly_propagate(&args),
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
    // poler_exec (E2/v0.32.0): идеальный исполнитель + фоновые задачи
    // -----------------------------------------------------------------
    /// Разбор общих аргументов запуска. Умолчание capture для агентов —
    /// head_tail: и голова, и хвост огромного вывода (E1 терял голову).
    #[cfg(feature = "pnd-ffi")]
    fn exec_spec_from_args(args: &Value) -> Result<crate::exec::ExecSpec, String> {
        use crate::exec::{CaptureMode, ExecSpec};

        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or("аргумент command (строка) обязателен")?
            .to_string();
        if command.contains('\0') {
            return Err("command не может содержать NUL".into());
        }
        let cmd_args: Vec<String> = args
            .get("args")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if cmd_args.iter().any(|a| a.contains('\0')) {
            return Err("аргументы не могут содержать NUL".into());
        }
        let capture = match args.get("capture").and_then(|v| v.as_str()) {
            None => CaptureMode::HeadTail,
            Some(s) => CaptureMode::parse(s)
                .ok_or_else(|| format!("неизвестный capture {s:?} (доступно: tail | head_tail)"))?,
        };
        let env = args.get("env").and_then(|v| v.as_object()).map(|o| {
            o.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                .collect::<Vec<_>>()
        });
        Ok(ExecSpec {
            program: command,
            args: cmd_args,
            env,
            timeout_ms: args.get("timeout_ms").and_then(|v| v.as_u64()).unwrap_or(30_000),
            grace_ms: args.get("grace_ms").and_then(|v| v.as_u64()).unwrap_or(100),
            max_out_bytes: args
                .get("max_out_bytes")
                .and_then(|v| v.as_u64())
                .unwrap_or(65_536) as usize,
            stdin_data: args.get("stdin").and_then(|v| v.as_str()).map(|s| s.as_bytes().to_vec()),
            cwd: args.get("cwd").and_then(|v| v.as_str()).map(String::from),
            pty: args.get("pty").and_then(|v| v.as_bool()).unwrap_or(false),
            capture,
            cancel: None,
        })
    }

    /// JSON-отчёт исполнения (общий для poler_exec и poler_exec_task).
    #[cfg(feature = "pnd-ffi")]
    fn exec_report_json(out: &crate::exec::ExecOutcome) -> Value {
        json!({
            "exit_code": out.exit_code,
            "signal": out.signal,
            "timed_out": out.timed_out,
            "cancelled": out.cancelled,
            "truncated": out.truncated,
            "pty": out.pty,
            "stdout": String::from_utf8_lossy(&out.stdout),
            "stderr": String::from_utf8_lossy(&out.stderr),
            "stdout_bytes": out.stdout.len(),
            "stderr_bytes": out.stderr.len(),
            "duration_us": out.duration_us,
            "pid": out.pid
        })
    }

    /// Запуск команды через Zig-ядро (raw-syscalls, os/core/poler_exec.zig).
    /// Блокирует до завершения/таймаута/отмены. Для фонового запуска —
    /// poler_exec_async; JSON-отчёт: exit_code/signal/stdout/stderr/
    /// timed_out/cancelled/truncated/duration_us/pid.
    #[cfg(feature = "pnd-ffi")]
    fn tool_exec(&self, args: &Value) -> Result<String, String> {
        use crate::exec::{self, ExecSpec};

        let spec: ExecSpec = Self::exec_spec_from_args(args)?;
        match exec::run(&spec) {
            Ok(out) => Ok(serde_json::to_string_pretty(&Self::exec_report_json(&out))
                .unwrap_or_else(|_| "{}".into())),
            Err(e) => Ok(format!(
                "{{\n  \"error\": \"{e}\",\n  \"exit_code\": {}\n}}",
                e.exit_code()
            )),
        }
    }

    /// Фоновый запуск: task_id сразу, результат — через poler_exec_task.
    /// Задача живёт в собственном потоке реестра; отмена — poler_exec_kill.
    #[cfg(feature = "pnd-ffi")]
    fn tool_exec_async(&self, args: &Value) -> Result<String, String> {
        use crate::exec::ExecSpec;

        let spec: ExecSpec = Self::exec_spec_from_args(args)?;
        let display = if spec.args.is_empty() {
            spec.program.clone()
        } else {
            format!("{} {}", spec.program, spec.args.join(" "))
        };
        let id = crate::exec::TaskRegistry::spawn(&self.exec_tasks, display, spec);
        let report = json!({
            "task_id": id,
            "status": "running",
            "hint": "опрос: poler_exec_task {task_id} (+wait_ms); отмена: poler_exec_kill {task_id}"
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// Опрос/ожидание фоновой задачи: wait_ms — сколько ждать завершения
    /// (0 — мгновенный снимок). Возвращает running-статус или полный отчёт.
    #[cfg(feature = "pnd-ffi")]
    fn tool_exec_task(&self, args: &Value) -> Result<String, String> {
        use crate::exec::TaskSnapshot;

        let id = args
            .get("task_id")
            .and_then(|v| v.as_u64())
            .ok_or("аргумент task_id (число) обязателен")?;
        let wait_ms = args.get("wait_ms").and_then(|v| v.as_u64()).unwrap_or(0).min(60_000);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms);
        loop {
            match self.exec_tasks.snapshot(id) {
                None => return Err(format!("задача {id} не найдена")),
                Some(TaskSnapshot::Running { command, elapsed_us }) => {
                    if std::time::Instant::now() >= deadline {
                        let report = json!({
                            "task_id": id, "status": "running",
                            "command": command, "elapsed_us": elapsed_us
                        });
                        return Ok(serde_json::to_string_pretty(&report)
                            .unwrap_or_else(|_| "{}".into()));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Some(TaskSnapshot::Done { command, elapsed_us, result }) => {
                    let report = match result {
                        Ok(out) => {
                            let mut r = Self::exec_report_json(&out);
                            r["task_id"] = json!(id);
                            r["command"] = json!(command);
                            r["elapsed_us"] = json!(elapsed_us);
                            r["status"] = json!("done");
                            r
                        }
                        Err(e) => json!({
                            "task_id": id, "status": "done", "command": command,
                            "elapsed_us": elapsed_us,
                            "error": e.to_string(), "exit_code": e.exit_code()
                        }),
                    };
                    return Ok(serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|_| "{}".into()));
                }
            }
        }
    }

    /// Отмена фоновой задачи: атомарный флаг → TERM → grace → KILL
    /// (ядро замечает отмену за ≤25 мс). Результат — через poler_exec_task.
    #[cfg(feature = "pnd-ffi")]
    fn tool_exec_kill(&self, args: &Value) -> Result<String, String> {
        let id = args
            .get("task_id")
            .and_then(|v| v.as_u64())
            .ok_or("аргумент task_id (число) обязателен")?;
        match self.exec_tasks.kill(id) {
            Some(true) => Ok(format!(
                "{{\n  \"task_id\": {id},\n  \"killed\": true,\n  \"note\": \"TERM→KILL отправлен; результат — poler_exec_task {id}\"\n}}"
            )),
            Some(false) => Ok(format!(
                "{{\n  \"task_id\": {id},\n  \"killed\": false,\n  \"note\": \"задача уже завершена\"\n}}"
            )),
            None => Err(format!("задача {id} не найдена")),
        }
    }

    /// Список фоновых задач (живые + завершённые, до 128 в реестре).
    #[cfg(feature = "pnd-ffi")]
    fn tool_exec_list(&self, _args: &Value) -> Result<String, String> {
        use crate::exec::TaskSnapshot;

        let tasks: Vec<Value> = self
            .exec_tasks
            .list()
            .into_iter()
            .map(|(id, snap)| match snap {
                TaskSnapshot::Running { command, elapsed_us } => json!({
                    "task_id": id, "status": "running",
                    "command": command, "elapsed_us": elapsed_us
                }),
                TaskSnapshot::Done { command, elapsed_us, result } => match result {
                    Ok(out) => json!({
                        "task_id": id, "status": "done", "command": command,
                        "elapsed_us": elapsed_us, "exit_code": out.exit_code,
                        "cancelled": out.cancelled, "timed_out": out.timed_out
                    }),
                    Err(e) => json!({
                        "task_id": id, "status": "done", "command": command,
                        "elapsed_us": elapsed_us, "error": e.to_string()
                    }),
                },
            })
            .collect();
        let count = tasks.len();
        let report = json!({ "tasks": tasks, "count": count });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    // -----------------------------------------------------------------
    // C2/v0.33.0 «Живая муха»: коннектом FLYCSR1 как резидентный объект
    // допроса для агента. Get-or-load + 10 инструментов ниже.
    // -----------------------------------------------------------------

    /// Get-or-load тёплого коннектома: csr — путь к .csr.zst (первый
    /// вызов обязан его передать; далее артефакт живёт в RAM), nodes —
    /// путь к nodes.bin (опционально: перевод индекс ↔ root_id FlyWire).
    fn fly_state(&self, csr: Option<&str>, nodes: Option<&str>) -> Result<Arc<WarmFly>, String> {
        let mut guard = self
            .warm_fly
            .lock()
            .expect("poler-mcp: отравленный fly-лок");
        // Тёплый артефакт: тот же путь (или запрос без csr) → reuse.
        if let Some(warm) = guard.as_ref() {
            let same = csr.map(|p| PathBuf::from(p) == warm.csr_path).unwrap_or(true);
            if same {
                if let Some(p) = nodes {
                    if warm.nodes.is_none() {
                        let tbl = fly_load_nodes(p, &warm.con)?;
                        let upgraded = Arc::new(WarmFly {
                            con: warm.con.clone(),
                            nodes: Some(tbl),
                            csr_path: warm.csr_path.clone(),
                            load_ms: warm.load_ms,
                            csc: std::sync::OnceLock::new(),
                        });
                        *guard = Some(upgraded.clone());
                        return Ok(upgraded);
                    }
                }
                return Ok(warm.clone());
            }
        }
        let Some(csr) = csr else {
            return Err(
                "коннектом не загружен: передайте csr (путь к .csr.zst) хотя бы \
                 в одном вызове poler_fly*"
                    .to_string(),
            );
        };
        guard_path(csr)?;
        let t0 = std::time::Instant::now();
        let con = Connectome::load(std::path::Path::new(csr))?;
        let load_ms = t0.elapsed().as_millis();
        let tbl = nodes.map(|p| fly_load_nodes(p, &con)).transpose()?;
        let warm = Arc::new(WarmFly {
            con: Arc::new(con),
            nodes: tbl,
            csr_path: PathBuf::from(csr),
            load_ms,
            csc: std::sync::OnceLock::new(),
        });
        *guard = Some(warm.clone());
        Ok(warm)
    }

    /// Спецификация нейрона из JSON: индекс (число/строка) или root_id.
    fn fly_spec(v: &Value) -> Option<String> {
        v.as_str()
            .map(str::to_string)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
    }

    /// Резолв спецификации: индекс CSR либо root_id (нужна таблица узлов).
    fn fly_resolve(w: &WarmFly, spec: &str) -> Result<usize, String> {
        if let Ok(idx) = spec.parse::<usize>() {
            if idx < w.con.n_nodes() {
                return Ok(idx);
            }
        }
        if let Some(ns) = &w.nodes {
            if let Ok(rid) = spec.parse::<u64>() {
                if let Some(i) = ns.idx_of(rid) {
                    return Ok(i);
                }
            }
        }
        Err(format!(
            "нейрон «{spec}» не найден (узлов: {}; формат: индекс 0..{} либо root_id с nodes)",
            w.con.n_nodes(),
            w.con.n_nodes().saturating_sub(1)
        ))
    }

    /// Обязательный аргумент-нейрон (ключ — имя аргумента).
    fn fly_neuron_arg(args: &Value, key: &str, w: &WarmFly) -> Result<usize, String> {
        let raw = args.get(key).ok_or(format!("аргумент {key} обязателен"))?;
        let spec = Self::fly_spec(raw)
            .ok_or(format!("аргумент {key}: индекс (число) или root_id (строка)"))?;
        Self::fly_resolve(w, &spec)
    }

    /// Массив нейронов (1..=lim) → индексы.
    fn fly_neurons_arg(args: &Value, key: &str, w: &WarmFly, lim: usize) -> Result<Vec<usize>, String> {
        let arr = args
            .get(key)
            .and_then(|v| v.as_array())
            .ok_or(format!("аргумент {key} (массив индексов/root_id) обязателен"))?;
        if arr.is_empty() {
            return Err(format!("{key}: пустой массив"));
        }
        if arr.len() > lim {
            return Err(format!("{key}: {} элементов — максимум {lim}", arr.len()));
        }
        arr.iter()
            .map(|v| {
                let spec = Self::fly_spec(v)
                    .ok_or(format!("{key}: индекс (число) или root_id (строка)"))?;
                Self::fly_resolve(w, &spec)
            })
            .collect()
    }

    /// Фильтр знака (умолчание all).
    fn fly_sign_arg(args: &Value) -> Result<SignFilter, String> {
        match args.get("sign") {
            None | Some(Value::Null) => Ok(SignFilter::All),
            Some(v) => SignFilter::parse(v.as_str().ok_or("sign: строка all | exc | inh")?),
        }
    }

    /// Направление (умолчание out; для common — down).
    fn fly_dir_arg(args: &Value) -> Result<Direction, String> {
        match args.get("direction") {
            None | Some(Value::Null) => Ok(Direction::Out),
            Some(v) => Direction::parse(v.as_str().ok_or("direction: строка out | in (down | up)")?),
        }
    }

    /// Корень узла (root_id FlyWire, если таблица загружена).
    fn fly_root(w: &WarmFly, idx: usize) -> Value {
        w.nodes.as_ref().and_then(|n| n.root_id(idx)).map(Value::from).unwrap_or(Value::Null)
    }

    /// JSON-объект ребра (other = другой конец).
    fn fly_edge_json(w: &WarmFly, e: &crate::graph::connectome::Edge) -> Value {
        json!({
            "other": e.target,
            "other_root_id": Self::fly_root(w, e.target as usize),
            "weight": e.weight,
            "nt": e.nt_name(),
            "sign": e.sign(),
            "signed_weight": e.signed_weight(),
        })
    }

    /// poler_fly: загрузка/сводка/выгрузка коннектома. Первый вызов
    /// грузит артефакт в RAM (csr + опционально nodes), дальше всё
    /// семейство работает без диска. action: summary (умолчание) | eject.
    fn tool_fly(&self, args: &Value) -> Result<String, String> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("summary");
        if matches!(action, "eject" | "выгрузить") {
            let mut guard = self
                .warm_fly
                .lock()
                .expect("poler-mcp: отравленный fly-лок");
            let had = guard.take().is_some();
            let report = json!({
                "action": "eject",
                "unloaded": had,
                "note": "коннектом выгружен из RAM; следующий poler_fly загрузит заново"
            });
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()));
        }
        if !matches!(action, "summary" | "сводка") {
            return Err(format!("неизвестное действие «{action}» (summary | eject)"));
        }
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let con = &w.con;
        let m = con.mass_by_nt();
        let exc = m[1].0 + m[2].0;
        let inh = m[0].0;
        let modm = m[3].0 + m[4].0 + m[5].0;
        let tot = con.total_mass();
        let pct = |x: u64| format!("{:.1}%", x as f64 / tot as f64 * 100.0);
        let report = json!({
            "mode": "summary",
            "artifact": w.csr_path.display().to_string(),
            "core": con.is_core(),
            "n_nodes": con.n_nodes(),
            "n_edges": con.n_edges(),
            "total_mass": tot,
            "load_ms": w.load_ms,
            "warm": {"csc_built": w.csc_ready(), "has_nodes": w.nodes.is_some()},
            "nt": (0..6).map(|c| json!({
                "name": crate::graph::connectome::NT_NAMES[c],
                "edges": m[c].1,
                "mass": m[c].0,
                "sign": crate::graph::connectome::nt_sign(c as u8),
            })).collect::<Vec<_>>(),
            "mass_balance": {
                "excitatory": exc, "inhibitory": inh, "modulatory": modm,
                "excitatory_pct": pct(exc), "inhibitory_pct": pct(inh), "modulatory_pct": pct(modm),
            },
            "hint": "семейство: poler_fly_node/edge/khop/path/common/centrality/rotor/motifs/propagate (csr больше не нужен — муха в RAM)",
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_node: паспорт нейрона — степени, массы, топ партнёров
    /// в обоих направлениях (с root_id, медиаторами и знаками).
    fn tool_fly_node(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let idx = Self::fly_neuron_arg(args, "neuron", &w)?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(8)
            .clamp(1, 500) as usize;
        let con = &w.con;
        let csc = w.csc();
        let outs: Vec<crate::graph::connectome::Edge> =
            con.out_edges(idx).unwrap().collect();
        let out_mass: u64 = outs.iter().map(|e| e.weight as u64).sum();
        let ins: Vec<crate::graph::connectome::Edge> =
            csc.in_edges(con, idx).unwrap().collect();
        let in_mass: u64 = ins.iter().map(|e| e.weight as u64).sum();
        let mut top_out = outs.clone();
        top_out.sort_by_key(|e| std::cmp::Reverse(e.weight));
        top_out.truncate(limit);
        let mut top_in = ins.clone();
        top_in.sort_by_key(|e| std::cmp::Reverse(e.weight));
        top_in.truncate(limit);
        let report = json!({
            "neuron": idx,
            "root_id": Self::fly_root(&w, idx),
            "out_degree": outs.len(),
            "out_mass": out_mass,
            "in_degree": ins.len(),
            "in_mass": in_mass,
            "top_out": top_out.iter().map(|e| Self::fly_edge_json(&w, e)).collect::<Vec<_>>(),
            "top_in": top_in.iter().map(|e| Self::fly_edge_json(&w, e)).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_edge: ребро u→v и ротор J = A − Aᵀ пары (циркуляция
    /// влияния: однонаправленный поток или реципрокная компенсация).
    fn tool_fly_edge(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let u = Self::fly_neuron_arg(args, "u", &w)?;
        let v = Self::fly_neuron_arg(args, "v", &w)?;
        let con = &w.con;
        let fwd = con.edge(u, v);
        let bwd = con.edge(v, u);
        let rotor = con.rotor(u, v);
        if fwd.is_none() && bwd.is_none() {
            let report = json!({
                "u": u, "u_root_id": Self::fly_root(&w, u),
                "v": v, "v_root_id": Self::fly_root(&w, v),
                "found": false,
                "rotor_Juv": 0,
            });
            return Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()));
        }
        let report = json!({
            "u": u, "u_root_id": Self::fly_root(&w, u),
            "v": v, "v_root_id": Self::fly_root(&w, v),
            "found": true,
            "forward": fwd.map(|e| Self::fly_edge_json(&w, &e)),
            "backward": bwd.map(|e| Self::fly_edge_json(&w, &e)),
            "rotor_Juv": rotor,
            "rotor_Jvu": -rotor,
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_khop: BFS потока сигнала от нейрона — размеры фронтов
    /// по хопам и всего достигнуто (фильтр знака сужает поток).
    fn tool_fly_khop(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let idx = Self::fly_neuron_arg(args, "neuron", &w)?;
        let depth = args
            .get("depth")
            .and_then(|v| v.as_u64())
            .unwrap_or(2)
            .clamp(1, 10) as usize;
        let filter = Self::fly_sign_arg(args)?;
        let r = w.con.k_hop(idx, depth, filter);
        let report = json!({
            "start": idx,
            "start_root_id": Self::fly_root(&w, idx),
            "depth": depth,
            "sign_filter": args.get("sign").and_then(|v| v.as_str()).unwrap_or("all"),
            "frontier_sizes": r.frontier_sizes,
            "visited": r.visited,
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_path: кратчайший путь сигнала from→to с цепочкой
    /// прыжков (вес/медиатор/знак каждого синапса на маршруте).
    fn tool_fly_path(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let from = Self::fly_neuron_arg(args, "from", &w)?;
        let to = Self::fly_neuron_arg(args, "to", &w)?;
        let filter = Self::fly_sign_arg(args)?;
        let r = w.con.shortest_path(from, to, filter);
        let report = json!({
            "from": from, "from_root_id": Self::fly_root(&w, from),
            "to": to, "to_root_id": Self::fly_root(&w, to),
            "sign_filter": args.get("sign").and_then(|v| v.as_str()).unwrap_or("all"),
            "found": r.found,
            "length": r.length(),
            "total_weight": r.total_weight(),
            "hops": r.hops.iter().map(|h| json!({
                "from": h.from, "from_root_id": Self::fly_root(&w, h.from),
                "to": h.to, "to_root_id": Self::fly_root(&w, h.to),
                "weight": h.edge.weight,
                "nt": h.edge.nt_name(),
                "sign": h.edge.sign(),
                "signed_weight": h.edge.signed_weight(),
            })).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_common: пересечение окрестностей набора нейронов —
    /// общие мишени (down) или общие источники (up): конвергенция
    /// и дивергенция схемы мозга.
    fn tool_fly_common(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let idxs = Self::fly_neurons_arg(args, "neurons", &w, 32)?;
        let dir = Self::fly_dir_arg(args)?;
        let filter = Self::fly_sign_arg(args)?;
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .clamp(1, 500) as usize;
        let mut partners = w.con.common_partners(&idxs, dir, filter, w.csc())?;
        let count_full = partners.len();
        partners.truncate(limit);
        let report = json!({
            "neurons": idxs,
            "direction": match dir { Direction::Out => "down", Direction::In => "up" },
            "sign_filter": args.get("sign").and_then(|v| v.as_str()).unwrap_or("all"),
            "count": count_full,
            "partners": partners.iter().map(|p| json!({
                "node": p.node,
                "root_id": Self::fly_root(&w, p.node),
                "total_weight": p.total_weight,
                "links": p.members.iter().map(|(q, e)| json!({
                    "query": q,
                    "edge": Self::fly_edge_json(&w, e),
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_centrality: хабы мозга — топ по степеням + взвешенный
    /// PageRank (модуляторы не проводят ранг; фильтр знака сужает граф).
    fn tool_fly_centrality(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let top = args
            .get("top")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 100) as usize;
        let filter = Self::fly_sign_arg(args)?;
        let damping = args
            .get("damping")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.85)
            .clamp(0.05, 0.99);
        let iterations = args
            .get("iterations")
            .and_then(|v| v.as_u64())
            .unwrap_or(30)
            .clamp(1, 200) as usize;
        let con = &w.con;
        let csc = w.csc();
        let top_out = con.degree_ranking(Direction::Out, top, csc);
        let top_in = con.degree_ranking(Direction::In, top, csc);
        let pr = con.pagerank(filter, damping, iterations, 1e-9, top);
        let report = json!({
            "sign_filter": args.get("sign").and_then(|v| v.as_str()).unwrap_or("all"),
            "top_out_degree": top_out.iter().map(|&(u, d)| json!({
                "neuron": u, "root_id": Self::fly_root(&w, u), "out_degree": d,
            })).collect::<Vec<_>>(),
            "top_in_degree": top_in.iter().map(|&(v, d)| json!({
                "neuron": v, "root_id": Self::fly_root(&w, v), "in_degree": d,
            })).collect::<Vec<_>>(),
            "pagerank": {
                "damping": damping,
                "iterations": pr.iterations,
                "converged": pr.converged,
                "top": pr.top.iter().map(|&(u, r)| json!({
                    "neuron": u, "root_id": Self::fly_root(&w, u), "rank": r,
                })).collect::<Vec<_>>(),
            },
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_rotor: глобальный топ пар по циркуляции J = A − Aᵀ —
    /// самые однонаправленные влияния мозга (чистые потоки без
    /// встречной компенсации).
    fn tool_fly_rotor(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let top = args
            .get("top")
            .and_then(|v| v.as_u64())
            .unwrap_or(10)
            .clamp(1, 1000) as usize;
        let min_abs = args
            .get("min_abs")
            .and_then(|v| v.as_i64())
            .unwrap_or(1)
            .clamp(1, i32::MAX as i64) as i32;
        let pairs = w.con.rotor_top(top, min_abs);
        let count = pairs.len();
        let report = json!({
            "top": top,
            "min_abs": min_abs,
            "count": count,
            "pairs": pairs.iter().map(|p| json!({
                "u": p.u, "u_root_id": Self::fly_root(&w, p.u),
                "v": p.v, "v_root_id": Self::fly_root(&w, p.v),
                "J_uv": p.j,
                "J_vu": -p.j,
                "forward_signed": p.forward,
                "backward_signed": p.backward,
            })).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_motifs: перепись мотивов вокруг нейрона — реципрокные
    /// пары, feedforward-треугольники, feedback-циклы (местные схемы
    /// усиления и обратной связи).
    fn tool_fly_motifs(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let idx = Self::fly_neuron_arg(args, "neuron", &w)?;
        let examples = args
            .get("examples")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .min(50) as usize;
        let m = w
            .con
            .motif_census(idx, w.csc(), examples)
            .ok_or(format!("нейрон {idx} вне диапазона (узлов: {})", w.con.n_nodes()))?;
        let report = json!({
            "neuron": idx,
            "root_id": Self::fly_root(&w, idx),
            "out_degree": m.out_degree,
            "in_degree": m.in_degree,
            "reciprocal_count": m.reciprocal.len(),
            "reciprocal": m.reciprocal.iter().map(|(fwd, bwd)| json!({
                "partner": fwd.target,
                "partner_root_id": Self::fly_root(&w, fwd.target as usize),
                "u_to_partner": {"weight": fwd.weight, "nt": fwd.nt_name(), "sign": fwd.sign()},
                "partner_to_u": {"weight": bwd.weight, "nt": bwd.nt_name(), "sign": bwd.sign()},
            })).collect::<Vec<_>>(),
            "feedforward": m.feedforward,
            "feedback3": m.feedback3,
            "ff_examples": m.ff_examples.iter().map(|&(v, t)| json!({
                "mid": v, "mid_root_id": Self::fly_root(&w, v),
                "target": t, "target_root_id": Self::fly_root(&w, t),
            })).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
    }

    /// poler_fly_propagate: симуляция распространения сигнала —
    /// x(t+1) = leak·x(t) + γ·A·x(t) на знаковых весах: возбуждение
    /// разгоняет, торможение гасит. Муха «думает» прямо в RAM.
    fn tool_fly_propagate(&self, args: &Value) -> Result<String, String> {
        let csr = args.get("csr").and_then(|v| v.as_str());
        let nodes = args.get("nodes").and_then(|v| v.as_str());
        let w = self.fly_state(csr, nodes)?;
        let seeds = Self::fly_neurons_arg(args, "neurons", &w, 64)?;
        let steps = args
            .get("steps")
            .and_then(|v| v.as_u64())
            .unwrap_or(4)
            .clamp(1, 64) as usize;
        let gamma = args
            .get("gamma")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.05)
            .clamp(0.0, 10.0);
        let leak = args
            .get("leak")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.8)
            .clamp(0.0, 1.0);
        let theta = args
            .get("theta")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.01);
        let top = args
            .get("top")
            .and_then(|v| v.as_u64())
            .unwrap_or(15)
            .clamp(1, 200) as usize;
        let filter = Self::fly_sign_arg(args)?;
        let r = w
            .con
            .propagate(&seeds, steps, gamma, leak, filter, top, theta)?;
        let round6 = |x: f64| (x * 1e6).round() / 1e6;
        let report = json!({
            "seeds": r.seeds.iter().map(|&s| json!({
                "neuron": s, "root_id": Self::fly_root(&w, s),
            })).collect::<Vec<_>>(),
            "params": {
                "steps": steps, "gamma": gamma, "leak": leak,
                "sign_filter": args.get("sign").and_then(|v| v.as_str()).unwrap_or("all"),
                "theta": theta,
            },
            "timeline": r.steps.iter().map(|s| json!({
                "step": s.step,
                "active": s.active,
                "positive_mass": round6(s.positive_mass),
                "negative_mass": round6(s.negative_mass),
            })).collect::<Vec<_>>(),
            "top": r.top.iter().map(|&(v, x)| json!({
                "neuron": v, "root_id": Self::fly_root(&w, v), "potential": round6(x),
            })).collect::<Vec<_>>(),
        });
        Ok(serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into()))
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
    let mut tools = vec![
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
    ];

    // E1/v0.31.0 + E2/v0.32.0: идеальный исполнитель команд (ядро Zig,
    // raw-syscalls). Требует фичу pnd-ffi (libpoler_core.a).
    #[cfg(feature = "pnd-ffi")]
    tools.push(json!({
        "name": "poler_exec",
        "description": "ИДЕАЛЬНЫЙ ИСПОЛНИТЕЛЬ КОМАНД (ядро Zig, raw-syscall слой): запустить \
программу с жёстким таймаутом (SIGTERM → grace → SIGKILL группе), лимитом захвата вывода \
(O(1) памяти) и гарантией отсутствия зомби (pidfd + wait4 в ppoll-цикле). Рождён диагностикой \
GNU bash 5.2: классы bash-ошибок исключены конструктивно. argv передаётся массивом — шелл-инъекции \
невозможны. E2: capture=head_tail (голова B/2 + маркер dropped N + хвост B/2 — умолчание), \
cwd (рабочий каталог), env (явное окружение), pty (настоящий терминал 200x50 для sudo/fzf/htop), \
PATH разрешает сам ребёнок (ноль stat в родителе). Возвращает JSON: exit_code, signal, timed_out, \
cancelled, truncated, pty, stdout, stderr, duration_us, pid.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Программа: имя из PATH или путь с '/'"},
                "args": {"type": "array", "items": {"type": "string"}, "description": "Аргументы (массив, БЕЗ шелл-парсинга)"},
                "timeout_ms": {"type": "integer", "default": 30000, "minimum": 1},
                "grace_ms": {"type": "integer", "default": 100, "minimum": 0},
                "max_out_bytes": {"type": "integer", "default": 65536, "description": "Лимит захвата НА ПОТОК"},
                "stdin": {"type": "string", "description": "Данные в stdin (опционально)"},
                "cwd": {"type": "string", "description": "Рабочий каталог ребёнка (chdir в бутстрапе)"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "Явное окружение (иначе наследуется)"},
                "pty": {"type": "boolean", "default": false, "description": "Псевдотерминал 200x50; stdout/stderr слиты"},
                "capture": {"type": "string", "enum": ["tail", "head_tail"], "default": "head_tail", "description": "Режим при переполнении: хвост | голова+маркер+хвост"}
            },
            "required": ["command"]
        }
    }));

    #[cfg(feature = "pnd-ffi")]
    tools.push(json!({
        "name": "poler_exec_async",
        "description": "ФОНОВЫЙ запуск команды (E2): возвращает task_id мгновенно, команда живёт \
в собственном потоке. Опрос результата — poler_exec_task, отмена — poler_exec_kill, обзор — \
poler_exec_list. Аргументы совпадают с poler_exec (command/args/timeout_ms/…/cwd/env/pty/capture). \
Для долгих сборок/сканов: запустил — и продолжил работу.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "command": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}},
                "timeout_ms": {"type": "integer", "default": 30000},
                "grace_ms": {"type": "integer", "default": 100},
                "max_out_bytes": {"type": "integer", "default": 65536},
                "stdin": {"type": "string"},
                "cwd": {"type": "string"},
                "env": {"type": "object", "additionalProperties": {"type": "string"}},
                "pty": {"type": "boolean", "default": false},
                "capture": {"type": "string", "enum": ["tail", "head_tail"], "default": "head_tail"}
            },
            "required": ["command"]
        }
    }));

    #[cfg(feature = "pnd-ffi")]
    tools.push(json!({
        "name": "poler_exec_task",
        "description": "ОПРОС/ОЖИДАНИЕ фоновой задачи poler_exec_async: task_id + необязательный \
wait_ms (сколько миллисекунд ждать завершения, до 60000). Отчёт: task_id, status (running|done), \
command, elapsed_us и — при done — полный JSON исполнения (exit_code, stdout, stderr, cancelled, …).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": {"type": "integer", "description": "id из poler_exec_async"},
                "wait_ms": {"type": "integer", "default": 0, "maximum": 60000}
            },
            "required": ["task_id"]
        }
    }));

    #[cfg(feature = "pnd-ffi")]
    tools.push(json!({
        "name": "poler_exec_kill",
        "description": "ОТМЕНА фоновой задачи: атомарный флаг → SIGTERM → grace → SIGKILL группе \
(ядро замечает отмену за ≤25 мс, зомби невозможны). Итог — через poler_exec_task \
(cancelled: true).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "task_id": {"type": "integer"}
            },
            "required": ["task_id"]
        }
    }));

    #[cfg(feature = "pnd-ffi")]
    tools.push(json!({
        "name": "poler_exec_list",
        "description": "ОБЗОР фоновых задач реестра: живые (running, elapsed_us) и завершённые \
(done, exit_code/cancelled/timed_out) — до 128 последних.",
        "inputSchema": {
            "type": "object",
            "properties": {},
            "required": []
        }
    }));

    // C2/v0.33.0 «Живая муха»: коннектом FLYCSR1 (мозг мухи FlyWire v783,
    // 138 639 нейронов) как резидентная матрица A. Артефакт грузится в RAM
    // ОДИН раз (csr в первом вызове), дальше всё семейство работает без
    // диска; тяжёлые запросы исполняются в пуле воркеров параллельно.
    tools.push(json!({
        "name": "poler_fly",
        "description": "ЖИВАЯ МУХА — загрузка/сводка/выгрузка коннектома FLYCSR1 (мозг мухи \
FlyWire v783: 138 639 нейронов, матрица A со знаками: ach/glut +1 возбуждающие, gaba −1 \
тормозные, oct/ser/da модуляторные). Первый вызов: csr = путь к .csr.zst (+ опционально \
nodes = nodes.bin для перевода индекс ↔ root_id FlyWire) — артефакт грузится в RAM и живёт \
между вызовами (тёплый: повторная сводка без диска). action: summary (умолчание) — сводка \
(узлы/рёбра/масса/баланс медиаторов), eject — выгрузить из RAM. Дальше вызывай \
poler_fly_node/edge/khop/path/common/centrality/rotor/motifs/propagate БЕЗ csr.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string", "description": "Путь к .csr.zst (первый вызов; далее опционален)"},
                "nodes": {"type": "string", "description": "Путь к nodes.bin (root_id FlyWire, опционально)"},
                "action": {"type": "string", "enum": ["summary", "eject"], "default": "summary"}
            },
            "required": []
        }
    }));

    tools.push(json!({
        "name": "poler_fly_node",
        "description": "ПАСПОРТ НЕЙРОНА мухи: исходящие/входящие степени и синаптические массы, \
топ партнёров обоих направлений (other, root_id, weight, nt, sign, signed_weight). neuron = \
индекс CSR или root_id (строкой, если загружена nodes). limit усекает топы (умолчание 8).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "neuron": {"type": ["string", "integer"], "description": "Индекс 0..138638 либо root_id FlyWire"},
                "limit": {"type": "integer", "default": 8, "maximum": 500}
            },
            "required": ["neuron"]
        }
    }));

    tools.push(json!({
        "name": "poler_fly_edge",
        "description": "РЕБРО u→v и РОТОР пары J = A − Aᵀ: циркуляция влияния. Однонаправленное \
ребро — чистый поток (J[u][v] = +w, J[v][u] = −w), реципрокная симметричная пара гасится в 0. \
forward/backward — знаковые веса обоих направлений.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "u": {"type": ["string", "integer"]},
                "v": {"type": ["string", "integer"]}
            },
            "required": ["u", "v"]
        }
    }));

    tools.push(json!({
        "name": "poler_fly_khop",
        "description": "K-HOP BFS потока сигнала от нейрона: размеры фронтов по хопам + всего \
достигнуто (signal expansion). sign: all | exc (только возбуждающие) | inh (только тормозные).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "neuron": {"type": ["string", "integer"]},
                "depth": {"type": "integer", "default": 2, "minimum": 1, "maximum": 10},
                "sign": {"type": "string", "enum": ["all", "exc", "inh"], "default": "all"}
            },
            "required": ["neuron"]
        }
    }));

    tools.push(json!({
        "name": "poler_fly_path",
        "description": "КРАТЧАЙШИЙ ПУТЬ СИГНАЛА from→to (BFS): цепочка прыжков — каждый синапс \
с весом, медиатором и знаком; total_weight — суммарная масса маршрута. Найди маршрут между \
любыми двумя нейронами мозга за миллисекунды.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "from": {"type": ["string", "integer"]},
                "to": {"type": ["string", "integer"]},
                "sign": {"type": "string", "enum": ["all", "exc", "inh"], "default": "all"}
            },
            "required": ["from", "to"]
        }
    }));

    tools.push(json!({
        "name": "poler_fly_common",
        "description": "ОБЩИЕ ПАРТНЁРЫ набора нейронов (2..32): direction=down — общие мишени \
(на кого влияет весь набор), up — общие источники (кто влияет на весь набор). Конвергенция и \
дивергенция схем мозга; у каждого партнёра — суммарная масса и рёбра от каждого нейрона набора.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "neurons": {"type": "array", "items": {"type": ["string", "integer"]}, "minItems": 1, "maxItems": 32},
                "direction": {"type": "string", "enum": ["down", "up"], "default": "down"},
                "sign": {"type": "string", "enum": ["all", "exc", "inh"], "default": "all"},
                "limit": {"type": "integer", "default": 20, "maximum": 500}
            },
            "required": ["neurons"]
        }
    }));

    tools.push(json!({
        "name": "poler_fly_centrality",
        "description": "ХАБЫ МОЗГА: топ нейронов по исходящей/входящей степени + взвешенный \
PageRank по |A| (модуляторы не проводят ранг; sign сужает граф до возбуждающих/тормозных путей). \
iterations/converged — диагностика сходимости.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "top": {"type": "integer", "default": 10, "maximum": 100},
                "sign": {"type": "string", "enum": ["all", "exc", "inh"], "default": "all"},
                "damping": {"type": "number", "default": 0.85, "minimum": 0.05, "maximum": 0.99},
                "iterations": {"type": "integer", "default": 30, "maximum": 200}
            },
            "required": []
        }
    }));

    tools.push(json!({
        "name": "poler_fly_rotor",
        "description": "ГЛОБАЛЬНЫЙ ТОП ЦИРКУЛЯЦИИ J = A − Aᵀ: самые однонаправленные влияния \
мозга — чистые потоки без встречной компенсации (J_uv > 0, J_vu = −J_uv). min_abs отсекает \
слабые пары; forward/backback_signed — компоненты потока.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "top": {"type": "integer", "default": 10, "maximum": 1000},
                "min_abs": {"type": "integer", "default": 1, "minimum": 1}
            },
            "required": []
        }
    }));

    tools.push(json!({
        "name": "poler_fly_motifs",
        "description": "МОТИВЫ ВОКРУГ НЕЙРОНА: реципрокные пары u⇄v (оба ребра с весами), \
feedforward-треугольники u→v→w + u→w (схемы усиления), feedback-циклы u→v→w→u (кольца \
обратной связи). examples ограничивает список примеров (счётчики всегда полные).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "neuron": {"type": ["string", "integer"]},
                "examples": {"type": "integer", "default": 5, "maximum": 50}
            },
            "required": ["neuron"]
        }
    }));

    tools.push(json!({
        "name": "poler_fly_propagate",
        "description": "СИМУЛЯЦИЯ РАСПРОСТРАНЕНИЯ СИГНАЛА — муха думает в RAM: \
x(t+1) = leak·x(t) + γ·A·x(t) на знаковых весах (возбуждение разгоняет, торможение гасит, \
модуляторы молчат). neurons — семена (до 64), steps — шаги (до 64), gamma/leak — динамика, \
theta — порог активности. Отчёт: хронология (active, ±масса) + топ возбуждённых нейронов.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "csr": {"type": "string"},
                "nodes": {"type": "string"},
                "neurons": {"type": "array", "items": {"type": ["string", "integer"]}, "minItems": 1, "maxItems": 64},
                "steps": {"type": "integer", "default": 4, "maximum": 64},
                "gamma": {"type": "number", "default": 0.05},
                "leak": {"type": "number", "default": 0.8},
                "theta": {"type": "number", "default": 0.01},
                "top": {"type": "integer", "default": 15, "maximum": 200},
                "sign": {"type": "string", "enum": ["all", "exc", "inh"], "default": "all"}
            },
            "required": ["neurons"]
        }
    }));

    tools
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

#[cfg(all(test, feature = "pnd-ffi"))]
mod exec_family_tests {
    use super::*;

    fn server() -> McpServer {
        McpServer::new(9222, 10, PathBuf::from("/nonexistent-poler-exec-test.db"))
    }

    fn call(srv: &McpServer, name: &str, arguments: Value) -> Value {
        srv.dispatch(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .expect("dispatch не падает")
    }

    fn text_of(resp: &Value) -> String {
        resp.pointer("/result/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string()
    }

    #[test]
    fn manifest_contains_exec_family() {
        let m = tools_manifest();
        let names: Vec<&str> =
            m.iter().filter_map(|t| t.get("name").and_then(|v| v.as_str())).collect();
        for expected in
            ["poler_exec", "poler_exec_async", "poler_exec_task", "poler_exec_kill", "poler_exec_list"]
        {
            assert!(names.contains(&expected), "нет {expected}: {names:?}");
        }
    }

    #[test]
    fn is_exec_tool_routes_exec_family_only() {
        assert!(is_exec_tool(&json!({
            "method": "tools/call", "params": {"name": "poler_exec", "arguments": {}}
        })));
        assert!(is_exec_tool(&json!({
            "method": "tools/call", "params": {"name": "poler_exec_kill", "arguments": {}}
        })));
        assert!(!is_exec_tool(&json!({
            "method": "tools/call", "params": {"name": "poler_grep", "arguments": {}}
        })));
        assert!(!is_exec_tool(&json!({"method": "tools/list"})));
    }

    #[test]
    fn tool_exec_head_tail_cwd_env() {
        let srv = server();
        let resp = call(
            &srv,
            "poler_exec",
            json!({
                "command": "sh",
                "args": ["-c", "printf HEAD7; dd if=/dev/zero bs=1024 count=64 2>/dev/null; printf TAIL7"],
                "cwd": "/tmp",
                "env": {"PATH": "/bin:/usr/bin"},
                "max_out_bytes": 1024,
                "timeout_ms": 10_000
            }),
        );
        let text = text_of(&resp);
        let report: Value = serde_json::from_str(&text)
            .unwrap_or_else(|e| panic!("не JSON ({e}): {text}"));
        assert_eq!(report["exit_code"], json!(0));
        assert_eq!(report["truncated"], json!(true));
        let stdout = report["stdout"].as_str().unwrap();
        assert!(stdout.starts_with("HEAD7"), "голова потеряна: {stdout:?}");
        assert!(stdout.ends_with("TAIL7"), "хвост потерян: {stdout:?}");
        assert!(stdout.contains("dropped"), "нет маркера: {stdout:?}");
    }

    #[test]
    fn tool_exec_rejects_unknown_capture() {
        let srv = server();
        let resp = call(
            &srv,
            "poler_exec",
            json!({"command": "echo", "args": ["x"], "capture": "sideways"}),
        );
        // ошибка аргумента → isError: true, но протокол отвечает 200-образно
        assert!(
            resp.pointer("/result/isError").and_then(|v| v.as_bool()).unwrap_or(false),
            "ожидался isError: {resp}"
        );
    }

    #[test]
    fn async_lifecycle_spawn_poll_kill() {
        let srv = server();
        // 1) фоновый запуск долгой задачи
        let resp = call(
            &srv,
            "poler_exec_async",
            json!({"command": "sleep", "args": ["30"], "timeout_ms": 60_000}),
        );
        let text = text_of(&resp);
        let report: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("не JSON ({e}): {text}"));
        let id = report["task_id"].as_u64().expect("task_id в отчёте");
        assert_eq!(report["status"], json!("running"));

        // 2) мгновенный опрос — running
        let resp = call(&srv, "poler_exec_task", json!({"task_id": id}));
        assert!(text_of(&resp).contains("running"), "ожидался running");

        // 3) kill → killed: true
        let resp = call(&srv, "poler_exec_kill", json!({"task_id": id}));
        assert!(text_of(&resp).contains("\"killed\": true"), "ожидался killed: true");

        // 4) ожидание завершения с cancelled: true
        let resp = call(&srv, "poler_exec_task", json!({"task_id": id, "wait_ms": 5000}));
        let text = text_of(&resp);
        let report: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("не JSON ({e}): {text}"));
        assert_eq!(report["status"], json!("done"), "задача не завершилась: {text}");
        assert_eq!(report["cancelled"], json!(true), "ожидалась отмена: {text}");
        assert_eq!(report["timed_out"], json!(false), "отмена ≠ таймаут");

        // 5) повторный kill по завершённой — killed: false
        let resp = call(&srv, "poler_exec_kill", json!({"task_id": id}));
        assert!(text_of(&resp).contains("\"killed\": false"));

        // 6) list видит задачу
        let resp = call(&srv, "poler_exec_list", json!({}));
        let text = text_of(&resp);
        let report: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("не JSON ({e}): {text}"));
        let listed = report["tasks"].as_array().unwrap();
        assert!(listed.iter().any(|t| t["task_id"] == json!(id)), "list не видит задачу");

        // 7) несуществующая задача — ошибка протокола (isError)
        let resp = call(&srv, "poler_exec_kill", json!({"task_id": 999_999}));
        assert!(
            resp.pointer("/result/isError").and_then(|v| v.as_bool()).unwrap_or(false),
            "ожидался isError для неизвестной задачи"
        );
    }

    #[test]
    fn async_completes_normally() {
        let srv = server();
        let resp = call(
            &srv,
            "poler_exec_async",
            json!({"command": "echo", "args": ["полер-фон"], "timeout_ms": 10_000}),
        );
        let report: Value = serde_json::from_str(&text_of(&resp)).expect("JSON");
        let id = report["task_id"].as_u64().unwrap();

        let resp = call(&srv, "poler_exec_task", json!({"task_id": id, "wait_ms": 10_000}));
        let text = text_of(&resp);
        let report: Value =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("не JSON ({e}): {text}"));
        assert_eq!(report["status"], json!("done"), "{text}");
        assert_eq!(report["exit_code"], json!(0), "{text}");
        assert!(report["stdout"].as_str().unwrap().contains("полер-фон"), "{text}");
    }
}

#[cfg(test)]
mod fly_family_tests {
    use super::*;

    fn server() -> McpServer {
        McpServer::new(9222, 10, PathBuf::from("/nonexistent-poler-fly-test.db"))
    }

    fn call(srv: &McpServer, name: &str, arguments: Value) -> Value {
        srv.dispatch(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }))
        .expect("dispatch не падает")
    }

    fn text_of(resp: &Value) -> String {
        resp.pointer("/result/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or_default()
            .to_string()
    }

    fn json_of(resp: &Value) -> Value {
        let text = text_of(resp);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("не JSON ({e}): {text}"))
    }

    fn core_csr() -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("docs/flywire-connectome/flywire_v783_core.csr.zst");
        assert!(p.exists(), "артефакт {} не найден", p.display());
        p.display().to_string()
    }

    #[test]
    fn manifest_contains_fly_family() {
        let m = tools_manifest();
        let names: Vec<&str> =
            m.iter().filter_map(|t| t.get("name").and_then(|v| v.as_str())).collect();
        for expected in [
            "poler_fly",
            "poler_fly_node",
            "poler_fly_edge",
            "poler_fly_khop",
            "poler_fly_path",
            "poler_fly_common",
            "poler_fly_centrality",
            "poler_fly_rotor",
            "poler_fly_motifs",
            "poler_fly_propagate",
        ] {
            assert!(names.contains(&expected), "нет {expected}: {names:?}");
        }
    }

    #[test]
    fn is_fly_tool_routes_family_only() {
        for name in ["poler_fly", "poler_fly_node", "poler_fly_propagate"] {
            assert!(is_fly_tool(&json!({
                "method": "tools/call", "params": {"name": name, "arguments": {}}
            })));
        }
        assert!(!is_fly_tool(&json!({
            "method": "tools/call", "params": {"name": "poler_grep", "arguments": {}}
        })));
        assert!(!is_fly_tool(&json!({"method": "tools/list"})));
    }

    // Полный жизненный цикл теплоты: холодная загрузка → тёплый вызов
    // без csr → eject → отказ без csr.
    #[test]
    fn fly_warm_lifecycle() {
        let srv = server();
        // 1) холодная загрузка + сводка (золотые числа C1)
        let r = json_of(&call(&srv, "poler_fly", json!({"csr": core_csr()})));
        assert_eq!(r["n_nodes"], json!(138_639));
        assert_eq!(r["n_edges"], json!(2_700_513));
        assert_eq!(r["total_mass"], json!(34_153_566));
        assert_eq!(r["core"], json!(true));
        assert!(r["load_ms"].as_u64().unwrap() > 0, "load_ms: {r}");
        // 2) тёплый вызов без csr — муха уже в RAM
        let r2 = json_of(&call(&srv, "poler_fly", json!({})));
        assert_eq!(r2["n_nodes"], json!(138_639));
        assert_eq!(r2["warm"]["csc_built"], json!(false), "сводка не строит CSC");
        assert_eq!(r2["warm"]["has_nodes"], json!(false));
        // 3) eject
        let r3 = json_of(&call(&srv, "poler_fly", json!({"action": "eject"})));
        assert_eq!(r3["unloaded"], json!(true));
        let r4 = json_of(&call(&srv, "poler_fly", json!({"action": "eject"})));
        assert_eq!(r4["unloaded"], json!(false), "повторный eject — пусто");
        // 4) после eject без csr — протокольная ошибка
        let resp = call(&srv, "poler_fly_node", json!({"neuron": 0}));
        assert!(
            resp.pointer("/result/isError").and_then(|v| v.as_bool()).unwrap_or(false),
            "ожидался isError без загруженного коннектома: {resp}"
        );
        assert!(text_of(&resp).contains("коннектом не загружен"));
    }

    // Паспорт/ребро/K-hop на золотых числах узла 0.
    #[test]
    fn fly_node_edge_khop_golden() {
        let srv = server();
        let csr = core_csr();
        let r = json_of(&call(&srv, "poler_fly_node", json!({"csr": csr, "neuron": 0})));
        assert_eq!(r["out_degree"], json!(13));
        assert_eq!(r["in_degree"], json!(13));
        let top_in = r["top_in"].as_array().unwrap();
        assert!(!top_in.is_empty());
        assert_eq!(top_in[0]["other"], json!(79_529));
        assert_eq!(top_in[0]["weight"], json!(17));
        assert_eq!(top_in[0]["nt"], json!("gaba"));
        assert_eq!(top_in[0]["sign"], json!(-1));
        // root_id без таблицы узлов — null (честно)
        assert_eq!(top_in[0]["other_root_id"], json!(null));
        // ребро 0→6135 + ротор
        let e = json_of(&call(&srv, "poler_fly_edge", json!({"u": 0, "v": 6135})));
        assert_eq!(e["found"], json!(true));
        assert_eq!(e["forward"]["weight"], json!(5));
        assert_eq!(e["forward"]["nt"], json!("ach"));
        assert_eq!(e["backward"], json!(null));
        assert_eq!(e["rotor_Juv"], json!(5));
        assert_eq!(e["rotor_Jvu"], json!(-5));
        // K-hop: фронты [13, 443], достигнуто 457
        let k = json_of(&call(&srv, "poler_fly_khop", json!({"neuron": 0, "depth": 2})));
        assert_eq!(k["frontier_sizes"], json!([13, 443]));
        assert_eq!(k["visited"], json!(457));
        // фильтр возбуждающих сужает поток
        let k2 = json_of(&call(&srv, "poler_fly_khop", json!({"neuron": 0, "depth": 2, "sign": "exc"})));
        let exc_visited = k2["visited"].as_u64().unwrap();
        assert!(exc_visited <= 457, "exc-поток не шире полного: {exc_visited}");
    }

    // Путь/общие партнёры/ротор-топ/центральность на реальном ядре.
    #[test]
    fn fly_path_common_rotor_centrality() {
        let srv = server();
        let csr = core_csr();
        // прямой путь 0→6135 (ребро w5 ach)
        let p = json_of(&call(&srv, "poler_fly_path", json!({"csr": csr, "from": 0, "to": 6135})));
        assert_eq!(p["found"], json!(true));
        assert_eq!(p["length"], json!(1));
        assert_eq!(p["total_weight"], json!(5));
        let hops = p["hops"].as_array().unwrap();
        assert_eq!(hops.len(), 1);
        assert_eq!(hops[0]["from"], json!(0));
        assert_eq!(hops[0]["to"], json!(6135));
        assert_eq!(hops[0]["nt"], json!("ach"));
        // пустой путь 0→0
        let p0 = json_of(&call(&srv, "poler_fly_path", json!({"from": 0, "to": 0})));
        assert_eq!(p0["found"], json!(true));
        assert_eq!(p0["length"], json!(0));
        // общие мишени соседей 0 и 6135 — обязаны быть (кольца мозга)
        let c = json_of(&call(&srv, "poler_fly_common", json!({"neurons": [0, 6135]})));
        assert!(
            c["count"].as_u64().unwrap() > 0,
            "у соседей обязаны быть общие мишени: {c}"
        );
        let partners = c["partners"].as_array().unwrap();
        assert!(!partners.is_empty());
        assert!(partners[0]["total_weight"].as_u64().unwrap() > 0);
        // топ ротора: антисимметрия + убывание
        let rot = json_of(&call(&srv, "poler_fly_rotor", json!({"top": 5, "min_abs": 10})));
        assert_eq!(rot["count"], json!(5));
        let pairs = rot["pairs"].as_array().unwrap();
        let j0 = pairs[0]["J_uv"].as_i64().unwrap();
        assert!(j0 >= 10, "топ-циркуляция < порога: {j0}");
        assert_eq!(pairs[0]["J_vu"].as_i64().unwrap(), -j0);
        for w in pairs.windows(2) {
            assert!(
                w[0]["J_uv"].as_i64().unwrap() >= w[1]["J_uv"].as_i64().unwrap(),
                "ротор не убывает: {w:?}"
            );
        }
        // центральность: хабы + PageRank
        let cen = json_of(&call(&srv, "poler_fly_centrality", json!({"top": 5})));
        assert_eq!(cen["top_out_degree"].as_array().unwrap().len(), 5);
        assert_eq!(cen["top_in_degree"].as_array().unwrap().len(), 5);
        let pr = cen["pagerank"]["top"].as_array().unwrap();
        assert_eq!(pr.len(), 5);
        for w in pr.windows(2) {
            assert!(
                w[0]["rank"].as_f64().unwrap() >= w[1]["rank"].as_f64().unwrap(),
                "ранг не убывает: {w:?}"
            );
        }
        assert!(pr.iter().all(|x| x["rank"].as_f64().unwrap() > 0.0));
    }

    // Мотивы и симуляция на реальном ядре.
    #[test]
    fn fly_motifs_propagate() {
        let srv = server();
        let csr = core_csr();
        let m = json_of(&call(&srv, "poler_fly_motifs", json!({"csr": csr, "neuron": 0})));
        assert_eq!(m["out_degree"], json!(13));
        assert_eq!(m["in_degree"], json!(13));
        assert!(m["reciprocal_count"].is_u64());
        assert!(m["feedforward"].is_u64());
        assert!(m["feedback3"].is_u64());
        // симуляция от узла 0: сигнал расходится по 13 исходящим
        let p = json_of(&call(
            &srv,
            "poler_fly_propagate",
            json!({"neurons": [0], "steps": 3, "gamma": 0.05, "leak": 0.8}),
        ));
        let tl = p["timeline"].as_array().unwrap();
        assert_eq!(tl.len(), 3);
        assert_eq!(tl[0]["step"], json!(1));
        assert!(
            tl[0]["active"].as_u64().unwrap() >= 2,
            "сигнал не вышел за семя: {p}"
        );
        assert!(tl[0]["positive_mass"].as_f64().unwrap() > 0.0);
        assert!(p["top"].as_array().unwrap().len() >= 2);
        // неверный нейрон — протокольная ошибка
        let resp = call(&srv, "poler_fly_propagate", json!({"neurons": [999_999_999]}));
        assert!(
            resp.pointer("/result/isError").and_then(|v| v.as_bool()).unwrap_or(false),
            "ожидался isError для нейрона вне диапазона"
        );
        // отдельный сервер без загрузки — отказ до валидации аргументов
        let srv2 = server();
        let resp = call(&srv2, "poler_fly_propagate", json!({"neurons": [0]}));
        assert!(text_of(&resp).contains("коннектом не загружен"));
    }
}
