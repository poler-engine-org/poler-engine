//! Нативный CDP-клиент (Chrome DevTools Protocol) на чистом std.
//!
//! Замена «костыльной» связке Rust → Node.js → CLI → Chromium из
//! super-z-skills (skills/agent-browser): без npm, без spawn подпроцессов,
//! прямой WebSocket к отрендеренному Chromium.
//!
//! Реализовано:
//! * WebSocket-клиент RFC 6455 поверх `TcpStream` (handshake,
//!   маскированные клиентские фреймы, приём серверных);
//! * HTTP `GET /json` — discovery таргетов;
//! * CDP: `Page.enable`/`Network.enable`/`Page.navigate`/
//!   `Runtime.evaluate` (рендер-текст страницы, Shadow DOM включён —
//!   браузер исполняет весь JS);
//! * Перехват сетевых ответов JSON API (`Network.responseReceived` +
//!   `Network.getResponseBody`) — сырые данные до превращения в HTML.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// Максимальный размер одного WebSocket-сообщения (текст страницы).
const MAX_WS_MESSAGE: usize = 64 * 1024 * 1024;

// ---------------------------------------------------------------------------
// base64 (только для Sec-WebSocket-Key — 16 байт)
// ---------------------------------------------------------------------------

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 16 случайных байт из /dev/urandom (или fallback на время).
fn random_key_16() -> [u8; 16] {
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    } else {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        for (i, b) in buf.iter_mut().enumerate() {
            *b = ((t >> (i * 4)) & 0xFF) as u8 ^ (i as u8).wrapping_mul(31);
        }
    }
    buf
}

// ---------------------------------------------------------------------------
// WebSocket-клиент (минимальный, текстовые фреймы)
// ---------------------------------------------------------------------------

struct WsClient {
    stream: TcpStream,
}

impl WsClient {
    /// Подключение к `ws://host:port/path` с WS-handshake.
    fn connect(host: &str, port: u16, path: &str) -> Result<Self, String> {
        let mut stream = TcpStream::connect((host, port)).map_err(|e| format!("tcp: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .ok();
        stream
            .set_write_timeout(Some(Duration::from_secs(10)))
            .ok();
        stream.set_nodelay(true).ok();

        let key = base64_encode(&random_key_16());
        let req = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("handshake write: {e}"))?;

        // читать до \r\n\r\n
        let mut buf = Vec::with_capacity(512);
        let mut byte = [0u8; 1];
        loop {
            stream
                .read_exact(&mut byte)
                .map_err(|e| format!("handshake read: {e}"))?;
            buf.push(byte[0]);
            if buf.ends_with(b"\r\n\r\n") {
                break;
            }
            if buf.len() > 4096 {
                return Err("handshake: слишком длинный ответ".into());
            }
        }
        let head = String::from_utf8_lossy(&buf);
        if !head.starts_with("HTTP/1.1 101") {
            return Err(format!(
                "handshake отклонён: {}",
                head.lines().next().unwrap_or("")
            ));
        }
        Ok(Self { stream })
    }

    /// Отправка текстового фрейма (клиентские фреймы маскируются).
    fn send_text(&mut self, payload: &str) -> Result<(), String> {
        let data = payload.as_bytes();
        let mask: [u8; 4] = random_key_16()[..4].try_into().unwrap();
        let mut frame = Vec::with_capacity(data.len() + 14);
        frame.push(0x81); // FIN + text
        let len = data.len();
        if len < 126 {
            frame.push(0x80 | len as u8);
        } else if len <= 0xFFFF {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        frame.extend_from_slice(&mask);
        for (i, b) in data.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        self.stream
            .write_all(&frame)
            .map_err(|e| format!("ws send: {e}"))
    }

    /// Приём одного текстового сообщения (склейка фрагментов,
    /// ping -> pong, pong/close игнорируются).
    fn recv_text(&mut self) -> Result<String, String> {
        let mut message: Vec<u8> = Vec::new();
        loop {
            let (opcode, fin, payload) = self.recv_frame()?;
            match opcode {
                0x9 => {
                    // ping -> pong
                    let mask: [u8; 4] = random_key_16()[..4].try_into().unwrap();
                    let mut pong = vec![0x8A, 0x80 | payload.len() as u8];
                    pong.extend_from_slice(&mask);
                    for (i, b) in payload.iter().enumerate() {
                        pong.push(b ^ mask[i % 4]);
                    }
                    let _ = self.stream.write_all(&pong);
                    continue;
                }
                0xA => continue, // pong
                0x8 => return Err("ws closed".into()),
                _ => {}
            }
            if message.len() + payload.len() > MAX_WS_MESSAGE {
                return Err("ws: сообщение превышает лимит".into());
            }
            message.extend_from_slice(&payload);
            if fin {
                return String::from_utf8(message)
                    .map_err(|_| "ws: невалидный UTF-8".to_string());
            }
        }
    }

    /// Приём одного фрейма: (opcode, fin, payload).
    fn recv_frame(&mut self) -> Result<(u8, bool, Vec<u8>), String> {
        let mut hdr = [0u8; 2];
        self.stream
            .read_exact(&mut hdr)
            .map_err(|e| format!("ws frame hdr: {e}"))?;
        let fin = hdr[0] & 0x80 != 0;
        let opcode = hdr[0] & 0x0F;
        let masked = hdr[1] & 0x80 != 0;
        let len = hdr[1] & 0x7F;
        let len = match len {
            126 => {
                let mut b = [0u8; 2];
                self.stream.read_exact(&mut b).map_err(|e| e.to_string())?;
                u16::from_be_bytes(b) as usize
            }
            127 => {
                let mut b = [0u8; 8];
                self.stream.read_exact(&mut b).map_err(|e| e.to_string())?;
                u64::from_be_bytes(b) as usize
            }
            n => n as usize,
        };
        let mask = if masked {
            let mut m = [0u8; 4];
            self.stream.read_exact(&mut m).map_err(|e| e.to_string())?;
            Some(m)
        } else {
            None
        };
        if len > MAX_WS_MESSAGE {
            return Err("ws: фрейм превышает лимит".into());
        }
        let mut payload = vec![0u8; len];
        self.stream
            .read_exact(&mut payload)
            .map_err(|e| format!("ws frame payload: {e}"))?;
        if let Some(m) = mask {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= m[i % 4];
            }
        }
        Ok((opcode, fin, payload))
    }
}

// ---------------------------------------------------------------------------
// CDP-клиент
// ---------------------------------------------------------------------------

/// HTTP GET к DevTools HTTP-эндпоинту (например, `/json`).
/// Читает тело по Content-Length (read_to_end ломается на keep-alive
/// соединениях Chromium с EAGAIN).
fn http_get(host: &str, port: u16, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect((host, port)).map_err(|e| format!("tcp: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .ok();
    let req =
        format!("GET {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("http write: {e}"))?;

    // заголовки до \r\n\r\n
    let mut head = Vec::with_capacity(256);
    let mut b = [0u8; 1];
    loop {
        stream
            .read_exact(&mut b)
            .map_err(|e| format!("http head: {e}"))?;
        head.push(b[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 8192 {
            return Err("http: гигантские заголовки".into());
        }
    }
    let head_str = String::from_utf8_lossy(&head).to_string();
    let content_len: usize = head_str
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(0);

    // тело: Content-Length байт (или до EOF при chunked/отсутствии)
    let mut body = vec![0u8; content_len];
    if content_len > 0 {
        stream
            .read_exact(&mut body)
            .map_err(|e| format!("http body: {e}"))?;
    } else {
        let mut buf = Vec::new();
        // короткое окно: DevTools /json всегда отдаёт Content-Length
        let _ = stream.read_to_end(&mut buf);
        body = buf;
    }
    Ok(String::from_utf8_lossy(&body).to_string())
}

/// Живое соединение к Chromium CDP.
pub struct CdpSession {
    ws: WsClient,
    next_id: u64,
}

/// Результат загрузки страницы: рендер-текст + перехваченный JSON.
pub struct WebPage {
    pub url: String,
    pub text: String,
    pub json_responses: Vec<(String, String)>, // (url, body)
}

impl CdpSession {
    /// Подключение к первому page-таргету Chromium на порту CDP.
    pub fn connect(port: u16) -> Result<Self, String> {
        let list = http_get("127.0.0.1", port, "/json")?;
        let targets: serde_json::Value =
            serde_json::from_str(&list).map_err(|e| format!("json /json: {e}"))?;
        let ws_url = targets
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|t| t.get("type").and_then(|v| v.as_str()) == Some("page"))
                    .and_then(|t| t.get("webSocketDebuggerUrl"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .ok_or("не найден page-таргет (Chromium запущен с --remote-debugging-port?)")?;

        // ws://127.0.0.1:PORT/devtools/page/ID
        let path = ws_url
            .split_once("ws://")
            .and_then(|(_, rest)| rest.find('/').map(|i| rest[i..].to_string()))
            .unwrap_or_else(|| "/devtools/page/0".to_string());
        let ws = WsClient::connect("127.0.0.1", port, &path)?;
        let mut s = Self { ws, next_id: 1 };
        s.command("Page.enable", "{}")?;
        s.command("Network.enable", "{}")?;
        Ok(s)
    }

    /// Отправка CDP-команды; события по пути складываются в `events`
    /// (нужны для сетевого перехвата).
    fn command_collect(
        &mut self,
        method: &str,
        params: &str,
        events: &mut Vec<serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        let msg = format!(r#"{{"id":{id},"method":"{method}","params":{params}}}"#);
        self.ws.send_text(&msg)?;
        loop {
            let text = self.ws.recv_text()?;
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("cdp json: {e}"))?;
            if v.get("id").and_then(|i| i.as_u64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    return Err(format!("cdp error: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(serde_json::Value::Null));
            }
            if v.get("method").is_some() {
                events.push(v);
            }
        }
    }

    fn command(&mut self, method: &str, params: &str) -> Result<serde_json::Value, String> {
        let mut ev = Vec::new();
        self.command_collect(method, params, &mut ev)
    }

    /// Загрузка страницы и извлечение рендер-текста.
    ///
    /// * `wait_ms` — пауза после load на дочерние fetch/XHR
    ///   (React/Vue догружают контент после onload).
    pub fn load_page(&mut self, url: &str, wait_ms: u64) -> Result<WebPage, String> {
        let mut events: Vec<serde_json::Value> = Vec::new();
        self.command_collect(
            "Page.navigate",
            &format!(r#"{{"url":"{url}"}}"#),
            &mut events,
        )?;

        // ждать Page.loadEventFired до 20 с (неблокирующе, собирая события)
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while std::time::Instant::now() < deadline {
            self.ws
                .stream
                .set_read_timeout(Some(Duration::from_millis(250)))
                .ok();
            match self.ws.recv_text() {
                Ok(text) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if v.get("method").and_then(|m| m.as_str())
                            == Some("Page.loadEventFired")
                        {
                            events.push(v);
                            break;
                        }
                        if v.get("method").is_some() {
                            events.push(v);
                        }
                    }
                }
                Err(_) => continue, // read timeout — ждём дальше
            }
        }
        // пауза на дочерние XHR + дренирование событий
        if wait_ms > 0 {
            std::thread::sleep(Duration::from_millis(wait_ms));
            self.ws
                .stream
                .set_read_timeout(Some(Duration::from_millis(100)))
                .ok();
            while let Ok(text) = self.ws.recv_text() {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if v.get("method").is_some() {
                        events.push(v);
                    }
                }
            }
        }

        // сетевой перехват: JSON-ответы скрытых API
        let json_requests: Vec<(String, String)> = events
            .iter()
            .filter_map(|e| {
                let p = e.get("params")?;
                let resp = p.get("response")?;
                let mime = resp.get("mimeType")?.as_str()?;
                if !mime.contains("json") {
                    return None;
                }
                let url = resp.get("url")?.as_str()?.to_string();
                let rid = p.get("requestId")?.as_str()?.to_string();
                Some((rid, url))
            })
            .collect();
        let mut intercepted: Vec<(String, String)> = Vec::new();
        for (rid, url) in json_requests.iter().take(64) {
            if let Ok(res) = self.command(
                "Network.getResponseBody",
                &format!(r#"{{"requestId":"{rid}"}}"#),
            ) {
                if let Some(body) = res.get("body").and_then(|b| b.as_str()) {
                    if body.len() > 16 && body.len() < 8 * 1024 * 1024 {
                        intercepted.push((url.clone(), body.to_string()));
                    }
                }
            }
        }

        // рендер-текст: innerText (браузер уже исполнил весь JS/Shadow DOM)
        let text = self
            .command(
                "Runtime.evaluate",
                r#"{"expression":"document.body ? document.body.innerText : ''","returnByValue":true}"#,
            )?
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        Ok(WebPage {
            url: url.to_string(),
            text,
            json_responses: intercepted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }
}
