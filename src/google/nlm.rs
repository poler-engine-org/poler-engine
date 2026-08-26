//! NotebookLM (Gemini Notebook) — нативный RPC-клиент `batchexecute`.
//!
//! Протокол извлечён из расширения **NLMTools.com «NotebookLM Tools»**
//! (публичный XPI с Mozilla Addons / Chrome Web Store). У NLMTools нет
//! своего REST-API — их «специальная интеграция» ходит во **внутренний
//! RPC NotebookLM** (`/_/{app}/data/batchexecute`) прямо из твоей
//! авторизованной вкладки: общение, все доки, метаданные источников.
//!
//! poler-engine повторяет тот же канал из **персистентного профиля**
//! (`--google-browse https://notebook.google.com/` — логин один раз
//! руками, куки живут месяцами):
//!
//! * пароль никогда не попадает в движок;
//! * те же RPC-методы, что у расширения (`wXbhsf` список ноутбуков,
//!   `hizoJc` контент источника, `cFji9` заметки, `gArtLc` Studio-объекты);
//! * **медиа** — то, чего нет у NLMTools-API: URL картинок слайдов
//!   отдаются `LOAD_SOURCE`, качаются тем же профилем ([`fetch_media`]),
//!   плюс скриншот страницы глазами юзера ([`screenshot`]);
//! * чат с ноутбуком — честной UI-автоматизацией ([`NlmSession::chat`]):
//!   ввод вопроса в поле чата и чтение появившегося ответа.
//!
//! RPC идентификаторы и схема ответов — собственность Google и меняются
//! без предупреждения; модуль держит их в одном месте ([RPC-константы])
//! и парсит ответы толерантно к отсутствующим полям.

use serde::Serialize;
use serde_json::Value;

use crate::web::cdp::CdpSession;

use super::{ensure_google_browser, google_cdp_port};

// ---------------------------------------------------------------------------
// RPC-методы NotebookLM (идентификаторы из NLMTools Tools 1.9.6)
// ---------------------------------------------------------------------------

/// Все ноутбуки аккаунта (LIST_RECENTLY_VIEWED_PROJECTS).
pub const RPC_LIST_NOTEBOOKS: &str = "wXbhsf";
/// Паспорт ноутбука (GET_PROJECT).
pub const RPC_GET_PROJECT: &str = "rLM1Ne";
/// Контент источника: текст или URL картинок слайдов (LOAD_SOURCE).
pub const RPC_LOAD_SOURCE: &str = "hizoJc";
/// Заметки ноутбука (GET_NOTES).
pub const RPC_GET_NOTES: &str = "cFji9";
/// Studio-объекты: аудио/отчёты/квизы/миндмэпы (LIST_ARTIFACTS).
pub const RPC_LIST_ARTIFACTS: &str = "gArtLc";
/// Аккаунт сессии (GET_OR_CREATE_ACCOUNT).
pub const RPC_ACCOUNT: &str = "ZwVcOc";

/// Запасной `bl` (сборка фронтенда), если `WIZ_global_data.cfb2h` пуст —
/// то же значение, что использует расширение NLMTools.
const BL_FALLBACK: &str = "boq_labs-tailwind-frontend_20250902.08_p1";
/// Запасной сегмент приложения, если `qwAQke` пуст.
const APP_FALLBACK: &str = "BardChatUi";

/// База NotebookLM: `POLER_NLM_BASE` (тесты/свои зеркала) → прод.
pub fn nlm_base() -> String {
    std::env::var("POLER_NLM_BASE").unwrap_or_else(|_| "https://notebook.google.com".to_string())
}

/// Таймаут чата, с (POLER_NLM_CHAT_TIMEOUT) [default: 90].
fn chat_timeout_s() -> u64 {
    std::env::var("POLER_NLM_CHAT_TIMEOUT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(90)
}

// ---------------------------------------------------------------------------
// Модели (схема ответов — из парсеров расширения NLMTools)
// ---------------------------------------------------------------------------

/// Источник ноутбука: док, ссылка, YouTube-транскрипт, слайды…
#[derive(Serialize, Clone, Debug)]
pub struct SourceMeta {
    pub id: String,
    pub title: String,
    /// Человекочитаемый тип («Google Docs», «YouTube», «Слайды»…).
    pub kind: String,
    pub url: Option<String>,
    pub youtube_id: Option<String>,
    pub author: Option<String>,
    pub drive_file_id: Option<String>,
    pub mime: Option<String>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
}

/// Ноутбук с полным списком источников.
#[derive(Serialize, Clone, Debug)]
pub struct Notebook {
    pub id: String,
    pub title: String,
    pub emoji: String,
    pub permission: Option<u64>,
    pub updated_at: Option<String>,
    pub created_at: Option<String>,
    pub sources: Vec<SourceMeta>,
}

/// Картинка из источника-слайдов (медиа-канал).
#[derive(Serialize, Clone, Debug)]
pub struct ImageRef {
    pub url: String,
    pub id: Option<String>,
}

/// Контент источника: текст ИЛИ картинки слайдов.
#[derive(Serialize, Clone, Debug)]
pub struct SourceContent {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub content: Option<String>,
    pub images: Vec<ImageRef>,
}

/// Studio-объект ноутбука (аудио-обзор, отчёт, квиз, миндмэп…).
#[derive(Serialize, Clone, Debug)]
pub struct Artifact {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub status: String,
    pub source_ids: Vec<String>,
}

// ---------------------------------------------------------------------------
// Парсеры batchexecute-ответов (перенос парсеров On из NLMTools на Rust)
// ---------------------------------------------------------------------------

/// Хождение по массиву-«кортежу» Google: v[i] или Null.
fn at(v: &Value, i: usize) -> &Value {
    v.get(i).unwrap_or(&Value::Null)
}

/// Строка по пути индексов (любой null/промах → None).
fn s_at(v: &Value, path: &[usize]) -> Option<String> {
    let mut cur = v;
    for &i in path {
        cur = at(cur, i);
        if cur.is_null() {
            return None;
        }
    }
    cur.as_str().map(String::from)
}

/// Число по пути индексов (зарезервировано под будущие парсеры меты).
#[allow(dead_code)]
fn n_at(v: &Value, path: &[usize]) -> Option<f64> {
    let mut cur = v;
    for &i in path {
        cur = at(cur, i);
        if cur.is_null() {
            return None;
        }
    }
    cur.as_f64()
}

/// Дата Google `[секунды, наносекунды]` → ISO-8601 (как R() в NLMTools).
fn sec_nanos_iso(v: &Value) -> Option<String> {
    let sec = at(v, 0).as_f64()?;
    if sec < 1.0 {
        return None;
    }
    let nanos = at(v, 1).as_f64().unwrap_or(0.0);
    let ms = (sec * 1000.0 + nanos / 1e6).round() as i64;
    // Григорианский диапазон Unix-эпохи без внешних крейтов:
    // дни → Y-M-D (гражданская формула), затем H:M:S.
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    let (hh, mm, ss) = (rem / 3_600_000, (rem % 3_600_000) / 60_000, (rem % 60_000) / 1000);
    Some(format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z"))
}

/// Дни с 1970-01-01 → (год, месяц, день). Алгоритм Говарда Хиннанта.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Метка типа источника по raw-коду enum `h` из NLMTools.
fn source_kind(raw: Option<&Value>) -> String {
    match raw.and_then(|v| v.as_i64()) {
        Some(1) => "Google Docs".into(),
        Some(2) => "Google Slides".into(),
        Some(3) => "PDF".into(),
        Some(4) => "Текст".into(),
        Some(5) => "Веб-страница".into(),
        Some(8) => "Markdown".into(),
        Some(9) => "YouTube".into(),
        Some(10) => "Медиа-файл".into(),
        Some(11) => "DOCX".into(),
        Some(13) => "Изображение".into(),
        Some(14) => "Google Drive".into(),
        Some(16) => "CSV".into(),
        Some(other) => format!("тип {other}"),
        None => "—".to_string(),
    }
}

/// Метка типа артефакта по enum `a` из NLMTools.
fn artifact_kind(raw: Option<&Value>) -> String {
    match raw.and_then(|v| v.as_i64()) {
        Some(1) => "Audio".into(),
        Some(2) => "Report".into(),
        Some(3) => "Video".into(),
        Some(4) => "Quiz/Flashcards".into(),
        Some(5) => "Mind Map".into(),
        Some(7) => "Infographic".into(),
        Some(8) => "Slide Deck".into(),
        Some(9) => "Data Table".into(),
        Some(10) => "Custom".into(),
        Some(101) => "Quiz".into(),
        Some(102) => "Flashcards".into(),
        Some(103) => "Note".into(),
        Some(other) => format!("тип {other}"),
        None => "—".to_string(),
    }
}

/// Статус артефакта по enum `j`: PROCESSING=1, PENDING=2, READY=3, FAILED=4.
fn artifact_status(raw: Option<&Value>) -> String {
    match raw.and_then(|v| v.as_i64()) {
        Some(1) => "processing",
        Some(2) => "pending",
        Some(3) => "ready",
        Some(4) => "failed",
        _ => "—",
    }
    .to_string()
}

/// Источник из строки `[key, title, metadata]`.
fn parse_source_meta(row: &Value) -> Option<SourceMeta> {
    let id = at(row, 0).as_str()?.to_string();
    let title = at(row, 1).as_str()?.to_string();
    if id.is_empty() || title.is_empty() {
        return None;
    }
    let m = at(row, 2);
    let type_raw = at(m, 4);
    Some(SourceMeta {
        id,
        title,
        kind: source_kind(Some(type_raw)),
        url: s_at(m, &[5, 0]).or_else(|| s_at(m, &[7, 0])),
        youtube_id: s_at(m, &[5, 1]),
        author: s_at(m, &[5, 2]),
        drive_file_id: s_at(m, &[0, 0]).or_else(|| s_at(m, &[9, 0])),
        mime: s_at(m, &[9, 2]),
        updated_at: sec_nanos_iso(at(m, 2)),
        created_at: sec_nanos_iso(at(m, 3)),
    })
}

/// Ноутбук из строки `[title, sources, id, emoji, _, metadata]`.
fn parse_notebook(row: &Value) -> Option<Notebook> {
    let id = at(row, 2).as_str()?.to_string();
    if id.is_empty() {
        return None;
    }
    let title_raw = at(row, 0).as_str().unwrap_or("");
    let meta = at(row, 5);
    Some(Notebook {
        id,
        title: if title_raw.is_empty() {
            "Untitled notebook".to_string()
        } else {
            title_raw.to_string()
        },
        emoji: at(row, 3).as_str().unwrap_or("").to_string(),
        permission: at(meta, 0).as_u64(),
        updated_at: sec_nanos_iso(at(meta, 5)),
        created_at: sec_nanos_iso(at(meta, 8)),
        sources: at(row, 1)
            .as_array()
            .map(|rows| rows.iter().filter_map(parse_source_meta).collect())
            .unwrap_or_default(),
    })
}

/// Ответ `LIST_RECENTLY_VIEWED_PROJECTS` → ноутбуки.
/// Данные приходят либо массивом, либо обёрткой `[массив]`.
pub fn parse_notebooks(data: &Value) -> Vec<Notebook> {
    let rows = if at(data, 0).is_array() {
        at(data, 0)
    } else {
        data
    };
    rows.as_array()
        .map(|rows| rows.iter().filter_map(parse_notebook).collect())
        .unwrap_or_default()
}

/// Артефакт из строки `[id, title, type, sourceIds, status, …]`.
fn parse_artifact(row: &Value) -> Option<Artifact> {
    let id = at(row, 0).as_str()?.to_string();
    if id.is_empty() {
        return None;
    }
    // sourceIds приходят вложенными массивами — собираем все строки.
    fn collect_strings(v: &Value, out: &mut Vec<String>) {
        if let Some(s) = v.as_str() {
            if !s.is_empty() {
                out.push(s.to_string());
            }
        } else if let Some(arr) = v.as_array() {
            for x in arr {
                collect_strings(x, out);
            }
        }
    }
    let mut source_ids = Vec::new();
    collect_strings(at(row, 3), &mut source_ids);
    Some(Artifact {
        id,
        title: at(row, 1).as_str().filter(|t| !t.is_empty()).unwrap_or("Untitled").to_string(),
        kind: artifact_kind(Some(at(row, 2))),
        status: artifact_status(Some(at(row, 4))),
        source_ids,
    })
}

/// Ответ `LIST_ARTIFACTS` → Studio-объекты.
pub fn parse_artifacts(data: &Value) -> Vec<Artifact> {
    let rows = if at(data, 0).is_array() {
        at(data, 0)
    } else {
        data
    };
    rows.as_array()
        .map(|rows| rows.iter().filter_map(parse_artifact).collect())
        .unwrap_or_default()
}

/// Ответ `LOAD_SOURCE` → контент источника.
///
/// Схема (из parseSourceContent NLMTools): `t[0][1]` — заголовок,
/// `t[0][0][0]` — id, `t[0][2][4]` — тип, `t[3]` — блоки контента.
/// Текст: `t[3][0][0]` → блоки → `s[2][0]` → куски `E[2][0]`.
/// Слайды: `t[3][0][0]` → блоки → `l[5][0]` — URL картинки, `l[5][2]` — id.
pub fn parse_source_content(data: &Value) -> Option<SourceContent> {
    let head = at(data, 0);
    if head.is_null() {
        return None;
    }
    let id = s_at(head, &[0, 0]).unwrap_or_default();
    let title = at(head, 1).as_str().filter(|t| !t.is_empty()).unwrap_or("Untitled Source").to_string();
    let type_raw = at(at(head, 2), 4);
    let blocks = at(data, 3);
    // блоки контента: data[3][0][0] (extractTextContent/extractSlideImages
    // NLMTools читают t[0][0] от t = data[3])
    let block_rows = at(at(blocks, 0), 0).as_array();

    // ── текстовый контент ──
    let mut text = String::new();
    if let Some(rows) = block_rows {
        for s in rows {
            // куски: s[2][0] → массив; каждый кусок E → строка E[2][0]
            if let Some(chunks) = at(at(s, 2), 0).as_array() {
                let mut line = String::new();
                for e in chunks {
                    if let Some(t) = at(at(e, 2), 0).as_str() {
                        line.push_str(t);
                    }
                }
                if !line.is_empty() {
                    if !text.is_empty() && !text.ends_with('\n') && !line.starts_with('\n') {
                        text.push('\n');
                    }
                    text.push_str(&line);
                }
            }
        }
    }

    // ── картинки слайдов (медиа) ──
    let mut images = Vec::new();
    if let Some(rows) = block_rows {
        for l in rows {
            let img = at(l, 5);
            if let Some(url) = at(img, 0).as_str() {
                if url.starts_with("https://") || url.starts_with("http://") {
                    images.push(ImageRef {
                        url: url.to_string(),
                        id: at(img, 2).as_str().map(String::from),
                    });
                }
            }
        }
    }

    let kind = source_kind(Some(type_raw));
    Some(SourceContent {
        id,
        title,
        kind: kind.clone(),
        content: if text.is_empty() { None } else { Some(text) },
        images,
    })
}

/// Тело batchexecute → данные RPC.
///
/// Формат: первая строка `)]}'`, дальше строки JSON; перваяparsable-строка —
/// массив конвертов `[[rpcid, …, payload, …, [errcode,…]]]`. Payload —
/// JSON-строка на позиции `[0][2]` (реже `[0][1]`); код ошибки — `[0][5][0]`
/// (0/null — успех; 8 — квота; 7/16 — авторизация).
pub fn parse_rpc_body(body: &str) -> Result<Value, String> {
    let mut envelope: Option<Value> = None;
    for line in body.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            if v.as_array().is_some_and(|a| !a.is_empty()) {
                envelope = Some(v);
                break;
            }
        }
    }
    let env = envelope.ok_or("batchexecute: нет JSON-строки в ответе")?;
    let u = at(&env, 0);
    if !u.is_array() {
        return Err("batchexecute: конверт не массив".to_string());
    }
    // код ошибки: [0][5][0] (строковые значения — не ошибка)
    if let Some(code) = at(u, 5).get(0).and_then(|c| c.as_i64()) {
        if code != 0 {
            return Err(match code {
                8 => "квота NotebookLM исчерпана (дневной лимит генерации)".to_string(),
                7 | 16 => "сессия не авторизована — обнови логин: \
                           poler-engine --google-browse https://notebook.google.com/"
                    .to_string(),
                other => format!("RPC-ошибка NotebookLM: код {other}"),
            });
        }
    }
    // payload: [0][2] как JSON-строка (формат расширения), иначе [0][1]
    for idx in [2usize, 1] {
        if let Some(payload) = at(u, idx).as_str() {
            if !payload.is_empty() && (payload.starts_with('[') || payload.starts_with('{')) {
                return serde_json::from_str(payload)
                    .map_err(|e| format!("payload не JSON: {e}"));
            }
        }
    }
    Ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Сессия NotebookLM: WIZ_global_data + RPC поверх страницы профиля
// ---------------------------------------------------------------------------

/// Живая сессия NotebookLM: CDP-соединение к странице профиля движка.
///
/// RPC-запросы делает `fetch()` **в контексте страницы NotebookLM** —
/// относительный URL, cookies авторизации прикладываются браузером,
/// origin/referer честные, CORS не существует (тот же origin).
pub struct NlmSession {
    session: CdpSession,
    origin: String,
    app: String,
    bl: String,
    fsid: Option<String>,
    token: String,
    pub email: Option<String>,
    authuser: u64,
}

impl NlmSession {
    /// Открыть сессию: google-браузер (headless, персистентный профиль) →
    /// страница NotebookLM → чтение `WIZ_global_data`.
    pub fn open() -> Result<Self, String> {
        let port = google_cdp_port();
        ensure_google_browser(port, false)?;
        let mut session = CdpSession::connect(port)?;
        let base = nlm_base();
        session.load_page(&base, 3000)?;

        // host + WIZ_global_data одним вызовом (страница уже загружена)
        let raw = session.eval_async_string(
            "(async()=>JSON.stringify({\
                host:location.hostname,\
                origin:location.origin,\
                at:(window.WIZ_global_data&&WIZ_global_data.SNlM0e)||null,\
                app:(window.WIZ_global_data&&WIZ_global_data.qwAQke)||null,\
                bl:(window.WIZ_global_data&&WIZ_global_data.cfb2h)||null,\
                fsid:(window.WIZ_global_data&&WIZ_global_data.FdrFJe)||null,\
                email:(window.WIZ_global_data&&WIZ_global_data.oPEP7c)||null\
             }))()",
        )?;
        let wiz: Value =
            serde_json::from_str(&raw).map_err(|e| format!("WIZ-ответ не JSON: {e}"))?;
        let host = wiz.get("host").and_then(|h| h.as_str()).unwrap_or("").to_string();
        let not_logged_in = || {
            format!(
                "NotebookLM: сессия не авторизована (host={host}). Один раз залогинься \
                 руками: poler-engine --google-browse {} — окно Chromium откроется, \
                 войди в Google-аккаунт, куки профиля будут жить месяцами",
                nlm_base()
            )
        };
        if host.contains("accounts.google") {
            return Err(not_logged_in());
        }
        let token = wiz
            .get("at")
            .and_then(|t| t.as_str())
            .filter(|t| !t.is_empty())
            .ok_or_else(not_logged_in)?
            .to_string();
        Ok(Self {
            origin: wiz
                .get("origin")
                .and_then(|o| o.as_str())
                .unwrap_or(&base)
                .to_string(),
            app: wiz
                .get("app")
                .and_then(|a| a.as_str())
                .filter(|a| !a.is_empty())
                .unwrap_or(APP_FALLBACK)
                .to_string(),
            bl: wiz
                .get("bl")
                .and_then(|b| b.as_str())
                .filter(|b| !b.is_empty())
                .unwrap_or(BL_FALLBACK)
                .to_string(),
            fsid: wiz
                .get("fsid")
                .and_then(|f| f.as_str())
                .filter(|f| !f.is_empty())
                .map(String::from),
            token,
            email: wiz
                .get("email")
                .and_then(|e| e.as_str())
                .filter(|e| e.contains('@'))
                .map(String::from),
            authuser: 0,
            session,
        })
    }

    /// Один RPC-вызов batchexecute (в контексте страницы NotebookLM).
    fn rpc(
        &mut self,
        rpcid: &str,
        args: &Value,
        notebook_id: Option<&str>,
    ) -> Result<Value, String> {
        let sp = match notebook_id {
            Some(id) => format!("/notebook/{}", urlencode(id)),
            None => "/".to_string(),
        };
        let payload = serde_json::json!({
            "app": self.app,
            "bl": self.bl,
            "fsid": self.fsid,
            "authuser": self.authuser,
            "token": self.token,
            "id": rpcid,
            "args": args,
            "sp": sp,
        });
        let expr = format!(
            "(async()=>{{const o={p};try{{\
                const qp=new URLSearchParams({{bl:o.bl,hl:'en','source-path':o.sp}});\
                if(o.fsid)qp.set('f.sid',o.fsid);\
                if(o.authuser>0)qp.set('authuser',String(o.authuser));\
                const url='/_/'+o.app+'/data/batchexecute?'+qp;\
                const inner=JSON.stringify(o.args);\
                const freq=JSON.stringify([[[o.id,inner,null,'generic']]]);\
                const body=new URLSearchParams({{rpcids:o.id,'f.req':freq,at:o.token}});\
                const r=await fetch(url,{{method:'POST',\
                  headers:{{'content-type':'application/x-www-form-urlencoded;charset=UTF-8','x-same-domain':'1'}},\
                  body,credentials:'include'}});\
                const t=await r.text();\
                return JSON.stringify({{s:r.status,b:t}});\
             }}catch(e){{return JSON.stringify({{s:0,b:String(e)}})}}}})()",
            p = payload
        );
        let raw = self.session.eval_async_string(&expr)?;
        let v: Value = serde_json::from_str(&raw).map_err(|e| format!("rpc-ответ не JSON: {e}"))?;
        let status = v.get("s").and_then(|s| s.as_u64()).unwrap_or(0);
        let body = v.get("b").and_then(|b| b.as_str()).unwrap_or("").to_string();
        if status != 200 {
            return Err(format!(
                "batchexecute HTTP {status} ({}): {}",
                if status == 401 || status == 403 { "нет авторизации" } else { "ошибка" },
                body.chars().take(200).collect::<String>()
            ));
        }
        parse_rpc_body(&body)
    }

    /// Все ноутбуки аккаунта с источниками.
    pub fn list_notebooks(&mut self) -> Result<Vec<Notebook>, String> {
        let data = self.rpc(RPC_LIST_NOTEBOOKS, &serde_json::json!([null, 500]), None)?;
        Ok(parse_notebooks(&data))
    }

    /// Паспорт ноутбука (raw-JSON — схема богаче, чем в парсере).
    pub fn get_project(&mut self, notebook_id: &str) -> Result<Value, String> {
        self.rpc(RPC_GET_PROJECT, &serde_json::json!([notebook_id, null, [2]]), Some(notebook_id))
    }

    /// Контент источника: текст и/или URL картинок слайдов.
    pub fn load_source(&mut self, notebook_id: &str, source_id: &str) -> Result<SourceContent, String> {
        let data = self.rpc(
            RPC_LOAD_SOURCE,
            &serde_json::json!([[source_id], [2], [2]]),
            Some(notebook_id),
        )?;
        parse_source_content(&data).ok_or_else(|| "LOAD_SOURCE: пустой ответ".to_string())
    }

    /// Заметки ноутбука (raw-JSON).
    pub fn notes(&mut self, notebook_id: &str) -> Result<Value, String> {
        self.rpc(RPC_GET_NOTES, &serde_json::json!([notebook_id]), Some(notebook_id))
    }

    /// Studio-объекты ноутбука: аудио, отчёты, квизы, миндмэпы.
    pub fn artifacts(&mut self, notebook_id: &str) -> Result<Vec<Artifact>, String> {
        let data = self.rpc(
            RPC_LIST_ARTIFACTS,
            &serde_json::json!([
                [2],
                notebook_id,
                "NOT artifact.status = \"ARTIFACT_STATUS_SUGGESTED\""
            ]),
            Some(notebook_id),
        )?;
        Ok(parse_artifacts(&data))
    }

    /// Аккаунт сессии (raw-JSON: email, настройки вывода).
    pub fn account(&mut self) -> Result<Value, String> {
        self.rpc(
            RPC_ACCOUNT,
            &serde_json::json!([null, [1, null, null, null, null, null, null, null, null, null, [1]]]),
            None,
        )
    }

    /// Скачать медиа-файл тем же авторизованным профилем (cookies идут
    /// с нами): байты + content-type. Профиль движка запускается с
    /// `--disable-web-security`, поэтому кросс-доменные картинки Google
    /// тоже читаются.
    pub fn fetch_media(&mut self, url: &str) -> Result<(Vec<u8>, String), String> {
        let expr = format!(
            "(async()=>{{const o={u};try{{\
                const r=await fetch(o,{{credentials:'include'}});\
                const buf=await r.arrayBuffer();\
                const bytes=new Uint8Array(buf);\
                let bin='';const chunk=0x8000;\
                for(let i=0;i<bytes.length;i+=chunk)\
                    bin+=String.fromCharCode.apply(null,bytes.subarray(i,i+chunk));\
                return JSON.stringify({{s:r.status,ct:r.headers.get('content-type')||'',b64:btoa(bin)}});\
             }}catch(e){{return JSON.stringify({{s:0,ct:'',b64:'',e:String(e)}})}}}})()",
            u = serde_json::json!(url)
        );
        let raw = self.session.eval_async_string(&expr)?;
        let v: Value = serde_json::from_str(&raw).map_err(|e| format!("media-ответ не JSON: {e}"))?;
        let status = v.get("s").and_then(|s| s.as_u64()).unwrap_or(0);
        if status != 200 {
            let e = v.get("e").and_then(|e| e.as_str()).unwrap_or("");
            return Err(format!("медиа HTTP {status} {e}"));
        }
        let ct = v.get("ct").and_then(|c| c.as_str()).unwrap_or("").to_string();
        let b64 = v.get("b64").and_then(|b| b.as_str()).unwrap_or("");
        let bytes = crate::web::cdp::base64_decode(b64)?;
        if bytes.is_empty() {
            return Err("медиа: 0 байт".to_string());
        }
        Ok((bytes, ct))
    }

    /// Скриншот текущей страницы профиля (PNG) — медиа глазами юзера.
    pub fn screenshot(&mut self) -> Result<Vec<u8>, String> {
        self.session.capture_screenshot()
    }

    /// Перейти на URL в этой же сессии (для --nlm-shot).
    pub fn load_page_raw(&mut self, url: &str) -> Result<(), String> {
        self.session.load_page(url, 3000).map(|_| ())
    }

    /// URL страницы ноутбука в профиле.
    fn notebook_url(&self, notebook_id: &str) -> String {
        format!("{}/notebook/{}", self.origin, urlencode(notebook_id))
    }

    /// Чат с ноутбуком через UI: ввод вопроса в поле чата страницы и
    /// чтение появившегося ответа (без хрупких привязок к классам —
    /// эвристика «текст после вопроса стабилизировался»).
    pub fn chat(&mut self, notebook_id: &str, question: &str) -> Result<String, String> {
        if question.trim().is_empty() {
            return Err("пустой вопрос".to_string());
        }
        let url = self.notebook_url(notebook_id);
        self.session.load_page(&url, 4000)?;

        // 1) найти поле ввода чата
        let found = self.session.eval_async_string(
            "(async()=>{\
                const sels=['textarea','[contenteditable=\"true\"]','[role=\"textbox\"]'];\
                for(const s of sels){const el=document.querySelector(s);\
                    if(el)return JSON.stringify({sel:s});}\
                return JSON.stringify({sel:null});})()",
        )?;
        let sel: Option<String> = serde_json::from_str::<Value>(&found)
            .ok()
            .and_then(|v| v.get("sel").and_then(|s| s.as_str()).map(String::from));
        let Some(sel) = sel else {
            return Err("не найдено поле ввода чата (страница не загрузилась до конца?)".to_string());
        };

        // 2) вбить вопрос + Enter (+ запасной клик по кнопке отправки)
        let typed = self.session.eval_async_string(&format!(
            "(async()=>{{const o={p};\
             const el=document.querySelector(o.sel);\
             if(!el)return JSON.stringify({{ok:false,why:'input-vanished'}});\
             try{{\
               if(el.tagName==='TEXTAREA'||el.tagName==='INPUT'){{\
                 const d=Object.getOwnPropertyDescriptor(el.constructor.prototype,'value');\
                 (d&&d.set)?d.set.call(el,o.q):el.value=o.q;\
               }}else{{el.textContent=o.q;}}\
               el.dispatchEvent(new Event('input',{{bubbles:true}}));\
               el.focus();\
               const ke=(t)=>new KeyboardEvent(t,{{key:'Enter',code:'Enter',keyCode:13,which:13,bubbles:true,cancelable:true}});\
               el.dispatchEvent(ke('keydown'));el.dispatchEvent(ke('keypress'));el.dispatchEvent(ke('keyup'));\
               await new Promise(r=>setTimeout(r,600));\
               const val=(el.tagName==='TEXTAREA'||el.tagName==='INPUT')?el.value:(el.textContent||'');\
               if(val.trim()===o.q.trim()){{\
                 const btns=[...document.querySelectorAll('button')];\
                 const b=btns.find(x=>/send|відправ|надісл|отправ/i.\
                     test(x.getAttribute('aria-label')||''));\
                 if(b&&!b.disabled)b.click();\
               }}\
               return JSON.stringify({{ok:true}});\
             }}catch(e){{return JSON.stringify({{ok:false,why:String(e)}})}}}})()",
            p = serde_json::json!({"sel": sel, "q": question})
        ))?;
        let ok: bool = serde_json::from_str::<Value>(&typed)
            .ok()
            .and_then(|v| v.get("ok").and_then(|b| b.as_bool()))
            .unwrap_or(false);
        if !ok {
            let why = serde_json::from_str::<Value>(&typed)
                .ok()
                .and_then(|v| v.get("why").and_then(|w| w.as_str()).map(String::from))
                .unwrap_or_else(|| "?".to_string());
            return Err(format!("не удалось отправить вопрос: {why}"));
        }

        // 3) дождаться ответа: вопрос появился в тексте → текст стабилизировался
        let probe = |s: &mut CdpSession, q: &str| -> Result<(usize, bool), String> {
            let raw = s.eval_async_string(&format!(
                "(async()=>JSON.stringify({{n:document.body.innerText.length,\
                  has:document.body.innerText.includes({q})}}))()",
                q = serde_json::json!(q)
            ))?;
            let v: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
            Ok((
                v.get("n").and_then(|n| n.as_u64()).unwrap_or(0) as usize,
                v.get("has").and_then(|h| h.as_bool()).unwrap_or(false),
            ))
        };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(chat_timeout_s());
        // фаза 1: вопрос отрисовался (до 20 с)
        let phase1 = deadline.min(std::time::Instant::now() + std::time::Duration::from_secs(20));
        while std::time::Instant::now() < phase1 {
            if let Ok((_, true)) = probe(&mut self.session, question) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(700));
        }
        // фаза 2: длина текста стабильна 3 poll-а подряд (стриминг ответа кончился)
        let mut last_len = 0usize;
        let mut stable = 0u32;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1200));
            if let Ok((n, _)) = probe(&mut self.session, question) {
                if n == last_len && n > 0 {
                    stable += 1;
                    if stable >= 3 {
                        break;
                    }
                } else {
                    stable = 0;
                    last_len = n;
                }
            }
        }

        // 4) ответ = текст после последнего вхождения вопроса
        let text = self.session.eval_async_string("(async()=>document.body.innerText)()")?;
        let answer = match text.rfind(question.trim()) {
            Some(i) => text[i + question.trim().len()..].trim().to_string(),
            None => text.trim().to_string(),
        };
        if answer.is_empty() {
            return Err(
                "ответ пуст — вопрос не отправился или ноутбук не ответил за таймаут \
                 (POLER_NLM_CHAT_TIMEOUT повышает ожидание)"
                    .to_string(),
            );
        }
        Ok(answer)
    }
}

/// Минимальный %-encode для path/query компонентов.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Форматтеры CLI/MCP
// ---------------------------------------------------------------------------

/// Расширение файла из content-type (CLI и MCP).
pub fn mime_ext(ct: &str) -> &'static str {
    let base = ct.split(';').next().unwrap_or("").trim();
    match base {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/mp4" | "audio/x-m4a" => "m4a",
        "audio/wav" => "wav",
        "video/mp4" => "mp4",
        "application/pdf" => "pdf",
        "text/plain" | "text/markdown" => "txt",
        _ => "bin",
    }
}

/// Каталог скачанных медиа/скриншотов: `~/.cache/poler-engine/nlm/`
/// (персистентный, как google-profile — файлы переживают перезапуски).
pub fn media_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    std::path::PathBuf::from(home).join(".cache/poler-engine/nlm")
}

/// Сохранить байты в неиспользуемый файл `<prefix>-N.<ext>` в [`media_dir`].
pub fn save_media(prefix: &str, ext: &str, bytes: &[u8]) -> Result<std::path::PathBuf, String> {
    let dir = media_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("создать {}: {e}", dir.display()))?;
    for n in 1..10_000 {
        let p = dir.join(format!("{prefix}-{n:02}.{ext}"));
        if !p.exists() {
            std::fs::write(&p, bytes).map_err(|e| format!("запись {}: {e}", p.display()))?;
            return Ok(p);
        }
    }
    Err("не найдено свободного имени для файла".to_string())
}

/// Markdown-список ноутбуков с источниками.
pub fn format_notebooks(nbs: &[Notebook]) -> String {
    let mut out = String::from("# NotebookLM: ноутбуки\n\n");
    for (i, nb) in nbs.iter().enumerate() {
        let upd = nb.updated_at.as_deref().unwrap_or("—");
        out.push_str(&format!(
            "## {}. {}{}\n\n",
            i + 1,
            nb.emoji,
            nb.title
        ));
        out.push_str(&format!("- ID: `{}`\n", nb.id));
        out.push_str(&format!("- Источников: {}, обновлён: {}\n", nb.sources.len(), upd));
        if !nb.sources.is_empty() {
            out.push_str("- Источники:\n");
            for s in &nb.sources {
                let url = s.url.as_deref().map(|u| format!(" — {u}")).unwrap_or_default();
                out.push_str(&format!(
                    "  - [{}] {}{}{}\n",
                    s.kind,
                    s.title,
                    url,
                    s.youtube_id.as_deref().map(|y| format!(" ({y})")).unwrap_or_default()
                ));
            }
        }
        out.push('\n');
    }
    out
}

/// Markdown контента источника.
pub fn format_source_content(sc: &SourceContent, notebook_id: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!("# Источник: {}\n\n", sc.title));
    out.push_str(&format!("- Ноутбук: `{}`\n", notebook_id));
    out.push_str(&format!("- ID источника: `{}`\n", sc.id));
    out.push_str(&format!("- Тип: {}\n\n", sc.kind));
    if let Some(text) = &sc.content {
        out.push_str(&format!("## Текст\n\n{}\n", text));
    }
    if !sc.images.is_empty() {
        out.push_str(&format!("\n## Медиа ({})\n\n", sc.images.len()));
        for (i, img) in sc.images.iter().enumerate() {
            out.push_str(&format!("{}. {}\n", i + 1, img.url));
        }
        out.push_str("\nСкачать: poler-engine --nlm-media <URL>\n");
    }
    out
}

/// Markdown списка Studio-объектов.
pub fn format_artifacts(arts: &[Artifact]) -> String {
    if arts.is_empty() {
        return "# Studio: пусто\n".to_string();
    }
    let mut out = String::from("# Studio-объекты ноутбука\n\n");
    for (i, a) in arts.iter().enumerate() {
        out.push_str(&format!(
            "{}. [{}] {} — {}\n",
            i + 1,
            a.kind,
            a.title,
            a.status
        ));
        out.push_str(&format!("   ID: `{}`\n", a.id));
    }
    out
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn base64_decode_roundtrip_with_cdp_encoder() {
        // вектор из тестов cdp.rs: "foobar" ↔ "Zm9vYmFy"
        assert_eq!(crate::web::cdp::base64_decode("Zm9vYmFy").unwrap(), b"foobar");
        assert_eq!(crate::web::cdp::base64_decode("Zg==").unwrap(), b"f");
        assert_eq!(crate::web::cdp::base64_decode("Zm8=").unwrap(), b"fo");
        assert!(crate::web::cdp::base64_decode("Zm9v!").is_err());
        // без паддинга (так отдаёт btoa при кратных 3)
        assert_eq!(crate::web::cdp::base64_decode("Zm9vYmFy").unwrap(), b"foobar");
    }

    #[test]
    fn rpc_body_parses_payload_at_index_2() {
        // формат расширения NLMTools / HanaokaYuzu-Gemini: payload на [0][2]
        let payload = json!([["Мой ноут", [], "nb-1", "🧠", null, [null, 1, null, 42, null, 1]]]);
        // payload в конверте — JSON-СТРОКА (двойное кодирование)
        let quoted = serde_json::to_string(&serde_json::to_string(&payload).unwrap()).unwrap();
        let prefix = ")]}'";
        let body = format!("{prefix}\n\n[[\"wXbhsf\",null,{q},null,null,[null,0]]]", q = quoted);
        assert_eq!(parse_rpc_body(&body).unwrap(), payload);
    }

    #[test]
    fn rpc_body_fallback_payload_at_index_1() {
        // запасная форма конверта: payload на [0][1]
        let body = ")]}'\n\n[[\"wXbhsf\",\"[1,2,3]\",null,\"generic\"]]";
        assert_eq!(parse_rpc_body(body).unwrap(), json!([1, 2, 3]));
    }

    #[test]
    fn rpc_body_maps_error_codes() {
        let quota = ")]}'\n\n[[\"x\",null,\"null\",null,null,[8,0]]]";
        let err = parse_rpc_body(quota).unwrap_err();
        assert!(err.contains("квота"), "{err}");
        let auth = ")]}'\n\n[[\"x\",null,\"null\",null,null,[16,0]]]";
        assert!(parse_rpc_body(auth).unwrap_err().contains("авторизована"));
        // строковый код — не ошибка
        let ok = ")]}'\n\n[[\"x\",null,\"[1]\",null,null,[\"generic\",0]]]";
        assert_eq!(parse_rpc_body(ok).unwrap(), json!([1]));
    }

    #[test]
    fn rpc_body_rejects_garbage() {
        assert!(parse_rpc_body("<html>err</html>").is_err());
        assert!(parse_rpc_body(")]}'\n\n{}").is_err());
    }

    #[test]
    fn notebooks_parse_with_sources_and_dates() {
        // схема: [title, sources, id, emoji, _, meta[perm,_,_,_,_,updated,_,_,created]]
        let data = json!([[
            [
                "Касіопея",
                [
                    ["s-1", "Роман повний текст", [
                        null, null, [1735689600, 0], [null, [1700000000, 0]], 4
                    ]],
                    ["s-2", "Відео розбір", [
                        null, null, null, null, 9,
                        ["https://youtu.be/abc", "abc", "Канал"]
                    ]]
                ],
                "nb-77", "📚", null,
                [1, null, null, null, null, [1735689600, 0], null, null, [1700000000, 0]]
            ],
            ["", [], "nb-78", "", null, null] // Untitled без меты
        ]]);
        let nbs = parse_notebooks(&data);
        assert_eq!(nbs.len(), 2);
        let nb = &nbs[0];
        assert_eq!(nb.id, "nb-77");
        assert_eq!(nb.title, "Касіопея");
        assert_eq!(nb.emoji, "📚");
        assert_eq!(nb.permission, Some(1));
        assert_eq!(nb.updated_at.as_deref(), Some("2025-01-01T00:00:00Z"));
        assert_eq!(nb.created_at.as_deref(), Some("2023-11-14T22:13:20Z"));
        assert_eq!(nb.sources.len(), 2);
        assert_eq!(nb.sources[0].kind, "Текст");
        assert_eq!(nb.sources[0].updated_at.as_deref(), Some("2025-01-01T00:00:00Z"));
        let yt = &nb.sources[1];
        assert_eq!(yt.kind, "YouTube");
        assert_eq!(yt.url.as_deref(), Some("https://youtu.be/abc"));
        assert_eq!(yt.youtube_id.as_deref(), Some("abc"));
        assert_eq!(yt.author.as_deref(), Some("Канал"));
        assert_eq!(nbs[1].title, "Untitled notebook");
    }

    #[test]
    fn notebooks_parse_wrapper_array() {
        // данные обёрнуты ещё одним массивом: [[rows]]
        let data = json!([[["T", [], "id-1", "", null, null]]]);
        let nbs = parse_notebooks(&data);
        assert_eq!(nbs.len(), 1);
        assert_eq!(nbs[0].title, "T");
    }

    #[test]
    fn source_content_text_and_slide_images() {
        // текст: блоки = data[3][0][0]; блок s[2][0] → куски E[2][0]
        let text_data = json!([
            [["src-9"], "Документ про двигун", [null, null, null, null, null, null, null, null, null, null, [11]]],
            null, null,
            [[[  // data[3][0][0] — массив блоков
                [null, null, [[[null, null, ["Рядок перший."]]]]],
                [null, null, [[[null, null, ["Рядок другий."]]]]]
            ]]]
        ]);
        let sc = parse_source_content(&text_data).unwrap();
        assert_eq!(sc.title, "Документ про двигун");
        assert_eq!(sc.content.as_deref(), Some("Рядок перший.\nРядок другий."));
        assert!(sc.images.is_empty());

        // слайды: блоки = data[3][0][0]; блок l[5] = [url, ?, id]
        let slides_data = json!([
            [["src-10"], "Презентація", [null, null, null, null, 2]],
            null, null,
            [[[  // data[3][0][0]
                [null, null, null, null, null, ["https://lh3.googleusercontent.com/slide1.png", null, "img-1"]],
                [null, null, null, null, null, ["https://lh3.googleusercontent.com/slide2.jpg", null, "img-2"]]
            ]]]
        ]);
        let sc2 = parse_source_content(&slides_data).unwrap();
        assert_eq!(sc2.kind, "Google Slides");
        assert!(sc2.content.is_none());
        assert_eq!(sc2.images.len(), 2);
        assert_eq!(sc2.images[0].url, "https://lh3.googleusercontent.com/slide1.png");
        assert_eq!(sc2.images[1].id.as_deref(), Some("img-2"));
    }

    #[test]
    fn artifacts_parse_kinds_and_status() {
        let data = json!([[
            ["art-1", "Аудіо огляд", 1, [["s-1"], ["s-2"]], 3],
            ["art-2", "Квиз", 4, null, 1],
            ["", "нет id — пропустить", 2, null, 3]
        ]]);
        let arts = parse_artifacts(&data);
        assert_eq!(arts.len(), 2);
        assert_eq!(arts[0].kind, "Audio");
        assert_eq!(arts[0].status, "ready");
        assert_eq!(arts[0].source_ids, vec!["s-1", "s-2"]);
        assert_eq!(arts[1].kind, "Quiz/Flashcards");
        assert_eq!(arts[1].status, "processing");
    }

    #[test]
    fn civil_dates_corner() {
        // 1970-01-01 и 2025-01-01
        assert_eq!(sec_nanos_iso(&json!([0, 0])), None);
        assert_eq!(
            sec_nanos_iso(&json!([1735689600, 0])).as_deref(),
            Some("2025-01-01T00:00:00Z")
        );
        assert_eq!(
            sec_nanos_iso(&json!([0, 1_735_689_600_000_000_000i64])).as_deref(),
            None // секунды 0 → эпоха, не дата (фильтр sec < 1)
        );
        // наносекунды без секунд не делают дату (фильтр), но 1 сек + наносы — да
        assert_eq!(
            sec_nanos_iso(&json!([1, 500_000_000i64])).as_deref(),
            Some("1970-01-01T00:00:01Z")
        );
    }

    #[test]
    fn urlencode_keeps_safe_chars() {
        assert_eq!(urlencode("nb-77.abc~x"), "nb-77.abc~x");
        assert_eq!(urlencode("id/с кириллицей"), "id%2F%D1%81%20%D0%BA%D0%B8%D1%80%D0%B8%D0%BB%D0%BB%D0%B8%D1%86%D0%B5%D0%B9");
    }

    #[test]
    fn formatters_render_markdown() {
        let nb = Notebook {
            id: "nb-1".into(),
            title: "Тест".into(),
            emoji: "🧪".into(),
            permission: Some(1),
            updated_at: Some("2025-01-01T00:00:00Z".into()),
            created_at: None,
            sources: vec![SourceMeta {
                id: "s-1".into(),
                title: "Doc".into(),
                kind: "Google Docs".into(),
                url: None,
                youtube_id: None,
                author: None,
                drive_file_id: Some("df-1".into()),
                mime: None,
                updated_at: None,
                created_at: None,
            }],
        };
        let md = format_notebooks(&[nb]);
        assert!(md.contains("# NotebookLM"));
        assert!(md.contains("🧪Тест"));
        assert!(md.contains("[Google Docs] Doc"));

        let sc = SourceContent {
            id: "s-1".into(),
            title: "Doc".into(),
            kind: "PDF".into(),
            content: Some("текст источника".into()),
            images: vec![ImageRef { url: "https://x/1.png".into(), id: None }],
        };
        let md2 = format_source_content(&sc, "nb-1");
        assert!(md2.contains("# Источник: Doc"));
        assert!(md2.contains("## Медиа (1)"));
    }
}
