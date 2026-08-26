//! Google-интеграция БЕЗ ПАРОЛЯ: два штатных механизма.
//!
//! 1. **OAuth 2.0 loopback** (RFC 8252) — для сервисов с публичными API
//!    (Gmail, Drive, Calendar): пароль остаётся между владельцем и формой
//!    Google **в его собственном браузере**. poler-engine получает только
//!    временный токен с узкими скоупами (`gmail.readonly`,
//!    `drive.readonly`), отзываемый в любой момент на
//!    myaccount.google.com/permissions. См. [`oauth`].
//!
//! 2. **Персистентный профиль браузера** — для сервисов без публичного API
//!    (NotebookLM и т.п.): `--google-browse` поднимает Chromium с
//!    выделенным `--user-data-dir`, владелец логинится там **один раз
//!    своими руками**, куки живут месяцами. Дальше `--google-fetch`
//!    читает уже авторизованную сессию headless-ом через тот же профиль.
//!
//! HTTPS-клиент — сам Chromium: [`GoogleHttp`] выполняет `fetch()` в
//! контексте страницы через CDP `Runtime.evaluate` + `awaitPromise`.
//! TLS отдаём браузеру — в Rust по-прежнему ноль TLS-зависимостей.
//! Браузер google-профиля запускается с `--disable-web-security`
//! (это отдельный профиль для API-вызовов, не stealth-краулер).

pub mod api;
pub mod companion;
pub mod nlm;
pub mod nlm_ingest;
pub mod oauth;

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::web::cdp::CdpSession;
use crate::web::{cdp_alive, find_browser};

/// CDP-порт google-браузера (отдельный от stealth-краулера 9222).
pub fn google_cdp_port() -> u16 {
    std::env::var("POLER_GOOGLE_CDP_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9223)
}

/// Конфиг-директория: `$POLER_CONFIG_DIR` → `~/.config/poler-engine`.
pub fn config_dir() -> PathBuf {
    if let Ok(p) = std::env::var("POLER_CONFIG_DIR") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".config/poler-engine")
}

/// Путь к файлу токенов: `$POLER_GOOGLE_TOKENS` → конфиг/google_tokens.json.
pub fn tokens_path() -> PathBuf {
    if let Ok(p) = std::env::var("POLER_GOOGLE_TOKENS") {
        return PathBuf::from(p);
    }
    config_dir().join("google_tokens.json")
}

/// Путь к файлу GCP-токенов (скоуп `cloud-platform` для NotebookLM Enterprise API):
/// `$POLER_GCP_TOKENS` → конфиг/gcp_tokens.json.
///
/// Отдельный файл от `google_tokens.json`, чтобы:
/// 1. Не требовать повторный consent от пользователя при добавлении скоупа в существующий набор.
/// 2. Изолировать более привилегированный токен (cloud-platform даёт доступ ко всему GCP).
/// 3. Дать возможность отозвать только GCP-доступ, не затрагивая Gmail/Drive.
pub fn gcp_tokens_path() -> PathBuf {
    if let Ok(p) = std::env::var("POLER_GCP_TOKENS") {
        return PathBuf::from(p);
    }
    config_dir().join("gcp_tokens.json")
}

/// Персистентный профиль браузера: `$POLER_GOOGLE_PROFILE` →
/// `~/.cache/poler-engine/google-profile`. Здесь живут куки Google-сессии
/// (NotebookLM и др.) между запусками.
pub fn profile_dir() -> PathBuf {
    if let Ok(p) = std::env::var("POLER_GOOGLE_PROFILE") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".cache/poler-engine/google-profile")
}

// ---------------------------------------------------------------------------
// Google-браузер: персистентный профиль, --disable-web-security, (не)headless
// ---------------------------------------------------------------------------

/// Поиск бинаря для ИНТЕРАКТИВНОГО (headed) запуска: полный Chromium,
/// headless-shell не подходит (у него нет окна для ручного логина).
fn find_headed_browser() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLER_CHROME_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    for name in [
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
    ] {
        if let Some(path) = std::env::var("PATH")
            .ok()?
            .split(':')
            .map(|dir| std::path::Path::new(dir).join(name))
            .find(|p| p.is_file())
        {
            return Some(path);
        }
    }
    [
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/snap/bin/chromium",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Является ли бинарь headless-shell (не умеет показывать окно).
fn is_headless_shell(bin: &std::path::Path) -> bool {
    bin.file_name()
        .map(|n| n.to_string_lossy().contains("headless-shell"))
        .unwrap_or(false)
}

/// Аргументы запуска google-браузера (общие для headless/headed).
fn google_browser_args(port: u16) -> Vec<String> {
    let profile = profile_dir();
    vec![
        "--no-sandbox".to_string(),
        "--disable-gpu".to_string(),
        "--disable-dev-shm-usage".to_string(),
        "--no-first-run".to_string(),
        "--disable-blink-features=AutomationControlled".to_string(),
        // CORS выкл: fetch() из about:blank к API Google читается свободно.
        // Это отдельный API-профиль движка, не stealth-краулер.
        "--disable-web-security".to_string(),
        format!("--user-data-dir={}", profile.display()),
        format!("--remote-debugging-port={port}"),
    ]
}

/// Гарантирует живой google-браузер на `port` с персистентным профилем.
/// `headed = true` — с окном (для ручного логина в сервисы без API).
/// Если браузер уже поднят (например, окно `--google-browse` открыто) —
/// переиспользуем его.
pub fn ensure_google_browser(port: u16, headed: bool) -> Result<(), String> {
    if cdp_alive(port) {
        return Ok(());
    }
    let bin = if headed {
        match find_headed_browser() {
            Some(b) if !is_headless_shell(&b) => b,
            _ => {
                return Err(
                    "интерактивный логин требует полный Chromium с окном. \
                     Установи POLER_CHROME_BIN=/usr/bin/chromium (или другой \
                     полный браузер) и повтори --google-browse"
                        .to_string(),
                )
            }
        }
    } else {
        find_browser().ok_or_else(|| {
            "Chromium не найден. Установите POLER_CHROME_BIN=/путь/к/chromium \
             или положите бинарь в PATH"
                .to_string()
        })?
    };

    let _ = std::fs::create_dir_all(profile_dir());
    let mut args = google_browser_args(port);
    if !headed {
        args.push("--headless".to_string());
    }
    args.push("about:blank".to_string());

    let log = std::env::temp_dir().join("poler-google-chromium.log");
    let log_f = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("лог {log:?}: {e}"))?;
    let mut child = std::process::Command::new(&bin)
        .args(&args)
        .stdout(log_f.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_f)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("запуск {:?}: {e}", bin))?;
    for _ in 0..60 {
        if cdp_alive(port) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let _ = child.kill();
    Err(format!(
        "google-браузер не поднялся на порт {port} за 15 с (лог: {log:?})"
    ))
}

// ---------------------------------------------------------------------------
// HTTP-мост: браузер как HTTPS-клиент (fetch через Runtime.evaluate)
// ---------------------------------------------------------------------------

/// JS-выражение для CDP: async fetch, возвращает JSON {s: status, b: body}.
/// Данные встраиваются как JSON-литерал (валидный JS), двойного
/// экранирования не требуется.
pub fn fetch_js(
    url: &str,
    method: &str,
    headers: &[(&str, String)],
    body: Option<&str>,
) -> String {
    let mut h = serde_json::Map::new();
    for (k, v) in headers {
        h.insert(k.to_string(), serde_json::json!(v));
    }
    let payload = serde_json::json!({
        "u": url,
        "m": method,
        "h": h,
        "b": body,
    });
    format!(
        "(async()=>{{const o={p};try{{\
          const r=await fetch(o.u,{{method:o.m,headers:o.h,body:o.b||undefined}});\
          const t=await r.text();\
          return JSON.stringify({{s:r.status,b:t}});\
         }}catch(e){{return JSON.stringify({{s:0,b:String(e)}})}}}})()",
        p = payload
    )
}

/// HTTPS-клиент поверх google-браузера: одна живая CDP-сессия.
pub struct GoogleHttp {
    session: CdpSession,
}

impl GoogleHttp {
    /// Поднимает google-браузер (headless) при необходимости и коннектится.
    pub fn connect(port: u16) -> Result<Self, String> {
        ensure_google_browser(port, false)?;
        Ok(Self {
            session: CdpSession::connect(port)?,
        })
    }

    /// Произвольный HTTP-запрос через браузер: (статус, тело).
    pub fn request(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(&str, String)],
        body: Option<&str>,
    ) -> Result<(u16, String), String> {
        let expr = fetch_js(url, method, headers, body);
        let raw = self.session.eval_async_string(&expr)?;
        if raw.is_empty() {
            return Err("пустой ответ fetch (JS вернул не строку)".to_string());
        }
        let v: serde_json::Value =
            serde_json::from_str(&raw).map_err(|e| format!("fetch-ответ не JSON: {e}"))?;
        let status = v.get("s").and_then(|s| s.as_u64()).unwrap_or(0) as u16;
        let body = v
            .get("b")
            .and_then(|b| b.as_str())
            .unwrap_or("")
            .to_string();
        Ok((status, body))
    }

    /// GET c заголовками (тело парсится как JSON вызывающим).
    pub fn get(&mut self, url: &str, headers: &[(&str, String)]) -> Result<(u16, String), String> {
        self.request("GET", url, headers, None)
    }

    /// POST формы (application/x-www-form-urlencoded).
    pub fn post_form(&mut self, url: &str, form: &str) -> Result<(u16, String), String> {
        self.request(
            "POST",
            url,
            &[("Content-Type", "application/x-www-form-urlencoded".to_string())],
            Some(form),
        )
    }
}

// ---------------------------------------------------------------------------
// --google-browse / --google-fetch: сервисы без API (NotebookLM и др.)
// ---------------------------------------------------------------------------

/// Поле `Browser` из `/json/version` («Chrome/152…» или «HeadlessChrome/152…»).
fn browser_version(port: u16) -> Result<String, String> {
    let raw = crate::web::cdp::http_get("127.0.0.1", port, "/json/version")?;
    let v: serde_json::Value =
        serde_json::from_str(&raw).map_err(|e| format!("json /json/version: {e}"))?;
    v.get("Browser")
        .and_then(|b| b.as_str())
        .map(String::from)
        .ok_or_else(|| "нет поля Browser".to_string())
}

/// Открыть URL в браузере с персистентным профилем poler-engine
/// (с окном — для ручного логина один раз). Браузер остаётся живым
/// после выхода из CLI: владелец закрывает окно сам, когда закончил.
///
/// Если на порту висит headless-инстанс (после --google-gmail/drive) —
/// он корректно закрывается (CDP `Browser.close`) и поднимается оконный.
pub fn browse(url: &str) -> Result<(), String> {
    let port = google_cdp_port();
    if cdp_alive(port) {
        match browser_version(port) {
            Ok(v) if v.starts_with("HeadlessChrome") => {
                // headless не годится для ручного логина — уступает место
                if let Ok(mut s) = CdpSession::connect(port) {
                    let _ = s.close_browser();
                }
                for _ in 0..20 {
                    if !cdp_alive(port) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
            Ok(_) => {
                // уже работает оконный браузер с этим профилем —
                // второй запуск просто откроет новую вкладку с URL
            }
            Err(_) => {}
        }
    }
    if !cdp_alive(port) {
        // headed-запуск: без --headless, страница открывается как аргумент
        let bin = match find_headed_browser() {
            Some(b) if !is_headless_shell(&b) => b,
            _ => {
                return Err(
                    "нужен полный Chromium с окном для ручного логина. \
                     Установи POLER_CHROME_BIN=/usr/bin/chromium и повтори"
                        .to_string(),
                )
            }
        };
        let _ = std::fs::create_dir_all(profile_dir());
        let mut args = google_browser_args(port);
        args.push(url.to_string());
        std::process::Command::new(&bin)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("запуск {:?}: {e}", bin))?;
        // готовность CDP (браузер откроет окно с URL)
        for _ in 0..60 {
            if cdp_alive(port) {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    Ok(())
}

/// Прочитать страницу через персистентный профиль (headless, CDP):
/// контент авторизованных сервисов — NotebookLM после логина
/// через `--google-browse`, и т.п.
pub fn fetch_profiled(url: &str, wait_ms: u64) -> Result<crate::web::cdp::WebPage, String> {
    let port = google_cdp_port();
    ensure_google_browser(port, false)?;
    let mut session = CdpSession::connect(port)?;
    session.load_page(url, wait_ms)
}

/// Тихо открыть URL в браузере пользователя (consent-экран Google).
/// В песочнице/SSH xdg-open может отсутствовать — тогда URL просто
/// печатается в терминал вызывающим кодом.
pub fn open_in_user_browser(url: &str) {
    for opener in ["xdg-open", "sensible-browser", "x-www-browser"] {
        if std::process::Command::new(opener)
            .arg(url)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .is_ok()
        {
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// мелочи, общие для oauth/api
// ---------------------------------------------------------------------------

/// Прочитать HTTP-запрос из сокета до конца заголовков (один байт за раз —
/// запросы крошечные).
pub(crate) fn read_http_head(stream: &mut TcpStream) -> String {
    let mut buf = Vec::with_capacity(512);
    let mut b = [0u8; 1];
    loop {
        match stream.read(&mut b) {
            Ok(0) => break,
            Ok(_) => {
                buf.push(b[0]);
                if buf.ends_with(b"\r\n\r\n") || buf.len() > 8192 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).to_string()
}

/// Отправить простой HTTP-ответ и закрыть.
pub(crate) fn write_http_response(stream: &mut TcpStream, body: &str) {
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
    let _ = stream.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_js_embeds_payload_and_awaits() {
        let js = fetch_js(
            "https://oauth2.googleapis.com/token",
            "POST",
            &[("Content-Type", "application/x-www-form-urlencoded".to_string())],
            Some("grant_type=authorization_code"),
        );
        assert!(js.starts_with("(async()=>{const o={"), "JS-обёртка: {js}");
        // serde_json::Map сортирует ключи (b,h,m,u) — проверяем по содержимому
        assert!(js.contains("\"u\":\"https://oauth2.googleapis.com/token\""));
        assert!(js.contains("\"m\":\"POST\""));
        assert!(js.contains("grant_type=authorization_code"));
        assert!(js.contains("await fetch"));
        assert!(js.ends_with("})()"));
    }

    #[test]
    fn fetch_js_escapes_quotes_in_body() {
        let js = fetch_js("https://x/", "POST", &[], Some("a=\"quoted\""));
        assert!(js.contains("\\\"quoted\\\""), "JSON-экранирование работает: {js}");
    }

    #[test]
    fn config_paths_are_poler_scoped() {
        assert!(tokens_path().to_string_lossy().contains("poler-engine"));
        assert!(profile_dir().to_string_lossy().contains("poler-engine"));
    }

    #[test]
    fn headed_browser_rejects_headless_shell_names() {
        let p = PathBuf::from("/opt/chrome-headless-shell-linux64/chrome-headless-shell");
        assert!(is_headless_shell(&p));
        let p = PathBuf::from("/usr/bin/chromium");
        assert!(!is_headless_shell(&p));
    }

    #[test]
    fn google_browser_args_have_profile_and_security_off() {
        let args = google_browser_args(9223);
        assert!(args.iter().any(|a| a.starts_with("--user-data-dir=")));
        assert!(args.contains(&"--disable-web-security".to_string()));
        assert!(args.contains(&"--remote-debugging-port=9223".to_string()));
        assert!(!args.contains(&"--headless".to_string()), "headed по умолчанию без headless");
    }

    #[test]
    fn default_port_9223_env_override() {
        // без env — 9223 (env восстанавливаем: тесты идут параллельно)
        let saved = std::env::var("POLER_GOOGLE_CDP_PORT");
        std::env::remove_var("POLER_GOOGLE_CDP_PORT");
        assert_eq!(google_cdp_port(), 9223);
        if let Ok(v) = saved {
            std::env::set_var("POLER_GOOGLE_CDP_PORT", v);
        }
    }
}
