//! OAuth 2.0 Authorization Code + loopback redirect (RFC 8252).
//!
//! Поток (пароль владельца НИКОГДА не попадает в poler-engine):
//!
//! ```text
//! 1. poler-engine --google-auth
//!    ├─ читает client_secret.json (свой GCP-проект владельца)
//!    ├─ поднимает ловушку на http://127.0.0.1:<случайный порт>
//!    └─ открывает БРАУЗЕР ВЛАДЕЛЬЦА на consent-экране Google
//! 2. владелец логинится своими руками в СВОЁМ браузере
//!    (там уже могут быть его Google-сессии — один клик «Разрешить»)
//! 3. Google редиректит браузер на http://127.0.0.1:<порт>?code=…&state=…
//!    └─ ловушка ловит код, проверяет state (CSRF)
//! 4. poler-engine меняет code на токены через HTTPS
//!    (TLS делает google-браузер движка — см. GoogleHttp)
//!    └─ access_token (~1 ч) + refresh_token (постоянный)
//! 5. токены сохраняются в ~/.config/poler-engine/google_tokens.json (0600)
//! ```
//!
//! Отзыв в любой момент: myaccount.google.com/permissions.
//! Скоупы по умолчанию — только чтение: gmail.readonly + drive.readonly.

use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::{gcp_tokens_path, read_http_head, tokens_path, write_http_response, GoogleHttp};
use crate::web::cdp::random_key_16;

pub const GMAIL_READONLY: &str = "https://www.googleapis.com/auth/gmail.readonly";
pub const DRIVE_READONLY: &str = "https://www.googleapis.com/auth/drive.readonly";

/// Cloud-platform — «корневой» скоуп GCP, даёт доступ к Discovery Engine API
/// (NotebookLM Enterprise). Применяется ТОЛЬКО для `GcpEnterpriseProvider` и
/// хранится отдельно в `gcp_tokens.json`, чтобы не смешивать с Gmail/Drive.
pub const CLOUD_PLATFORM: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Consent-эндпоинт (переопределяется POLER_GOOGLE_AUTH_URI для тестов).
pub fn auth_uri() -> String {
    std::env::var("POLER_GOOGLE_AUTH_URI")
        .unwrap_or_else(|_| "https://accounts.google.com/o/oauth2/v2/auth".to_string())
}

/// Token-эндпоинт (POLER_GOOGLE_TOKEN_URI).
pub fn token_uri() -> String {
    std::env::var("POLER_GOOGLE_TOKEN_URI")
        .unwrap_or_else(|_| "https://oauth2.googleapis.com/token".to_string())
}

/// Tokeninfo-эндпоинт (POLER_GOOGLE_TOKENINFO_URI).
pub fn tokeninfo_uri() -> String {
    std::env::var("POLER_GOOGLE_TOKENINFO_URI")
        .unwrap_or_else(|_| "https://oauth2.googleapis.com/tokeninfo".to_string())
}

// ---------------------------------------------------------------------------
// client_secret.json (свой GCP-проект владельца)
// ---------------------------------------------------------------------------

/// OAuth-клиент владельца (тип «Desktop app» из Google Cloud Console).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSecret {
    pub client_id: String,
    pub client_secret: String,
}

/// Парсинг client_secret.json: поддерживаются ключи `installed` (Desktop)
/// и `web` (Web-application) — GCP отдаёт оба формата.
pub fn parse_client_secret(text: &str) -> Result<ClientSecret, String> {
    let v: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("JSON: {e}"))?;
    let obj = v
        .get("installed")
        .or_else(|| v.get("web"))
        .ok_or_else(|| {
            "нет ключа installed/web — это точно client_secret.json из \
             Google Cloud Console?"
                .to_string()
        })?;
    let client_id = obj
        .get("client_id")
        .and_then(|x| x.as_str())
        .ok_or("client_id отсутствует")?
        .to_string();
    let client_secret = obj
        .get("client_secret")
        .and_then(|x| x.as_str())
        .ok_or("client_secret отсутствует")?
        .to_string();
    Ok(ClientSecret {
        client_id,
        client_secret,
    })
}

/// Поиск client_secret.json: `$POLER_GOOGLE_SECRET` → `./client_secret.json`
/// → `~/.config/poler-engine/client_secret.json`.
pub fn load_client_secret() -> Result<(ClientSecret, PathBuf), String> {
    let candidates: Vec<PathBuf> = {
        let mut v = Vec::new();
        if let Ok(p) = std::env::var("POLER_GOOGLE_SECRET") {
            v.push(PathBuf::from(p));
        }
        v.push(PathBuf::from("client_secret.json"));
        v.push(super::config_dir().join("client_secret.json"));
        v
    };
    for p in candidates {
        if p.is_file() {
            let text =
                std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            return parse_client_secret(&text).map(|s| (s, p));
        }
    }
    Err(
        "client_secret.json не найден. Как получить (5 минут, бесплатно):\n\
         1. console.cloud.google.com → создать проект\n\
         2. APIs & Services → Library → включить Gmail API и Google Drive API\n\
         3. APIs & Services → OAuth consent screen → External → добавить себя \
         в Test users\n\
         4. Credentials → Create credentials → OAuth client ID → Desktop app\n\
         5. Скачать JSON → положить в ~/.config/poler-engine/client_secret.json"
            .to_string(),
    )
}

// ---------------------------------------------------------------------------
// кодирование
// ---------------------------------------------------------------------------

/// Percent-encoding для query-параметров (RFC 3986 unreserved: -_.~).
pub fn urlencode(s: &str) -> String {
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

/// Percent-decoding (+ трактуем как пробел — форма-кодирование).
pub fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hex = bytes
                    .get(i + 1..i + 3)
                    .and_then(|h| std::str::from_utf8(h).ok())
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                match hex {
                    Some(b) => {
                        out.push(b);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Разбор query-строки `a=1&b=2` (значения percent-decoded).
pub fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

/// Случайный state (защита CSRF): 32 hex-символа.
pub fn random_state() -> String {
    random_key_16().iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// URL согласия
// ---------------------------------------------------------------------------

/// Consent URL: offline-access (refresh_token) + явный prompt
/// (Google отдаёт refresh_token только при первом согласии или
/// prompt=consent).
pub fn build_auth_url(
    client_id: &str,
    redirect_uri: &str,
    scopes: &[&str],
    state: &str,
) -> String {
    format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}\
         &access_type=offline&prompt=consent",
        auth_uri(),
        urlencode(client_id),
        urlencode(redirect_uri),
        urlencode(&scopes.join(" ")),
        urlencode(state),
    )
}

// ---------------------------------------------------------------------------
// loopback-ловушка (RFC 8252: 127.0.0.1 + случайный порт)
// ---------------------------------------------------------------------------

/// Поднятая ловушка: слушатель + redirect_uri для consent URL.
pub struct Loopback {
    pub listener: TcpListener,
    pub port: u16,
    pub redirect_uri: String,
}

/// Bind на 127.0.0.1:0 — порт выдаст ОС.
pub fn start_loopback() -> Result<Loopback, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .map_err(|e| format!("bind loopback: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("local_addr: {e}"))?
        .port();
    Ok(Loopback {
        listener,
        port,
        redirect_uri: format!("http://127.0.0.1:{port}"),
    })
}

/// Ждать редиректа Google на ловушку: `GET /?code=…&state=…`.
/// state обязан совпасть; `error=` отдаётся как ошибка. Чужие/мусорные
/// запросы получают 400 и ожидание продолжается.
pub fn wait_for_code(
    listener: TcpListener,
    expected_state: &str,
    timeout: Duration,
) -> Result<String, String> {
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("nonblocking: {e}"))?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream.set_nonblocking(false).ok();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .ok();
                let head = read_http_head(&mut stream);
                let first_line = head.lines().next().unwrap_or("").to_string();
                // "GET /?code=…&state=… HTTP/1.1"
                let query = first_line
                    .split_whitespace()
                    .nth(1)
                    .and_then(|p| p.split_once('?').map(|(_, q)| q.to_string()))
                    .unwrap_or_default();
                let params = parse_query(&query);
                let get = |k: &str| {
                    params
                        .iter()
                        .find(|(pk, _)| pk == k)
                        .map(|(_, v)| v.clone())
                };
                if let Some(err) = get("error") {
                    write_http_response(
                        &mut stream,
                        "<html><body><h3>poler-engine: Google вернул ошибку согласия</h3></body></html>",
                    );
                    return Err(format!("Google вернул ошибку: {err}"));
                }
                let code = get("code");
                let state = get("state").unwrap_or_default();
                match (code, state) {
                    (Some(code), st) if st == expected_state => {
                        write_http_response(
                            &mut stream,
                            "<html><head><meta charset=\"utf-8\"></head><body>\
                             <h3>poler-engine: код получен</h3>\
                             <p>Можно вернуться в терминал и закрыть эту вкладку.</p>\
                             </body></html>",
                        );
                        return Ok(code);
                    }
                    _ => {
                        // чужой запрос (state не совпал или нет code) — мимо
                        let _ = stream.write_all(
                            b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        );
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => return Err(format!("accept: {e}")),
        }
    }
    Err("таймаут ожидания согласия Google (180 с)".to_string())
}

// ---------------------------------------------------------------------------
// хранилище токенов
// ---------------------------------------------------------------------------

/// Сохранённые токены Google. refresh_token — постоянный (до отзыва
/// владельцем на myaccount.google.com/permissions).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct StoredTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at_unix: u64,
    pub scope: String,
}

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

impl StoredTokens {
    /// Истекает ли access_token в ближайшую минуту (и можно ли обновить).
    pub fn needs_refresh(&self) -> bool {
        self.refresh_token.is_some() && now_unix() + 60 >= self.expires_at_unix
    }
}

/// Сохранить токены по явному пути (права 0600 на unix).
pub fn save_tokens_at(path: &Path, t: &StoredTokens) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let json = serde_json::to_string_pretty(t).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("chmod 600 {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Загрузить токены по явному пути.
pub fn load_tokens_at(path: &Path) -> Result<StoredTokens, String> {
    let text = std::fs::read_to_string(path).map_err(|_| {
        format!(
            "токены Google не найдены ({}). Выполни один раз: \
             poler-engine --google-auth",
            path.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|e| format!("токены повреждены: {e}"))
}

/// Сохранить токены в стандартный путь.
pub fn save_tokens(t: &StoredTokens) -> Result<PathBuf, String> {
    let p = tokens_path();
    save_tokens_at(&p, t)?;
    Ok(p)
}

/// Загрузить токены из стандартного пути.
pub fn load_tokens() -> Result<StoredTokens, String> {
    load_tokens_at(&tokens_path())
}

// ---------------------------------------------------------------------------
// GCP-токены (cloud-platform scope, отдельный файл gcp_tokens.json)
// ---------------------------------------------------------------------------

/// Сохранить GCP-токены в `gcp_tokens.json`.
pub fn save_gcp_tokens(t: &StoredTokens) -> Result<PathBuf, String> {
    let p = gcp_tokens_path();
    save_tokens_at(&p, t)?;
    Ok(p)
}

/// Загрузить GCP-токены из `gcp_tokens.json`.
pub fn load_gcp_tokens() -> Result<StoredTokens, String> {
    let p = gcp_tokens_path();
    let text = std::fs::read_to_string(&p).map_err(|_| {
        format!(
            "GCP-токены не найдены ({}). Выполни один раз: poler-engine --gcp-auth\n\
             Это запустит OAuth с скоупом cloud-platform (нужен для NotebookLM Enterprise API).",
            p.display()
        )
    })?;
    serde_json::from_str(&text).map_err(|e| format!("GCP-токены повреждены: {e}"))
}

/// Загрузить GCP-токены и при необходимости тихо обновить (через GoogleHttp
/// с тем же client_secret). Сохраняет свежие токены обратно в gcp_tokens.json.
pub fn ensure_gcp_fresh(http: &mut GoogleHttp) -> Result<StoredTokens, String> {
    let tokens = load_gcp_tokens()?;
    if !tokens.needs_refresh() {
        return Ok(tokens);
    }
    let (secret, _) = load_client_secret()?;
    let fresh = refresh_tokens(http, &secret, &tokens)?;
    save_gcp_tokens(&fresh)?;
    Ok(fresh)
}

// ---------------------------------------------------------------------------
// обмен кода и обновление токенов (HTTPS делает google-браузер)
// ---------------------------------------------------------------------------

/// Тело POST для обмена authorization code.
pub fn build_exchange_form(secret: &ClientSecret, code: &str, redirect_uri: &str) -> String {
    format!(
        "grant_type=authorization_code&code={}&client_id={}&client_secret={}&redirect_uri={}",
        urlencode(code),
        urlencode(&secret.client_id),
        urlencode(&secret.client_secret),
        urlencode(redirect_uri),
    )
}

/// Тело POST для refresh.
pub fn build_refresh_form(secret: &ClientSecret, refresh_token: &str) -> String {
    format!(
        "grant_type=refresh_token&refresh_token={}&client_id={}&client_secret={}",
        urlencode(refresh_token),
        urlencode(&secret.client_id),
        urlencode(&secret.client_secret),
    )
}

fn token_error(status: u16, body: &str) -> String {
    let desc = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            v.get("error_description")
                .or_else(|| v.get("error"))
                .and_then(|e| e.as_str().map(String::from))
        })
        .unwrap_or_else(|| {
            let t: String = body.chars().take(200).collect();
            if t.is_empty() {
                format!("HTTP {status}")
            } else {
                t
            }
        });
    format!("token endpoint HTTP {status}: {desc}")
}

/// Токены из ответа token endpoint.
fn tokens_from_response(body: &str, expires_default: u64) -> Result<StoredTokens, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("ответ token endpoint не JSON: {e}"))?;
    let access_token = v
        .get("access_token")
        .and_then(|x| x.as_str())
        .ok_or("в ответе нет access_token")?
        .to_string();
    let expires_in = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(expires_default);
    Ok(StoredTokens {
        access_token,
        refresh_token: v.get("refresh_token").and_then(|x| x.as_str().map(String::from)),
        expires_at_unix: now_unix() + expires_in,
        scope: v
            .get("scope")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Обмен authorization code на токены.
pub fn exchange_code(
    http: &mut GoogleHttp,
    secret: &ClientSecret,
    code: &str,
    redirect_uri: &str,
) -> Result<StoredTokens, String> {
    let form = build_exchange_form(secret, code, redirect_uri);
    let (status, body) = http.post_form(&token_uri(), &form)?;
    if status != 200 {
        return Err(token_error(status, &body));
    }
    let mut t = tokens_from_response(&body, 3600)?;
    if t.refresh_token.is_none() {
        return Err(
            "Google не вернул refresh_token (повтори согласие — должно \
             появиться окно разрешения)"
                .to_string(),
        );
    }
    t.scope = if t.scope.is_empty() {
        "gmail.readonly drive.readonly (не указан Google-ом)".to_string()
    } else {
        t.scope
    };
    Ok(t)
}

/// Обновление access_token по refresh_token.
pub fn refresh_tokens(
    http: &mut GoogleHttp,
    secret: &ClientSecret,
    old: &StoredTokens,
) -> Result<StoredTokens, String> {
    let rt = old
        .refresh_token
        .clone()
        .ok_or("нет refresh_token — нужно повторить --google-auth")?;
    let form = build_refresh_form(secret, &rt);
    let (status, body) = http.post_form(&token_uri(), &form)?;
    if status != 200 {
        return Err(token_error(status, &body));
    }
    let mut t = tokens_from_response(&body, 3600)?;
    // refresh_token в ответе refresh-а обычно не приходит — сохраняем старый
    t.refresh_token = Some(rt);
    if t.scope.is_empty() {
        t.scope = old.scope.clone();
    }
    Ok(t)
}

/// Загрузить токены и при необходимости тихо обновить (и сохранить).
pub fn ensure_fresh(http: &mut GoogleHttp) -> Result<StoredTokens, String> {
    let tokens = load_tokens()?;
    if !tokens.needs_refresh() {
        return Ok(tokens);
    }
    let (secret, _) = load_client_secret()?;
    let fresh = refresh_tokens(http, &secret, &tokens)?;
    save_tokens(&fresh)?;
    Ok(fresh)
}

// ---------------------------------------------------------------------------
// полный поток авторизации (CLI --google-auth)
// ---------------------------------------------------------------------------

/// Запустить полный OAuth-поток. Возвращает сохранённые токены.
/// Печатает инструкции в stdout (URL согласия и т.п.).
pub fn run_auth(extra_scopes: &[String]) -> Result<StoredTokens, String> {
    let (secret, secret_path) = load_client_secret()?;
    let mut scopes: Vec<String> = vec![GMAIL_READONLY.to_string(), DRIVE_READONLY.to_string()];
    scopes.extend(extra_scopes.iter().cloned());
    let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

    let state = random_state();
    let lb = start_loopback()?;
    let url = build_auth_url(&secret.client_id, &lb.redirect_uri, &scope_refs, &state);

    println!("poler google-auth: OAuth 2.0 loopback (пароль остаётся в твоём браузере)");
    println!("client_secret: {}", secret_path.display());
    println!("скоупы: {}", scopes.join(" "));
    println!();
    println!("Открываю браузер для согласия Google…");
    println!("Если окно не открылось — скопируй URL вручную:");
    println!();
    println!("{url}");
    println!();
    super::open_in_user_browser(&url);

    let code = wait_for_code(lb.listener, &state, Duration::from_secs(180))?;

    // HTTPS-обмен делает google-браузер движка (TLS бесплатно)
    let mut http = GoogleHttp::connect(super::google_cdp_port())?;
    let tokens = exchange_code(&mut http, &secret, &code, &lb.redirect_uri)?;
    let path = save_tokens(&tokens)?;
    println!("Токены сохранены: {} (права 0600)", path.display());
    println!(
        "Отзыв в любой момент: https://myaccount.google.com/permissions \
         (приложение «{}»)",
        secret.client_id.split('-').next().unwrap_or("poler-engine")
    );
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// GCP-авторизация (CLI --gcp-auth) — cloud-platform scope для Companion Bridge
// ---------------------------------------------------------------------------

/// Запустить OAuth-поток для GCP (скоуп `cloud-platform`).
///
/// Токены сохраняются в `gcp_tokens.json` отдельно от `google_tokens.json`:
/// так пользователь может отозвать GCP-доступ, не затрагивая Gmail/Drive.
///
/// **Важно:** Google Cloud Console → OAuth consent screen → Test users
/// должен содержать твой email. Иначе будет 403 access_denied.
pub fn run_gcp_auth() -> Result<StoredTokens, String> {
    let (secret, secret_path) = load_client_secret()?;
    let scopes: Vec<String> = vec![CLOUD_PLATFORM.to_string()];
    let scope_refs: Vec<&str> = scopes.iter().map(|s| s.as_str()).collect();

    let state = random_state();
    let lb = start_loopback()?;
    let url = build_auth_url(&secret.client_id, &lb.redirect_uri, &scope_refs, &state);

    println!("poler gcp-auth: OAuth 2.0 loopback для Companion Bridge (NotebookLM Enterprise API)");
    println!("client_secret: {}", secret_path.display());
    println!("скоупы: {}", scopes.join(" "));
    println!();
    println!("Этот скоуп даёт доступ к Discovery Engine API (NotebookLM Enterprise).");
    println!("Токены сохраняются отдельно в gcp_tokens.json — отозвать можно будет");
    println!("независимо от Gmail/Drive-доступа на myaccount.google.com/permissions.");
    println!();
    println!("Открываю браузер для согласия Google…");
    println!("Если окно не открылось — скопируй URL вручную:");
    println!();
    println!("{url}");
    println!();
    super::open_in_user_browser(&url);

    let code = wait_for_code(lb.listener, &state, Duration::from_secs(180))?;

    // HTTPS-обмен делает google-браузер движка (TLS бесплатно)
    let mut http = GoogleHttp::connect(super::google_cdp_port())?;
    let tokens = exchange_code(&mut http, &secret, &code, &lb.redirect_uri)?;
    let path = save_gcp_tokens(&tokens)?;
    println!("GCP-токены сохранены: {} (права 0600)", path.display());
    println!(
        "Отзыв в любой момент: https://myaccount.google.com/permissions \
         (приложение «{}»)",
        secret.client_id.split('-').next().unwrap_or("poler-engine")
    );
    println!();
    println!("Теперь доступны:");
    println!("  poler-engine --gcp-status");
    println!("  poler-engine --nlm-upload <NB_ID> <PATH>   # POST sources:uploadFile");
    println!("  poler-engine --nlm-aoview <NB_ID>          # POST audioOverviews");
    println!("  poler-engine --nlm-aodel <NB_ID>           # DELETE audioOverviews/default");
    Ok(tokens)
}

// ---------------------------------------------------------------------------
// ЖИВЫЕ тесты против РЕАЛЬНЫХ эндпоинтов Google (запуск вручную):
//   cargo test --lib google::oauth::tests::live_ -- --ignored --nocapture
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_client_secret_installed_and_web() {
        let installed = r#"{"installed":{"client_id":"abc.apps.googleusercontent.com",
            "project_id":"x","auth_uri":"https://accounts.google.com/o/oauth2/v2/auth",
            "token_uri":"https://oauth2.googleapis.com/token",
            "client_secret":"SECRET","redirect_uris":["http://localhost"]}}"#;
        let s = parse_client_secret(installed).unwrap();
        assert_eq!(s.client_id, "abc.apps.googleusercontent.com");
        assert_eq!(s.client_secret, "SECRET");

        let web = r#"{"web":{"client_id":"w.apps.googleusercontent.com","client_secret":"W"}}"#;
        assert_eq!(parse_client_secret(web).unwrap().client_id, "w.apps.googleusercontent.com");

        assert!(parse_client_secret("{}").is_err());
        assert!(parse_client_secret("не json").is_err());
    }

    #[test]
    fn urlencode_rfc3986() {
        assert_eq!(urlencode("abcXYZ09-_.~"), "abcXYZ09-_.~");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a+b"), "a%2Bb");
        assert_eq!(urlencode("укр"), "%D1%83%D0%BA%D1%80");
        assert_eq!(urlencode("a&b=c"), "a%26b%3Dc");
    }

    #[test]
    fn percent_decode_roundtrip() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%D1%83%D0%BA%D1%80"), "укр");
        assert_eq!(percent_decode("broken%2"), "broken%2"); // битый hex не ломает
        assert_eq!(percent_decode(&urlencode("код 42+")), "код 42+");
    }

    #[test]
    fn parse_query_pairs() {
        let q = parse_query("code=4%2F0Ax&state=abc&error=");
        assert_eq!(q[0], ("code".to_string(), "4/0Ax".to_string()));
        assert_eq!(q[1], ("state".to_string(), "abc".to_string()));
        assert_eq!(q[2], ("error".to_string(), "".to_string()));
    }

    #[test]
    fn auth_url_contains_all_params() {
        let url = build_auth_url(
            "cid.apps.googleusercontent.com",
            "http://127.0.0.1:54321",
            &[GMAIL_READONLY, DRIVE_READONLY],
            "st4te",
        );
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(url.contains("client_id=cid.apps.googleusercontent.com"));
        assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A54321"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fgmail.readonly"));
        assert!(url.contains("state=st4te"));
        assert!(url.contains("access_type=offline"));
        assert!(url.contains("prompt=consent"));
    }

    #[test]
    fn state_is_32_hex_and_random() {
        let a = random_state();
        let b = random_state();
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn loopback_catches_code_with_matching_state() {
        let lb = start_loopback().unwrap();
        let port = lb.port;
        let listener = lb.listener;
        let handle = std::thread::spawn(move || {
            // «Google»: редирект браузера на ловушку
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            use std::io::{Read, Write};
            s.write_all(b"GET /?code=4%2F0FAKE&state=st123 HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            String::from_utf8_lossy(&buf[..n]).contains("код получен")
        });
        let code = wait_for_code(listener, "st123", Duration::from_secs(5)).unwrap();
        assert!(handle.join().unwrap(), "ловушка ответила HTML успеха");
        assert_eq!(code, "4/0FAKE");
    }

    #[test]
    fn loopback_rejects_wrong_state() {
        let lb = start_loopback().unwrap();
        let port = lb.port;
        let handle = std::thread::spawn(move || {
            let mut s = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
            use std::io::{Read, Write};
            s.write_all(b"GET /?code=X&state=WRONG HTTP/1.1\r\nHost: x\r\n\r\n")
                .unwrap();
            let mut buf = [0u8; 1024];
            let n = s.read(&mut buf).unwrap_or(0);
            String::from_utf8_lossy(&buf[..n]).contains("400 Bad Request")
        });
        // с коротким таймаутом: чужой state не должен пройти
        let res = wait_for_code(lb.listener, "RIGHT", Duration::from_millis(1500));
        assert!(handle.join().unwrap(), "на чужой state — 400");
        assert!(res.is_err(), "чужой state отброшен");
    }

    #[test]
    fn tokens_roundtrip_and_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("google_tokens.json");
        let t = StoredTokens {
            access_token: "at".into(),
            refresh_token: Some("rt".into()),
            expires_at_unix: now_unix() + 3600,
            scope: "gmail.readonly".into(),
        };
        save_tokens_at(&p, &t).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "файл токенов доступен только владельцу");
        }
        assert_eq!(load_tokens_at(&p).unwrap(), t);
    }

    #[test]
    fn needs_refresh_logic() {
        let fresh = StoredTokens {
            access_token: "a".into(),
            refresh_token: Some("r".into()),
            expires_at_unix: now_unix() + 3600,
            scope: String::new(),
        };
        assert!(!fresh.needs_refresh());
        let expiring = StoredTokens {
            expires_at_unix: now_unix() + 30,
            ..fresh.clone()
        };
        assert!(expiring.needs_refresh());
        let no_refresh = StoredTokens {
            refresh_token: None,
            expires_at_unix: now_unix() - 10,
            ..fresh
        };
        assert!(!no_refresh.needs_refresh(), "без refresh_token не обновляемся");
    }

    #[test]
    fn exchange_form_contents() {
        let secret = ClientSecret {
            client_id: "cid/x".into(),
            client_secret: "s&c".into(),
        };
        let form = build_exchange_form(&secret, "4/0A code", "http://127.0.0.1:1");
        assert!(form.starts_with("grant_type=authorization_code&code=4%2F0A%20code&"));
        assert!(form.contains("client_id=cid%2Fx&client_secret=s%26c"));
        assert!(form.ends_with("&redirect_uri=http%3A%2F%2F127.0.0.1%3A1"));
        assert_eq!(form.matches('&').count(), 4);

        let rf = build_refresh_form(&secret, "rtok");
        assert!(rf.starts_with("grant_type=refresh_token&refresh_token=rtok&"));
    }

    #[test]
    fn tokens_from_response_parses() {
        let body = r#"{"access_token":"ya29.a","expires_in":3599,
            "refresh_token":"1//rt","scope":"gmail.readonly","token_type":"Bearer"}"#;
        let t = tokens_from_response(body, 3600).unwrap();
        assert_eq!(t.access_token, "ya29.a");
        assert_eq!(t.refresh_token.as_deref(), Some("1//rt"));
        assert_eq!(t.scope, "gmail.readonly");
        assert!(t.expires_at_unix > now_unix() + 3000);

        assert!(tokens_from_response("{}", 3600).is_err());
        assert!(tokens_from_response("не json", 3600).is_err());
    }

    /// ЖИВОЙ тест: реальный token endpoint Google отклоняет мусорный код
    /// (проверяет TLS-мост CDP→fetch→HTTPS целиком).
    #[test]
    #[ignore = "живой тест: поднимает google-браузер и ходит в Google"]
    fn live_google_token_endpoint_rejects_bogus_exchange() {
        let mut http = GoogleHttp::connect(super::super::google_cdp_port()).unwrap();
        let secret = ClientSecret {
            client_id: "fake.apps.googleusercontent.com".into(),
            client_secret: "fake".into(),
        };
        let form = build_exchange_form(&secret, "bogus-code", "http://127.0.0.1:1");
        let (status, body) = http.post_form(&token_uri(), &form).unwrap();
        // Google: 401 invalid_client (фейковый client_id) или 400 invalid_grant
        assert!(
            status == 400 || status == 401,
            "Google обязан отклонить мусорный обмен: {status} {body}"
        );
        assert!(body.contains("invalid"), "тело ошибки: {body}");
    }
}
