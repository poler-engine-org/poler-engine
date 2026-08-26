//! Gmail API + Google Drive API поверх Bearer-токена из [`super::oauth`].
//!
//! Всё чтение, никаких записей: скоупы по умолчанию `*.readonly`.
//! HTTP делает google-браузер движка ([`GoogleHttp`]) — TLS, редиректы,
//! сжатие и rate-limit-поведение Chromium-а бесплатно.
//!
//! Gmail-запрос — нативный синтаксис Gmail:
//! `from:vasya has:attachment newer_than:7d «отчёт»` и т.п.

use serde::Serialize;

use super::oauth::{ensure_fresh, load_tokens, tokeninfo_uri};
use super::GoogleHttp;

/// База Gmail API (POLER_GOOGLE_GMAIL_API для тестов).
pub fn gmail_api_base() -> String {
    std::env::var("POLER_GOOGLE_GMAIL_API")
        .unwrap_or_else(|_| "https://gmail.googleapis.com/gmail/v1".to_string())
}

/// База Drive API (POLER_GOOGLE_DRIVE_API).
pub fn drive_api_base() -> String {
    std::env::var("POLER_GOOGLE_DRIVE_API")
        .unwrap_or_else(|_| "https://www.googleapis.com/drive/v3".to_string())
}

/// Заголовок авторизации для API-вызовов.
fn bearer_headers(token: &str) -> Vec<(&'static str, String)> {
    vec![("Authorization", format!("Bearer {token}"))]
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Урезанный набор символов для однострочного вывода.
fn one_line(s: &str, max: usize) -> String {
    let t: String = s
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let squashed = split_ws(&t);
    truncate(squashed.trim(), max)
}

/// Схлопнуть пробельные последовательности в один пробел.
fn split_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
            }
            prev_ws = true;
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Gmail
// ---------------------------------------------------------------------------

/// Письмо из выдачи Gmail API (id + метаданные + сниппет).
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct MailHit {
    pub id: String,
    pub thread_id: String,
    pub subject: String,
    pub from: String,
    pub date: String,
    pub snippet: String,
}

/// URL списка сообщений Gmail.
pub fn gmail_list_url(query: &str, max: usize) -> String {
    let mut url = format!(
        "{}/users/me/messages?maxResults={max}",
        gmail_api_base()
    );
    if !query.trim().is_empty() {
        url.push_str(&format!("&q={}", super::oauth::urlencode(query)));
    }
    url
}

/// URL метаданных одного сообщения.
pub fn gmail_message_url(id: &str) -> String {
    format!(
        "{}/users/me/messages/{}?format=metadata\
         &metadataHeaders=Subject&metadataHeaders=From&metadataHeaders=Date",
        gmail_api_base(),
        super::oauth::urlencode(id),
    )
}

/// Разбор сообщения Gmail API (format=metadata) в MailHit.
pub fn parse_gmail_message(v: &serde_json::Value) -> Option<MailHit> {
    let id = v.get("id")?.as_str()?.to_string();
    let thread_id = v
        .get("threadId")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();
    let snippet = v
        .get("snippet")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let mut subject = String::new();
    let mut from = String::new();
    let mut date = String::new();
    if let Some(headers) = v
        .get("payload")
        .and_then(|p| p.get("headers"))
        .and_then(|h| h.as_array())
    {
        for h in headers {
            let name = h.get("name").and_then(|n| n.as_str()).unwrap_or("");
            let value = h.get("value").and_then(|x| x.as_str()).unwrap_or("");
            match name.to_ascii_lowercase().as_str() {
                "subject" => subject = value.to_string(),
                "from" => from = value.to_string(),
                "date" => date = value.to_string(),
                _ => {}
            }
        }
    }
    Some(MailHit {
        id,
        thread_id,
        subject,
        from,
        date,
        snippet,
    })
}

/// Поиск в Gmail владельца. Пустой запрос — недавняя почта.
/// Требует одноразового `--google-auth`.
pub fn gmail_search(query: &str, max: usize) -> Result<Vec<MailHit>, String> {
    let mut http = GoogleHttp::connect(crate::google::google_cdp_port())?;
    let tokens = ensure_fresh(&mut http)?;
    let auth = bearer_headers(&tokens.access_token);

    let (status, body) = http.get(&gmail_list_url(query, max), &auth)?;
    if status != 200 {
        return Err(format!(
            "Gmail API HTTP {status}: {}. Токен отозван? Повтори --google-auth",
            truncate(&body, 300)
        ));
    }
    let list: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("ответ Gmail не JSON: {e}"))?;
    let ids: Vec<String> = list
        .get("messages")
        .and_then(|m| m.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut hits = Vec::with_capacity(ids.len().min(max));
    for id in ids.iter().take(max) {
        let (st, b) = http.get(&gmail_message_url(id), &auth)?;
        if st != 200 {
            continue; // письмо могло удалиться между list и get
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&b) {
            if let Some(hit) = parse_gmail_message(&v) {
                hits.push(hit);
            }
        }
    }
    Ok(hits)
}

/// Текстовое представление для CLI/MCP.
pub fn format_mail_hits(hits: &[MailHit], query: &str) -> String {
    let mut out = format!(
        "poler gmail «{query}»: {} писем\n\n",
        hits.len()
    );
    for (i, h) in hits.iter().enumerate() {
        out.push_str(&format!(
            "{}. {}\n   от: {} | дата: {}\n   id: {} | поток: {}\n   {}\n\n",
            i + 1,
            if h.subject.is_empty() { "(без темы)" } else { &h.subject },
            one_line(&h.from, 90),
            one_line(&h.date, 40),
            h.id,
            h.thread_id,
            one_line(&h.snippet, 200),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Drive
// ---------------------------------------------------------------------------

/// Файл из выдачи Drive API.
#[derive(Serialize, Clone, Debug, PartialEq, Eq)]
pub struct DriveHit {
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub modified_time: String,
    pub size_bytes: Option<u64>,
    pub web_view_link: Option<String>,
}

/// Drive q-запрос по имени: `name contains '…'` (кавычки экранируются).
pub fn drive_query(query: &str) -> String {
    let escaped = query.replace('\\', "\\\\").replace('\'', "\\'");
    format!("name contains '{escaped}'")
}

/// URL списка файлов Drive.
pub fn drive_list_url(query: &str, max: usize) -> String {
    let fields = "files(id,name,mimeType,modifiedTime,size,webViewLink)";
    let mut url = format!(
        "{}/files?pageSize={max}&orderBy={}&fields={}",
        drive_api_base(),
        super::oauth::urlencode("modifiedTime desc"),
        super::oauth::urlencode(fields),
    );
    if !query.trim().is_empty() {
        url.push_str(&format!(
            "&q={}",
            super::oauth::urlencode(&drive_query(query))
        ));
    }
    url
}

/// Разбор файла из ответа Drive.
pub fn parse_drive_file(v: &serde_json::Value) -> Option<DriveHit> {
    let id = v.get("id")?.as_str()?.to_string();
    Some(DriveHit {
        id,
        name: v
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string(),
        mime_type: v
            .get("mimeType")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
        modified_time: v
            .get("modifiedTime")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
        size_bytes: v.get("size").and_then(|s| s.as_str()).and_then(|s| s.parse().ok()),
        web_view_link: v
            .get("webViewLink")
            .and_then(|l| l.as_str())
            .map(String::from),
    })
}

/// Файлы Google Drive по имени (пустой запрос — недавние).
pub fn drive_list(query: &str, max: usize) -> Result<Vec<DriveHit>, String> {
    let mut http = GoogleHttp::connect(crate::google::google_cdp_port())?;
    let tokens = ensure_fresh(&mut http)?;
    let auth = bearer_headers(&tokens.access_token);

    let (status, body) = http.get(&drive_list_url(query, max), &auth)?;
    if status != 200 {
        return Err(format!(
            "Drive API HTTP {status}: {}. Токен отозван? Повтори --google-auth",
            truncate(&body, 300)
        ));
    }
    let v: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("ответ Drive не JSON: {e}"))?;
    Ok(v
        .get("files")
        .and_then(|f| f.as_array())
        .map(|arr| arr.iter().filter_map(parse_drive_file).collect())
        .unwrap_or_default())
}

/// Текстовое представление для CLI/MCP.
pub fn format_drive_hits(hits: &[DriveHit], query: &str) -> String {
    let mut out = format!(
        "poler drive «{query}»: {} файлов\n\n",
        hits.len()
    );
    for (i, h) in hits.iter().enumerate() {
        let size = h
            .size_bytes
            .map(|b| format!("{:.1} КБ", b as f64 / 1024.0))
            .unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "{}. {} [{}]\n   изменён: {} | размер: {} | id: {}\n   {}\n\n",
            i + 1,
            h.name,
            h.mime_type,
            one_line(&h.modified_time, 25),
            size,
            h.id,
            h.web_view_link.as_deref().unwrap_or(""),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// статус токенов (--google-status)
// ---------------------------------------------------------------------------

/// Состояние Google-интеграции: локальные токены + (если браузер доступен)
/// tokeninfo Google (email, точный exp).
pub fn status() -> Result<serde_json::Value, String> {
    let tokens = load_tokens()?;
    let mut st = serde_json::json!({
        "authorized": true,
        "scope": tokens.scope,
        "expires_at_unix": tokens.expires_at_unix,
        "refreshable": tokens.refresh_token.is_some(),
        "expires_in_secs": tokens.expires_at_unix.saturating_sub(super::oauth::now_unix()),
        "tokens_file": super::tokens_path().display().to_string(),
    });
    // tokeninfo — через google-браузер (может не подняться — деградируем)
    if let Ok(mut http) = GoogleHttp::connect(crate::google::google_cdp_port()) {
        let url = format!(
            "{}?access_token={}",
            tokeninfo_uri(),
            super::oauth::urlencode(&tokens.access_token)
        );
        if let Ok((200, body)) = http.get(&url, &[]) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
                if let Some(email) = v.get("email").and_then(|e| e.as_str()) {
                    st["account"] = serde_json::json!(email);
                }
                if let Some(exp) = v.get("exp").and_then(|e| e.as_i64()) {
                    st["expires_at_unix"] = serde_json::json!(exp);
                }
            }
        } else {
            st["tokeninfo"] = "недоступен (токен истёк или отозван)".into();
        }
    }
    Ok(st)
}

// ---------------------------------------------------------------------------
// ЖИВЫЕ тесты против РЕАЛЬНЫХ эндпоинтов Google (запуск вручную):
//   cargo test --lib google::api::tests::live_ -- --ignored --nocapture
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gmail_list_url_with_and_without_query() {
        let base = gmail_api_base();
        assert_eq!(
            gmail_list_url("", 10),
            format!("{base}/users/me/messages?maxResults=10")
        );
        assert_eq!(
            gmail_list_url("from:vasya отчёт", 5),
            format!(
                "{base}/users/me/messages?maxResults=5&q=from%3Avasya%20%D0%BE%D1%82%D1%87%D1%91%D1%82"
            )
        );
    }

    #[test]
    fn gmail_message_url_metadata_headers() {
        let url = gmail_message_url("abc123");
        assert!(url.contains("/users/me/messages/abc123?format=metadata"));
        assert!(url.contains("metadataHeaders=Subject"));
        assert!(url.contains("metadataHeaders=From"));
        assert!(url.contains("metadataHeaders=Date"));
    }

    #[test]
    fn parse_gmail_message_extracts_headers_and_snippet() {
        let v: serde_json::Value = serde_json::json!({
            "id": "18c2f",
            "threadId": "18c2f",
            "snippet": "Привет, во вложении отчёт за март",
            "payload": {
                "headers": [
                    {"name": "Subject", "value": "Отчёт за март"},
                    {"name": "From", "value": "vasya@example.com"},
                    {"name": "Date", "value": "Mon, 1 Apr 2024 10:00:00 +0300"},
                    {"name": "To", "value": "me@example.com"}
                ]
            }
        });
        let h = parse_gmail_message(&v).unwrap();
        assert_eq!(h.subject, "Отчёт за март");
        assert_eq!(h.from, "vasya@example.com");
        assert_eq!(h.date, "Mon, 1 Apr 2024 10:00:00 +0300");
        assert!(h.snippet.contains("отчёт"));
        assert_eq!(h.id, "18c2f");

        assert!(parse_gmail_message(&serde_json::json!({"threadId": "x"})).is_none());
    }

    #[test]
    fn format_mail_hits_renders_lines() {
        let hits = vec![MailHit {
            id: "i1".into(),
            thread_id: "t1".into(),
            subject: "Тема".into(),
            from: "a@b.c".into(),
            date: "Mon".into(),
            snippet: "сниппет\nв две строки".into(),
        }];
        let text = format_mail_hits(&hits, "q");
        assert!(text.contains("poler gmail «q»: 1 писем"));
        assert!(text.contains("1. Тема"));
        assert!(text.contains("от: a@b.c"));
        // многострочный сниппет сворачивается в одну строку вывода
        assert!(text.contains("сниппет в две строки"));
        assert!(!text.contains("сниппет\nв"));
    }

    #[test]
    fn drive_query_escapes_quotes() {
        assert_eq!(drive_query("план"), "name contains 'план'");
        assert_eq!(drive_query("o'brien"), "name contains 'o\\'brien'");
        assert_eq!(drive_query("a\\b"), "name contains 'a\\\\b'");
    }

    #[test]
    fn drive_list_url_fields_and_order() {
        let url = drive_list_url("док", 7);
        assert!(url.starts_with(&format!("{}/files?", drive_api_base())));
        assert!(url.contains("pageSize=7"));
        assert!(url.contains("orderBy=modifiedTime%20desc"));
        assert!(url.contains("fields=files%28id%2Cname%2CmimeType%2CmodifiedTime%2Csize%2CwebViewLink%29"));
        assert!(url.contains("&q=name%20contains%20%27%D0%B4%D0%BE%D0%BA%27"));
        // без запроса — без q
        assert!(!drive_list_url("", 7).contains("&q="));
    }

    #[test]
    fn parse_drive_file_full_and_partial() {
        let v: serde_json::Value = serde_json::json!({
            "id": "1AbC",
            "name": "Касіопея.md",
            "mimeType": "text/markdown",
            "modifiedTime": "2026-08-01T12:00:00Z",
            "size": "4096",
            "webViewLink": "https://drive.google.com/file/d/1AbC/view"
        });
        let h = parse_drive_file(&v).unwrap();
        assert_eq!(h.name, "Касіопея.md");
        assert_eq!(h.size_bytes, Some(4096));
        assert_eq!(h.web_view_link.as_deref(), Some("https://drive.google.com/file/d/1AbC/view"));

        let partial: serde_json::Value =
            serde_json::json!({"id": "x", "name": "минимум"});
        let h2 = parse_drive_file(&partial).unwrap();
        assert_eq!(h2.size_bytes, None);
        assert_eq!(h2.web_view_link, None);
        assert!(parse_drive_file(&serde_json::json!({"name": "no id"})).is_none());
    }

    #[test]
    fn format_drive_hits_renders() {
        let hits = vec![DriveHit {
            id: "d1".into(),
            name: "file.md".into(),
            mime_type: "text/markdown".into(),
            modified_time: "2026-08-01T12:00:00Z".into(),
            size_bytes: Some(2048),
            web_view_link: Some("https://drive.google.com/x".into()),
        }];
        let text = format_drive_hits(&hits, "file");
        assert!(text.contains("poler drive «file»: 1 файлов"));
        assert!(text.contains("file.md [text/markdown]"));
        assert!(text.contains("2.0 КБ"));
    }

    /// ЖИВОЙ тест: Gmail API отклоняет фейковый Bearer (мост до real Google).
    #[test]
    #[ignore = "живой тест: поднимает google-браузер и ходит в Google"]
    fn live_gmail_api_rejects_fake_bearer() {
        let mut http = GoogleHttp::connect(crate::google::google_cdp_port()).unwrap();
        let auth = bearer_headers("faketoken");
        let (status, body) = http
            .get(
                &format!("{}/users/me/messages?maxResults=1", gmail_api_base()),
                &auth,
            )
            .unwrap();
        assert_eq!(status, 401, "Gmail обязан требовать валидный токен: {body}");
        assert!(body.contains("error") || body.to_lowercase().contains("unauthorized"));
    }

    /// ЖИВОЙ тест: Drive API отклоняет фейковый Bearer.
    #[test]
    #[ignore = "живой тест: поднимает google-браузер и ходит в Google"]
    fn live_drive_api_rejects_fake_bearer() {
        let mut http = GoogleHttp::connect(crate::google::google_cdp_port()).unwrap();
        let auth = bearer_headers("faketoken");
        let (status, body) = http
            .get(&format!("{}/files?pageSize=1", drive_api_base()), &auth)
            .unwrap();
        assert_eq!(status, 401, "Drive обязан требовать валидный токен: {body}");
    }
}
