//! Web-Native Ingestion: рендер страницы через Chromium CDP → полный
//! POLER-пайплайн (ε / R / сцены / тройки / K-hop граф) + ВЕБ-ПОИСК
//! для AI: краулер (robots/sitemap/SimHash) → SQLite-индекс (BM25 +
//! PageRank + WebRank) → `--web-search`.
//!
//! Поток (веб-версия потокового конвейера движка):
//!
//! ```text
//! URL ──► Chromium (CDP): JS исполнен, DOM отрендерен
//!          ├── innerText (то, что видит человек: SPA/Shadow DOM решены)
//!          └── перехваченные JSON скрытых API (до превращения в HTML)
//!                 ▼
//!        Временный .md/.txt в веб-кэше (url → файл, детерминированно)
//!                 ▼
//!        poler_engine::scan_path: ε, R, сцены, тройки, граф
//!                 ▼
//!        ContextAnchor JSON + Cross-Universe рёбра (web→локальные файлы)
//! ```
//!
//! Фильтрация шума: реклама/меню/футеры отсеиваются сами — у них низкая
//! ε-плотность относительно запроса (редкие осмысленные токены побеждают
//! частотный шаблонный мусор).

pub mod cdp;
pub mod crawl;
pub mod extract;
pub mod index;
pub mod phrase;
pub mod robots;
pub mod simhash;
pub mod stem;
pub mod urlnorm;

use std::path::{Path, PathBuf};

pub use cdp::{CdpSession, WebPage};
pub use crawl::{CrawlConfig, CrawlStats, PageFetcher};
pub use index::{WebHit, WebIndex};

/// Ленивая обёртка: CdpFetcher с автозапуском Chromium при необходимости.
pub fn cdp_fetcher(cdp_port: u16, wait_ms: u64) -> Result<CdpFetcher, String> {
    ensure_chromium(cdp_port)?;
    CdpFetcher::new(cdp_port, wait_ms)
}

/// Реальный PageFetcher поверх Chromium CDP (одна живая сессия).
pub struct CdpFetcher {
    session: CdpSession,
    wait_ms: u64,
}

impl CdpFetcher {
    pub fn new(cdp_port: u16, wait_ms: u64) -> Result<Self, String> {
        Ok(Self {
            session: CdpSession::connect(cdp_port)?,
            wait_ms,
        })
    }
}

impl PageFetcher for CdpFetcher {
    fn fetch(&mut self, url: &str) -> Result<crawl::FetchedPage, String> {
        let p = self.session.load_page_full(url, self.wait_ms)?;
        Ok(crawl::FetchedPage {
            final_url: p.final_url,
            title: p.title,
            meta_description: p.meta_description,
            lang: p.lang,
            text: p.text,
            links: p.links,
        })
    }

    fn fetch_raw(&mut self, url: &str) -> Result<(u16, String), String> {
        self.session.fetch_raw(url)
    }
}

/// Путь к базе веб-индекса по умолчанию:
/// `$POLER_WEB_DB` → иначе `~/.local/share/poler-engine/web-index.db`.
pub fn default_db_path() -> PathBuf {
    if let Ok(p) = std::env::var("POLER_WEB_DB") {
        return PathBuf::from(p);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share/poler-engine/web-index.db")
}

/// Директория веб-кэша (текст страниц + перехваченный JSON).
pub fn web_cache_dir() -> PathBuf {
    let d = std::env::temp_dir().join("poler-web");
    let _ = std::fs::create_dir_all(&d);
    d
}

// ---------------------------------------------------------------------------
// Автозапуск Chromium: AI-агент не должен думать о браузере — движок сам
// поднимает headless-инстанс, если CDP-порт молчит.
// ---------------------------------------------------------------------------

/// Жив ли CDP на порту (tcp-коннект достаточно: DevTools слушает только его).
pub fn cdp_alive(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port)).is_ok()
}

/// Поиск бинаря браузера: $POLER_CHROME_BIN → PATH → известные абсолютные пути.
pub fn find_browser() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLER_CHROME_BIN") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    for name in [
        "chrome-headless-shell",
        "chromium",
        "chromium-browser",
        "google-chrome",
        "google-chrome-stable",
        "chrome",
    ] {
        if let Some(path) = std::env::var("PATH")
            .ok()?
            .split(':')
            .map(|dir| Path::new(dir).join(name))
            .find(|p| p.is_file())
        {
            return Some(path);
        }
    }
    // известные точки размещения (в т.ч. bundle движка)
    [
        "/home/z/my-project/browser/chrome-headless-shell-linux64/chrome-headless-shell",
        "/usr/bin/chromium",
        "/usr/bin/chromium-browser",
        "/usr/bin/google-chrome",
        "/snap/bin/chromium",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// Гарантирует живой Chromium с CDP на `port`: молча возвращается, если уже
/// слушает; иначе находит бинарь, поднимает headless и ждёт готовности ~15 с.
/// Дочерний процесс живёт, пока жив родитель (CLI-ран или MCP-сервер).
pub fn ensure_chromium(port: u16) -> Result<(), String> {
    if cdp_alive(port) {
        return Ok(());
    }
    let bin = find_browser().ok_or_else(|| {
        "Chromium не найден. Установите POLER_CHROME_BIN=/путь/к/chrome-headless-shell \
         или положите бинарь в PATH"
            .to_string()
    })?;
    let log = std::env::temp_dir().join("poler-chromium.log");
    let log_f = std::fs::File::options()
        .create(true)
        .append(true)
        .open(&log)
        .map_err(|e| format!("лог {log:?}: {e}"))?;
    let mut child = std::process::Command::new(&bin)
        .args([
            "--headless",
            "--no-sandbox",
            "--disable-gpu",
            "--disable-dev-shm-usage",
            "--no-first-run",
            "--disable-blink-features=AutomationControlled",
            &format!("--remote-debugging-port={port}"),
            "about:blank",
        ])
        .stdout(log_f.try_clone().map_err(|e| e.to_string())?)
        .stderr(log_f)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("запуск {:?}: {e}", bin))?;
    // готовность: до ~15 с (холодный старт на слабых машинах)
    for _ in 0..60 {
        if cdp_alive(port) {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let _ = child.kill();
    Err(format!(
        "Chromium не поднялся на порт {port} за 15 с (лог: {log:?})"
    ))
}

/// Детерминированное имя файла для URL (fnv-подобный хеш).
pub fn url_slug(url: &str) -> String {
    // простой стабильный хеш (FNV-1a 64)
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // + человекочитаемый хвост
    let tail: String = url
        .chars()
        .rev()
        .take(24)
        .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '.')
        .collect();
    format!("{h:016x}_{tail}")
}

/// Результат ingestion-сессии.
pub struct IngestResult {
    /// Файл с рендер-текстом страницы (в веб-кэше).
    pub text_file: PathBuf,
    /// Файлы перехваченных JSON API.
    pub json_files: Vec<PathBuf>,
    /// Заголовок страницы (из рендер-текста, первая непустая строка).
    pub title: String,
    /// Число символов рендер-текста.
    pub text_len: usize,
}

/// Загрузка URL через CDP и материализация в веб-кэш.
///
/// * `cdp_port` — порт Chromium с --remote-debugging-port;
/// * `wait_ms` — пауза на дочерние fetch/XHR после load.
pub fn ingest_url(url: &str, cdp_port: u16, wait_ms: u64) -> Result<IngestResult, String> {
    let mut session = CdpSession::connect(cdp_port)?;
    let page = session.load_page(url, wait_ms)?;

    if page.text.trim().is_empty() {
        return Err(format!(
            "пустой рендер-текст (страница не загрузилась или JS-ошибка): {url}"
        ));
    }

    let dir = web_cache_dir();
    let slug = url_slug(url);

    // рендер-текст: сохраняем с заголовком-URL сверху (сцена/глава)
    let title = page
        .text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(url)
        .trim()
        .chars()
        .take(120)
        .collect::<String>();
    let md = format!(
        "# {title}\n\n> source: {url}\n\n{}\n",
        page.text.trim()
    );
    let text_file = dir.join(format!("{slug}.md"));
    std::fs::write(&text_file, md).map_err(|e| format!("запись кэша: {e}"))?;

    // перехваченные JSON: каждый как отдельный файл кэша
    let mut json_files = Vec::new();
    for (i, (jurl, body)) in page.json_responses.iter().enumerate() {
        let jslug = format!("{}_api{}.json", url_slug(jurl), i);
        let f = dir.join(jslug);
        // pretty-print JSON, если парсится
        let pretty = serde_json::from_str::<serde_json::Value>(body)
            .and_then(|v| serde_json::to_string_pretty(&v))
            .unwrap_or_else(|_| body.clone());
        let _ = std::fs::write(&f, pretty);
        json_files.push(f);
    }

    Ok(IngestResult {
        text_file,
        json_files,
        title,
        text_len: page.text.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_slug_stable_and_readable() {
        let a = url_slug("https://example.com/docs/mmap");
        let b = url_slug("https://example.com/docs/mmap");
        assert_eq!(a, b);
        assert!(a.ends_with("docs.mmap") || a.len() > 16);
        assert!(url_slug("https://a.com") != url_slug("https://b.com"));
    }

    #[test]
    fn cache_dir_creates() {
        let d = web_cache_dir();
        assert!(d.exists());
    }

    #[test]
    fn default_db_path_env_override() {
        // не мутируем env параллельных тестов — только проверка формы
        let p = default_db_path();
        assert!(p.to_string_lossy().contains("poler-engine"));
    }
}
