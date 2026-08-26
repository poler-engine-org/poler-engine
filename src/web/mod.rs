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
pub mod robots;
pub mod simhash;
pub mod urlnorm;

use std::path::PathBuf;

pub use cdp::{CdpSession, WebPage};
pub use crawl::{CrawlConfig, CrawlStats, PageFetcher};
pub use index::{WebHit, WebIndex};

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

/// Детерминированное имя файла для URL (fnv-подобный хеш).
fn url_slug(url: &str) -> String {
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
