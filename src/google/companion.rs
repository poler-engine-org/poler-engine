//! CompanionBridge — сменный коннектор ввода-вывода к оф. Pre-GA
//! Gemini Notebook Enterprise API (раньше NotebookLM API).
//!
//! ## Архитектурный принцип
//!
//! **Не заменять ядро, а дополнять.** poler-engine уже имеет рабочий канал
//! к NotebookLM через `nlm.rs` (потребительский `batchexecute`-протокол).
//! Он покрывает операции, **которых нет в оф. API** — chat/query/get_notes/
//! get_artifacts/get_source_content. Зато оф. Pre-GA API силён там, где
//! `batchexecute` слаб: пакетная заливка источников (`sources:batchCreate`),
//! upload файлов (`sources:uploadFile`), создание/удаление audio overview.
//!
//! Companion — это **сменный модуль**, а не замена: `HybridProvider` роутит
//! каждую операцию к тому провайдеру, который её поддерживает, с прозрачным
//! fallback CDP → GCP (и наоборот) при отказах.
//!
//! ## Слои
//!
//! ```text
//! shell/TUI/CLI  →  HybridProvider  →  GcpEnterpriseProvider (оф. Pre-GA)
//!                                  ↘  CdpBatchexecuteProvider (существ. nlm.rs)
//! ```
//!
//! ## Что НЕ трогает
//!
//! * `web-index.db` (BM25/PageRank/IIR/SimHash) — ядро поиска;
//! * `nlm_ingest.rs` (FNV-1a content_hash, URL схема `nlm://…`);
//! * `GoogleHttp` — HTTPS через CDP `fetch()` (TLS в Chromium, 0 Rust-TLS deps);
//! * `oauth.rs` flow — переиспользуется Bearer с расширением скоупа.
//!
//! ## Endpoints (research session 2026-08-26)
//!
//! Base: `https://{us|eu|global}-discoveryengine.googleapis.com/v1alpha`
//!
//! | Endpoint                                                     | Method  | Операция           |
//! |--------------------------------------------------------------|---------|--------------------|
//! | `/projects/{P}/locations/{L}/notebooks`                      | POST    | Create notebook    |
//! | `/projects/{P}/locations/{L}/notebooks/{NB}`                 | GET     | Get notebook       |
//! | `/projects/{P}/locations/{L}/notebooks/{NB}/sources:batchCreate`   | POST | Batch add sources  |
//! | `/upload/v1alpha/projects/{P}/locations/{L}/notebooks/{NB}/sources:uploadFile` | POST | Upload single file |
//! | `/projects/{P}/locations/{L}/notebooks/{NB}/sources/{SRC}`   | GET     | Get source metadata |
//! | `/projects/{P}/locations/{L}/notebooks/{NB}/sources:batchDelete`   | POST | Bulk delete sources |
//! | `/projects/{P}/locations/{L}/notebooks/{NB}/audioOverviews`  | POST    | Create audio overview |
//! | `/projects/{P}/locations/{L}/notebooks/{NB}/audioOverviews/default` | DELETE | Delete audio overview |
//!
//! Auth: Bearer-токен из `oauth.rs` со скоупом `cloud-platform`.
//!
//! ## Статус impls
//!
//! * **M1 (этот файл):** URL builders + trait + типы + тесты URL builders.
//! * **M2:** `GcpEnterpriseProvider` реал-имплементация эндпоинтов.
//! * **M3:** `HybridProvider` routing + fallback.
//! * **M4:** TUI Enter-handler на источнике.
//! * **M5:** CLI subcommands (`nlm upload`, `nlm aoview`).

use std::path::Path;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Конфигурация (env vars, как уже принято в poler-engine)
// ---------------------------------------------------------------------------

/// GCP-регион endpoint'а: `global` (default), `us`, `eu`.
///
/// Определяет хост: `https://{region}-discoveryengine.googleapis.com`.
pub fn gcp_region() -> String {
    std::env::var("POLER_GCP_REGION").unwrap_or_else(|_| "global".to_string())
}

/// Google Cloud project number (число, например `123456789012`).
pub fn gcp_project_number() -> Option<String> {
    std::env::var("POLER_GCP_PROJECT_NUMBER").ok().filter(|s| !s.is_empty())
}

/// Локация data store: `global` (default), `us`, `eu` или конкретный регион.
pub fn gcp_location() -> String {
    std::env::var("POLER_GCP_LOCATION").unwrap_or_else(|_| "global".to_string())
}

/// Режим работы моста: `auto` (default), `gcp_only`, `cdp_only`.
pub fn companion_mode() -> CompanionMode {
    match std::env::var("POLER_COMPANION_MODE")
        .unwrap_or_else(|_| "auto".to_string())
        .as_str()
    {
        "gcp_only" => CompanionMode::GcpOnly,
        "cdp_only" => CompanionMode::CdpOnly,
        _ => CompanionMode::Auto,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompanionMode {
    Auto,
    GcpOnly,
    CdpOnly,
}

// ---------------------------------------------------------------------------
// URL builders — детерминированные, тестируются
// ---------------------------------------------------------------------------

/// Базовый URL оф. Pre-GA NotebookLM API для заданного региона.
///
/// `https://global-discoveryengine.googleapis.com/v1alpha`
pub fn api_base(region: &str) -> String {
    format!("https://{region}-discoveryengine.googleapis.com/v1alpha")
}

/// Путь к ноутбуку: `/v1alpha/projects/{P}/locations/{L}/notebooks/{NB}`.
pub fn notebook_path(region: &str, project: &str, location: &str, nb_id: &str) -> String {
    format!(
        "{}/projects/{}/locations/{}/notebooks/{}",
        api_base(region),
        project,
        location,
        nb_id
    )
}

/// URL создания ноутбука (POST).
pub fn notebooks_create_url(region: &str, project: &str, location: &str) -> String {
    format!(
        "{}/projects/{}/locations/{}/notebooks",
        api_base(region),
        project,
        location
    )
}

/// URL получения ноутбука по ID (GET).
pub fn notebook_get_url(region: &str, project: &str, location: &str, nb_id: &str) -> String {
    notebook_path(region, project, location, nb_id)
}

/// URL пакетной заливки источников (POST).
/// `…/notebooks/{NB}/sources:batchCreate`
pub fn sources_batch_create_url(
    region: &str,
    project: &str,
    location: &str,
    nb_id: &str,
) -> String {
    format!("{}/sources:batchCreate", notebook_path(region, project, location, nb_id))
}

/// URL заливки одного файла (POST, X-Goog-Upload-Protocol: raw).
/// `https://{region}-discoveryengine.googleapis.com/upload/v1alpha/…/notebooks/{NB}/sources:uploadFile`
pub fn sources_upload_file_url(
    region: &str,
    project: &str,
    location: &str,
    nb_id: &str,
) -> String {
    // Google media-upload convention: `/upload/v1alpha/…` — upload-префикс ВПЕРЕДИ
    // версии API. Endpoint — `notebooks/{NB}/sources:uploadFile` (colon-method
    // применяется к коллекции sources, а не к самому notebook).
    let nb_path = notebook_path(region, project, location, nb_id);
    let base = api_base(region);
    // base = `https://{region}-discoveryengine.googleapis.com/v1alpha`
    // nb_path = `{base}/projects/{P}/locations/{L}/notebooks/{NB}`
    // upload_base = `https://{region}-discoveryengine.googleapis.com/upload/v1alpha`
    // path_after_base = `projects/{P}/locations/{L}/notebooks/{NB}`
    // result = `{upload_base}/{path_after_base}/sources:uploadFile`
    let path_after_base = nb_path
        .strip_prefix(&format!("{}/", base))
        .unwrap_or(&nb_path);
    let upload_base = base.replacen("/v1alpha", "/upload/v1alpha", 1);
    format!("{}/{}/sources:uploadFile", upload_base, path_after_base)
}

/// URL получения источника по ID (GET, metadata only: wordCount, tokenCount).
pub fn source_get_url(
    region: &str,
    project: &str,
    location: &str,
    nb_id: &str,
    src_id: &str,
) -> String {
    format!("{}/sources/{}", notebook_path(region, project, location, nb_id), src_id)
}

/// URL пакетного удаления источников (POST, body: { "names": [...] }).
pub fn sources_batch_delete_url(
    region: &str,
    project: &str,
    location: &str,
    nb_id: &str,
) -> String {
    format!("{}/sources:batchDelete", notebook_path(region, project, location, nb_id))
}

/// URL создания audio overview (POST).
pub fn audio_overview_create_url(
    region: &str,
    project: &str,
    location: &str,
    nb_id: &str,
) -> String {
    format!("{}/audioOverviews", notebook_path(region, project, location, nb_id))
}

/// URL удаления audio overview (DELETE, путь `default` — только один overview на ноутбук).
pub fn audio_overview_delete_url(
    region: &str,
    project: &str,
    location: &str,
    nb_id: &str,
) -> String {
    format!(
        "{}/audioOverviews/default",
        notebook_path(region, project, location, nb_id)
    )
}

// ---------------------------------------------------------------------------
// Типы (для trait + providers)
// ---------------------------------------------------------------------------

/// Операции моста — для `supports(op)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Op {
    ListNotebooks,
    GetNotebook,
    CreateNotebook,
    BatchCreateSources,
    UploadFile,
    GetSourceMeta,
    GetSourceContent,
    GetNotes,
    ListArtifacts,
    Chat,
    CreateAudioOverview,
    DeleteAudioOverview,
    DeleteSources,
}

/// Краткая инфа о ноутбуке для list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotebookBrief {
    pub id: String,
    pub title: String,
    pub emoji: String,
    pub updated_at: Option<String>,
}

/// Описание источника для batch upload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceUpload {
    /// Google Docs/Slides: `documentId` + mimeType + sourceName.
    GoogleDrive {
        document_id: String,
        mime_type: String,
        source_name: String,
    },
    /// Сырой текст.
    Text {
        source_name: String,
        content: String,
    },
    /// Веб-URL.
    Web {
        url: String,
        source_name: String,
    },
    /// YouTube-видео.
    YouTube {
        youtube_url: String,
    },
}

/// Метаданные источника после заливки (response от batchCreate/uploadFile).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMeta {
    pub source_id: String,
    pub title: String,
    /// Полный resource name: `projects/{P}/locations/{L}/notebooks/{NB}/sources/{SRC}`.
    pub resource_name: String,
    pub status: String,
    pub word_count: Option<u64>,
    pub token_count: Option<u64>,
}

/// Тип источника (для TUI Enter-handler).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SourceKind {
    GoogleDrive { drive_file_id: String },
    YouTube { youtube_id: String },
    Web { url: String },
    Text { content: String },
    /// Файл, залитый через `sources:uploadFile`. Local_path — путь к
    /// оригиналу на машине владельца (для мгновенного Enter→$EDITOR).
    FileUpload { local_path: String },
    /// Неизвестный/неподдержанный тип — fallback на CdpBatchexecuteProvider.
    Unknown,
}

impl SourceKind {
    /// Что делать при Enter в TUI на источнике этого типа.
    pub fn enter_action(&self, src_id: &str) -> EnterAction {
        match self {
            SourceKind::GoogleDrive { drive_file_id } => EnterAction::OpenUrl(format!(
                "https://docs.google.com/document/d/{}/edit",
                drive_file_id
            )),
            SourceKind::YouTube { youtube_id } => EnterAction::OpenUrl(format!(
                "https://www.youtube.com/watch?v={}",
                youtube_id
            )),
            SourceKind::Web { url } => EnterAction::OpenUrl(url.clone()),
            SourceKind::Text { content } => {
                EnterAction::EditTemp {
                    content: content.clone(),
                    suggested_filename: format!("poler-src-{}.md", src_id),
                }
            }
            SourceKind::FileUpload { local_path } => {
                EnterAction::EditLocal(local_path.clone())
            }
            SourceKind::Unknown => EnterAction::FallbackFetch {
                src_id: src_id.to_string(),
            },
        }
    }
}

/// Действие, которое TUI должен выполнить при Enter на источнике.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnterAction {
    /// Открыть URL в системном браузере (xdg-open).
    OpenUrl(String),
    /// Открыть локальный файл в $EDITOR (для uploadFile-источников — мгновенно).
    EditLocal(String),
    /// Сохранить content в /tmp/{filename} и открыть в $EDITOR.
    EditTemp {
        content: String,
        suggested_filename: String,
    },
    /// Неизвестный тип — фетч через CdpBatchexecuteProvider.hizoJc.
    FallbackFetch { src_id: String },
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Сменный коннектор ввода-вывода к источникам NotebookLM.
///
/// Имплементации:
/// * `GcpEnterpriseProvider` — оф. Pre-GA API (M2).
/// * `CdpBatchexecuteProvider` — обёртка над `nlm.rs` (M2/M3).
/// * `HybridProvider` — routing + fallback (M3).
pub trait SourceContentProvider {
    fn name(&self) -> &'static str;

    /// Покрыта ли операция у этого провайдера?
    /// Решает routing в `HybridProvider`.
    fn supports(&self, op: Op) -> bool;

    /// Готов ли провайдер к работе (есть токен, настроен project, etc.)?
    fn ready(&self) -> bool {
        true
    }

    fn list_notebooks(&mut self) -> Result<Vec<NotebookBrief>, BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::ListNotebooks,
            provider: Self::name(self),
        })
    }

    fn get_notebook(&mut self, _nb_id: &str) -> Result<Vec<SourceMeta>, BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::GetNotebook,
            provider: Self::name(self),
        })
    }

    fn batch_create_sources(
        &mut self,
        _nb_id: &str,
        _items: &[SourceUpload],
    ) -> Result<Vec<String>, BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::BatchCreateSources,
            provider: Self::name(self),
        })
    }

    fn upload_file(
        &mut self,
        _nb_id: &str,
        _local_path: &Path,
        _display_name: &str,
        _mime: &str,
    ) -> Result<String, BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::UploadFile,
            provider: Self::name(self),
        })
    }

    fn get_source(&mut self, _nb_id: &str, _src_id: &str) -> Result<SourceMeta, BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::GetSourceMeta,
            provider: Self::name(self),
        })
    }

    fn create_audio_overview(
        &mut self,
        _nb_id: &str,
        _source_ids: &[&str],
        _focus: Option<&str>,
        _lang: &str,
    ) -> Result<String, BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::CreateAudioOverview,
            provider: Self::name(self),
        })
    }

    fn delete_audio_overview(&mut self, _nb_id: &str) -> Result<(), BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::DeleteAudioOverview,
            provider: Self::name(self),
        })
    }
}

// ---------------------------------------------------------------------------
// Ошибки
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum BridgeError {
    /// Операция не поддержана этим провайдером.
    NotSupported { op: Op, provider: &'static str },
    /// Не сконфигурирован GCP-аккаунт (нет project number, нет токена).
    NotConfigured(String),
    /// HTTP-ошибка моста (статус + тело ответа).
    Http { status: u16, body: String },
    /// Сетевая/CDP-ошибка (браузер не поднят, fetch упал).
    Transport(String),
    /// Не удалось распарсить JSON-ответ Google.
    Parse(String),
}

impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BridgeError::NotSupported { op, provider } => {
                write!(f, "операция {:?} не поддержана провайдером {}", op, provider)
            }
            BridgeError::NotConfigured(msg) => {
                write!(f, "GCP-мост не сконфигурирован: {}", msg)
            }
            BridgeError::Http { status, body } => {
                let snippet: String = body.chars().take(300).collect();
                write!(f, "GCP-запрос вернул HTTP {}: {}", status, snippet)
            }
            BridgeError::Transport(msg) => write!(f, "транспорт: {}", msg),
            BridgeError::Parse(msg) => write!(f, "парсинг ответа: {}", msg),
        }
    }
}

impl std::error::Error for BridgeError {}

// ---------------------------------------------------------------------------
// GcpEnterpriseProvider — skeleton (M2 сделает методы реальными)
// ---------------------------------------------------------------------------

/// Конфигурация GCP-провайдера.
#[derive(Debug, Clone)]
pub struct GcpConfig {
    pub region: String,
    pub project_number: String,
    pub location: String,
}

impl GcpConfig {
    /// Из env: `POLER_GCP_REGION` + `POLER_GCP_PROJECT_NUMBER` + `POLER_GCP_LOCATION`.
    /// Возвращает `None` если project_number не задан.
    pub fn from_env() -> Option<Self> {
        let project_number = gcp_project_number()?;
        Some(Self {
            region: gcp_region(),
            project_number,
            location: gcp_location(),
        })
    }

    /// Готов ли (есть project + region).
    pub fn is_ready(&self) -> bool {
        !self.project_number.is_empty() && !self.region.is_empty()
    }
}

impl Default for GcpConfig {
    fn default() -> Self {
        Self {
            region: "global".to_string(),
            project_number: String::new(),
            location: "global".to_string(),
        }
    }
}

/// Провайдер поверх оф. Pre-GA Gemini Notebook Enterprise API.
///
/// M1: skeleton — все методы возвращают `NotSupported`.
/// M2: реальные вызовы через `GoogleHttp` с Bearer из `oauth.rs`.
pub struct GcpEnterpriseProvider {
    pub config: GcpConfig,
    // M2: pub http: GoogleHttp,  ← добавится в M2
}

impl GcpEnterpriseProvider {
    pub fn new(config: GcpConfig) -> Self {
        Self { config }
    }

    pub fn from_env_or_default() -> Self {
        Self::new(GcpConfig::from_env().unwrap_or_default())
    }
}

impl SourceContentProvider for GcpEnterpriseProvider {
    fn name(&self) -> &'static str {
        "gcp_enterprise"
    }

    fn supports(&self, op: Op) -> bool {
        match op {
            Op::ListNotebooks
            | Op::GetNotebook
            | Op::CreateNotebook
            | Op::BatchCreateSources
            | Op::UploadFile
            | Op::GetSourceMeta
            | Op::CreateAudioOverview
            | Op::DeleteAudioOverview
            | Op::DeleteSources => true,
            // Оф. Pre-GA API НЕ покрывает:
            Op::GetSourceContent | Op::GetNotes | Op::ListArtifacts | Op::Chat => false,
        }
    }

    fn ready(&self) -> bool {
        self.config.is_ready()
    }

    // Реальные имплементации добавятся в M2.
    // Сейчас все методы используют default-trait impl выше и возвращают NotSupported.
}

// ---------------------------------------------------------------------------
// CdpBatchexecuteProvider — skeleton-обёртка над существующим nlm.rs (M3)
// ---------------------------------------------------------------------------

/// Обёртка над `crate::google::nlm::NlmSession`, представляющая его
/// как `SourceContentProvider`. Покрывает то, чего нет в оф. API:
/// chat, get_notes, list_artifacts, get_source_content.
pub struct CdpBatchexecuteProvider {
    // M3: pub session: NlmSession,
    pub google_cdp_port: u16,
}

impl CdpBatchexecuteProvider {
    pub fn new(google_cdp_port: u16) -> Self {
        Self { google_cdp_port }
    }
}

impl SourceContentProvider for CdpBatchexecuteProvider {
    fn name(&self) -> &'static str {
        "cdp_batchexecute"
    }

    fn supports(&self, op: Op) -> bool {
        match op {
            Op::ListNotebooks
            | Op::GetNotebook
            | Op::GetSourceContent
            | Op::GetNotes
            | Op::ListArtifacts
            | Op::Chat => true,
            // CDP не умеет:
            Op::CreateNotebook
            | Op::BatchCreateSources
            | Op::UploadFile
            | Op::GetSourceMeta
            | Op::CreateAudioOverview
            | Op::DeleteAudioOverview
            | Op::DeleteSources => false,
        }
    }

    fn ready(&self) -> bool {
        // CDP готов, если браузер на порту живой (проверка делается в NlmSession).
        // M3: реальная проверка через crate::web::cdp_alive(self.google_cdp_port).
        true
    }
}

// ---------------------------------------------------------------------------
// HybridProvider — skeleton (M3)
// ---------------------------------------------------------------------------

/// Гибридный провайдер: роутит операции к тому, кто их поддерживает,
/// с прозрачным fallback GCP ↔ CDP.
pub struct HybridProvider {
    pub gcp: GcpEnterpriseProvider,
    pub cdp: CdpBatchexecuteProvider,
    pub mode: CompanionMode,
}

impl HybridProvider {
    pub fn from_env() -> Self {
        Self {
            gcp: GcpEnterpriseProvider::from_env_or_default(),
            cdp: CdpBatchexecuteProvider::new(crate::google::google_cdp_port()),
            mode: companion_mode(),
        }
    }
}

impl SourceContentProvider for HybridProvider {
    fn name(&self) -> &'static str {
        "hybrid"
    }

    fn supports(&self, op: Op) -> bool {
        self.gcp.supports(op) || self.cdp.supports(op)
    }

    fn ready(&self) -> bool {
        match self.mode {
            CompanionMode::GcpOnly => self.gcp.ready(),
            CompanionMode::CdpOnly => self.cdp.ready(),
            CompanionMode::Auto => self.gcp.ready() || self.cdp.ready(),
        }
    }
    // Реальные routing-имплементации методов появятся в M3.
    // Сейчас все методы используют default-trait impl (NotSupported).
}

// ---------------------------------------------------------------------------
// Тесты URL builders (главная часть M1 — детерминированная, без сети)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const R: &str = "global";
    const P: &str = "123456789012";
    const L: &str = "global";
    const NB: &str = "704f2610-c02b-4ec1-9fc7-a3b72dde2af1";
    const SRC: &str = "src-abc123def456";

    #[test]
    fn api_base_format() {
        assert_eq!(
            api_base("global"),
            "https://global-discoveryengine.googleapis.com/v1alpha"
        );
        assert_eq!(
            api_base("us"),
            "https://us-discoveryengine.googleapis.com/v1alpha"
        );
        assert_eq!(
            api_base("eu"),
            "https://eu-discoveryengine.googleapis.com/v1alpha"
        );
    }

    #[test]
    fn notebooks_create_url_format() {
        let url = notebooks_create_url(R, P, L);
        assert_eq!(
            url,
            "https://global-discoveryengine.googleapis.com/v1alpha\
             /projects/123456789012/locations/global/notebooks"
        );
    }

    #[test]
    fn notebook_get_url_format() {
        let url = notebook_get_url(R, P, L, NB);
        assert!(url.starts_with(&api_base(R)));
        assert!(url.contains("/projects/123456789012/"));
        assert!(url.contains("/locations/global/"));
        assert!(url.ends_with(&format!("/notebooks/{}", NB)));
    }

    #[test]
    fn sources_batch_create_url_format() {
        let url = sources_batch_create_url(R, P, L, NB);
        assert_eq!(
            url,
            format!(
                "https://global-discoveryengine.googleapis.com/v1alpha\
                 /projects/{}/locations/{}/notebooks/{}/sources:batchCreate",
                P, L, NB
            )
        );
    }

    #[test]
    fn sources_upload_file_url_uses_upload_path() {
        let url = sources_upload_file_url(R, P, L, NB);
        // Ключевое: должен быть /upload/v1alpha/… — отдельный media-upload path
        assert!(url.contains("/upload/v1alpha/"));
        assert!(url.ends_with(":uploadFile"));
        assert!(url.contains(&format!("/notebooks/{}/", NB)));
    }

    #[test]
    fn source_get_url_includes_src_id() {
        let url = source_get_url(R, P, L, NB, SRC);
        assert!(url.ends_with(&format!("/sources/{}", SRC)));
        assert!(url.contains(&format!("/notebooks/{}/", NB)));
    }

    #[test]
    fn sources_batch_delete_url_format() {
        let url = sources_batch_delete_url(R, P, L, NB);
        assert!(url.ends_with("/sources:batchDelete"));
        assert!(url.contains(&format!("/notebooks/{}/", NB)));
    }

    #[test]
    fn audio_overview_create_url_format() {
        let url = audio_overview_create_url(R, P, L, NB);
        assert!(url.ends_with("/audioOverviews"));
        assert!(url.contains(&format!("/notebooks/{}/", NB)));
    }

    #[test]
    fn audio_overview_delete_url_uses_default() {
        let url = audio_overview_delete_url(R, P, L, NB);
        assert!(url.ends_with("/audioOverviews/default"));
        assert!(url.contains(&format!("/notebooks/{}/", NB)));
    }

    #[test]
    fn gcp_config_from_env_returns_none_without_project() {
        let saved = std::env::var("POLER_GCP_PROJECT_NUMBER");
        std::env::remove_var("POLER_GCP_PROJECT_NUMBER");
        assert!(GcpConfig::from_env().is_none());
        if let Ok(v) = saved {
            std::env::set_var("POLER_GCP_PROJECT_NUMBER", v);
        }
    }

    #[test]
    fn gcp_config_from_env_returns_some_with_project() {
        let saved_p = std::env::var("POLER_GCP_PROJECT_NUMBER");
        let saved_r = std::env::var("POLER_GCP_REGION");
        let saved_l = std::env::var("POLER_GCP_LOCATION");
        std::env::set_var("POLER_GCP_PROJECT_NUMBER", "999999999999");
        std::env::set_var("POLER_GCP_REGION", "eu");
        std::env::set_var("POLER_GCP_LOCATION", "europe-west1");
        let cfg = GcpConfig::from_env().expect("config должен построиться");
        assert_eq!(cfg.project_number, "999999999999");
        assert_eq!(cfg.region, "eu");
        assert_eq!(cfg.location, "europe-west1");
        assert!(cfg.is_ready());
        // restore
        match saved_p {
            Ok(v) => std::env::set_var("POLER_GCP_PROJECT_NUMBER", v),
            Err(_) => std::env::remove_var("POLER_GCP_PROJECT_NUMBER"),
        }
        match saved_r {
            Ok(v) => std::env::set_var("POLER_GCP_REGION", v),
            Err(_) => std::env::remove_var("POLER_GCP_REGION"),
        }
        match saved_l {
            Ok(v) => std::env::set_var("POLER_GCP_LOCATION", v),
            Err(_) => std::env::remove_var("POLER_GCP_LOCATION"),
        }
    }

    #[test]
    fn gcp_default_config_not_ready() {
        let cfg = GcpConfig::default();
        assert!(!cfg.is_ready(), "empty project_number → not ready");
    }

    #[test]
    fn gcp_provider_supports_matrix() {
        let p = GcpEnterpriseProvider::new(GcpConfig::default());
        // Покрыто оф. API
        assert!(p.supports(Op::BatchCreateSources));
        assert!(p.supports(Op::UploadFile));
        assert!(p.supports(Op::GetSourceMeta));
        assert!(p.supports(Op::CreateAudioOverview));
        assert!(p.supports(Op::DeleteAudioOverview));
        assert!(p.supports(Op::ListNotebooks));
        assert!(p.supports(Op::GetNotebook));
        assert!(p.supports(Op::CreateNotebook));
        assert!(p.supports(Op::DeleteSources));
        // НЕ покрыто оф. API
        assert!(!p.supports(Op::Chat));
        assert!(!p.supports(Op::GetNotes));
        assert!(!p.supports(Op::ListArtifacts));
        assert!(!p.supports(Op::GetSourceContent));
    }

    #[test]
    fn cdp_provider_supports_matrix_inverse_of_gcp() {
        let p = CdpBatchexecuteProvider::new(9223);
        // CDP покрывает то, чего нет в оф. API
        assert!(p.supports(Op::Chat));
        assert!(p.supports(Op::GetNotes));
        assert!(p.supports(Op::ListArtifacts));
        assert!(p.supports(Op::GetSourceContent));
        assert!(p.supports(Op::ListNotebooks));
        assert!(p.supports(Op::GetNotebook));
        // CDP НЕ умеет
        assert!(!p.supports(Op::BatchCreateSources));
        assert!(!p.supports(Op::UploadFile));
        assert!(!p.supports(Op::CreateAudioOverview));
        assert!(!p.supports(Op::DeleteAudioOverview));
        assert!(!p.supports(Op::DeleteSources));
        assert!(!p.supports(Op::CreateNotebook));
    }

    #[test]
    fn hybrid_supports_union_of_both_providers() {
        let h = HybridProvider::from_env();
        // должно поддерживать всё, что поддерживает хотя бы один
        for op in [
            Op::ListNotebooks,
            Op::GetNotebook,
            Op::CreateNotebook,
            Op::BatchCreateSources,
            Op::UploadFile,
            Op::GetSourceMeta,
            Op::GetSourceContent,
            Op::GetNotes,
            Op::ListArtifacts,
            Op::Chat,
            Op::CreateAudioOverview,
            Op::DeleteAudioOverview,
            Op::DeleteSources,
        ] {
            assert!(h.supports(op), "hybrid должен поддерживать {:?}", op);
        }
    }

    #[test]
    fn companion_mode_parses_env() {
        let saved = std::env::var("POLER_COMPANION_MODE");
        std::env::set_var("POLER_COMPANION_MODE", "gcp_only");
        assert_eq!(companion_mode(), CompanionMode::GcpOnly);
        std::env::set_var("POLER_COMPANION_MODE", "cdp_only");
        assert_eq!(companion_mode(), CompanionMode::CdpOnly);
        std::env::set_var("POLER_COMPANION_MODE", "auto");
        assert_eq!(companion_mode(), CompanionMode::Auto);
        std::env::remove_var("POLER_COMPANION_MODE");
        assert_eq!(companion_mode(), CompanionMode::Auto, "default = auto");
        if let Ok(v) = saved {
            std::env::set_var("POLER_COMPANION_MODE", v);
        }
    }

    #[test]
    fn source_kind_enter_action_for_each_variant() {
        let src_id = "src-test";

        // GoogleDrive → OpenUrl на docs.google.com
        let action = SourceKind::GoogleDrive {
            drive_file_id: "1AbC".to_string(),
        }
        .enter_action(src_id);
        match action {
            EnterAction::OpenUrl(url) => {
                assert_eq!(url, "https://docs.google.com/document/d/1AbC/edit");
            }
            _ => panic!("GoogleDrive → OpenUrl"),
        }

        // YouTube → OpenUrl на youtube.com/watch?v=…
        let action = SourceKind::YouTube {
            youtube_id: "dQw4w9WgXcQ".to_string(),
        }
        .enter_action(src_id);
        match action {
            EnterAction::OpenUrl(url) => {
                assert_eq!(url, "https://www.youtube.com/watch?v=dQw4w9WgXcQ");
            }
            _ => panic!("YouTube → OpenUrl"),
        }

        // Web → OpenUrl(url)
        let action = SourceKind::Web {
            url: "https://example.com/post".to_string(),
        }
        .enter_action(src_id);
        match action {
            EnterAction::OpenUrl(url) => assert_eq!(url, "https://example.com/post"),
            _ => panic!("Web → OpenUrl"),
        }

        // Text → EditTemp с контентом и suggested_filename
        let action = SourceKind::Text {
            content: "# Текст источника\n\nПривет, мир.".to_string(),
        }
        .enter_action(src_id);
        match action {
            EnterAction::EditTemp {
                content,
                suggested_filename,
            } => {
                assert!(content.contains("Привет, мир."));
                assert!(suggested_filename.contains(src_id));
                assert!(suggested_filename.ends_with(".md"));
            }
            _ => panic!("Text → EditTemp"),
        }

        // FileUpload → EditLocal(local_path) — мгновенно, без фетча
        let action = SourceKind::FileUpload {
            local_path: "/home/user/docs/report.pdf".to_string(),
        }
        .enter_action(src_id);
        match action {
            EnterAction::EditLocal(path) => {
                assert_eq!(path, "/home/user/docs/report.pdf");
            }
            _ => panic!("FileUpload → EditLocal"),
        }

        // Unknown → FallbackFetch
        let action = SourceKind::Unknown.enter_action(src_id);
        match action {
            EnterAction::FallbackFetch { src_id: sid } => assert_eq!(sid, "src-test"),
            _ => panic!("Unknown → FallbackFetch"),
        }
    }

    #[test]
    fn source_upload_serializes_to_google_drive_shape() {
        let u = SourceUpload::GoogleDrive {
            document_id: "1AbC".to_string(),
            mime_type: "application/vnd.google-apps.document".to_string(),
            source_name: "Отчёт".to_string(),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["kind"], "google_drive");
        assert_eq!(v["document_id"], "1AbC");
        assert_eq!(v["mime_type"], "application/vnd.google-apps.document");
        assert_eq!(v["source_name"], "Отчёт");
    }

    #[test]
    fn source_upload_serializes_to_text_shape() {
        let u = SourceUpload::Text {
            source_name: "Заметка".to_string(),
            content: "Контент".to_string(),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["kind"], "text");
        assert_eq!(v["source_name"], "Заметка");
        assert_eq!(v["content"], "Контент");
    }

    #[test]
    fn source_upload_serializes_to_web_shape() {
        let u = SourceUpload::Web {
            url: "https://example.com".to_string(),
            source_name: "Page".to_string(),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["kind"], "web");
        assert_eq!(v["url"], "https://example.com");
    }

    #[test]
    fn source_upload_serializes_to_youtube_shape() {
        let u = SourceUpload::YouTube {
            youtube_url: "https://youtube.com/watch?v=abc".to_string(),
        };
        let v = serde_json::to_value(&u).unwrap();
        assert_eq!(v["kind"], "you_tube");
        assert_eq!(v["youtube_url"], "https://youtube.com/watch?v=abc");
    }

    #[test]
    fn bridge_error_display_truncates_body() {
        let long_body = "x".repeat(500);
        let e = BridgeError::Http {
            status: 500,
            body: long_body.clone(),
        };
        let s = format!("{}", e);
        assert!(s.starts_with("GCP-запрос вернул HTTP 500:"));
        // Display должен обрезать тело до 300 символов
        let body_part = s.strip_prefix("GCP-запрос вернул HTTP 500: ").unwrap();
        assert!(body_part.len() <= 300);
    }

    #[test]
    fn bridge_error_not_supported_lists_op_and_provider() {
        let e = BridgeError::NotSupported {
            op: Op::Chat,
            provider: "gcp_enterprise",
        };
        let s = format!("{}", e);
        assert!(s.contains("Chat"));
        assert!(s.contains("gcp_enterprise"));
    }

    #[test]
    fn upload_file_url_google_drive_recipe() {
        // Эмуляция тела запроса для Google Drive источника (см. доку)
        let u = SourceUpload::GoogleDrive {
            document_id: "1AbC".to_string(),
            mime_type: "application/vnd.google-apps.document".to_string(),
            source_name: "Документ".to_string(),
        };
        let body = serde_json::json!({
            "userContents": [serde_json::to_value(&u).unwrap()]
        });
        assert_eq!(
            body["userContents"][0]["kind"],
            "google_drive"
        );
        assert_eq!(
            body["userContents"][0]["document_id"],
            "1AbC"
        );
    }
}
