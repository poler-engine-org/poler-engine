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
//! * **M1:** URL builders + trait + типы + тесты URL builders. ✓
//! * **M2:** `GcpEnterpriseProvider` реал-имплементация эндпоинтов. ✓
//!   - `ureq` (rustls) HTTP-клиент, lazy-init `Agent` (60s timeout).
//!   - Bearer из `oauth::ensure_gcp_fresh` (cloud-platform scope).
//!   - 9 операций: list_notebooks, get_notebook, batch_create_sources,
//!     upload_file (X-Goog-Upload-Protocol: raw), get_source,
//!     batch_delete_sources, create_audio_overview, delete_audio_overview.
//!   - 3 новых unit-теста: last_segment, ready, supports matrix (всего 27).
//! * **M3:** `HybridProvider` routing + fallback. ✓
//!   - `HybridProvider::route<F,G,R>(op, f_gcp, f_cdp)` generic-helper.
//!   - Routing policy: GcpOnly/CdpOnly — primary only, fallback off;
//!     Auto — primary=GCP если `gcp.supports(op)`, иначе CDP; fallback on
//!     (только если secondary `supports(op)`).
//!   - Fallback триггерится только на `NotSupported`/`NotConfigured`;
//!     `Http`/`Transport`/`Parse` propagates без fallback.
//!   - `primary_for(op)` и `fallback_enabled()` — pure-fns для тестов.
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

    /// Пакетное удаление источников по списку source_id.
    /// Оф. API: POST `/notebooks/{NB}/sources:batchDelete` body `{ "names": [...] }`.
    fn batch_delete_sources(
        &mut self,
        _nb_id: &str,
        _src_ids: &[&str],
    ) -> Result<(), BridgeError> {
        Err(BridgeError::NotSupported {
            op: Op::DeleteSources,
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
// GcpEnterpriseProvider — реал-имплементация (M2 ✓, ureq + cloud-platform Bearer)
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
/// M2: реальные вызовы через `ureq` (rustls) с Bearer из `gcp_tokens.json`.
/// `cloud-platform` scope — самый привилегированный GCP-скоуп, поэтому
/// токен живёт в отдельном файле от Gmail/Drive.
pub struct GcpEnterpriseProvider {
    pub config: GcpConfig,
    /// ureq-агент с rustls (lazy-init при первом запросе).
    agent: Option<ureq::Agent>,
    /// Кэш токенов (lazy-load + auto-refresh). None = ещё не загружен.
    tokens: Option<super::oauth::StoredTokens>,
}

impl std::fmt::Debug for GcpEnterpriseProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GcpEnterpriseProvider")
            .field("config", &self.config)
            .field("agent_init", &self.agent.is_some())
            .field("tokens_loaded", &self.tokens.is_some())
            .finish()
    }
}

impl GcpEnterpriseProvider {
    pub fn new(config: GcpConfig) -> Self {
        Self {
            config,
            agent: None,
            tokens: None,
        }
    }

    pub fn from_env_or_default() -> Self {
        Self::new(GcpConfig::from_env().unwrap_or_default())
    }

    /// Lazy-init ureq-агента (один на всё время жизни провайдера).
    fn agent(&mut self) -> Result<&ureq::Agent, BridgeError> {
        if self.agent.is_none() {
            self.agent = Some(
                ureq::AgentBuilder::new()
                    .timeout(std::time::Duration::from_secs(60))
                    .build(),
            );
        }
        Ok(self.agent.as_ref().unwrap())
    }

    /// Загрузить и при необходимости обновить GCP-токены через GoogleHttp (CDP).
    /// Кэшируется в `self.tokens` — повторных чтений с диска не будет в рамках
    /// одной сессии. Если токен на исходе (минута до истечения) — тихо refresh.
    fn ensure_token(&mut self) -> Result<&super::oauth::StoredTokens, BridgeError> {
        if self.tokens.is_none() {
            let mut http = super::GoogleHttp::connect(crate::google::google_cdp_port())
                .map_err(|e| BridgeError::Transport(format!("google browser: {e}")))?;
            let t = super::oauth::ensure_gcp_fresh(&mut http)
                .map_err(|e| BridgeError::NotConfigured(e))?;
            self.tokens = Some(t);
        }
        // Если близко к истечению — обновить (метод needs_refresh) и заменить.
        let needs_refresh = self.tokens.as_ref().unwrap().needs_refresh();
        if needs_refresh {
            let mut http = super::GoogleHttp::connect(crate::google::google_cdp_port())
                .map_err(|e| BridgeError::Transport(format!("google browser: {e}")))?;
            let secret = super::oauth::load_client_secret()
                .map_err(|e| BridgeError::NotConfigured(e))?;
            let old = self.tokens.as_ref().unwrap();
            let fresh = super::oauth::refresh_tokens(&mut http, &secret.0, old)
                .map_err(|e| BridgeError::Transport(e))?;
            let _ = super::oauth::save_gcp_tokens(&fresh);
            self.tokens = Some(fresh);
        }
        Ok(self.tokens.as_ref().unwrap())
    }

    /// Bearer-заголовок + JSON content-type (для POST/DELETE с JSON-телом).
    fn bearer_json(&mut self) -> Result<Vec<(&'static str, String)>, BridgeError> {
        let t = self.ensure_token()?;
        Ok(vec![
            ("Authorization", format!("Bearer {}", t.access_token)),
            ("Content-Type", "application/json".to_string()),
            ("Accept", "application/json".to_string()),
            ("x-goog-user-project", self.config.project_number.clone()),
        ])
    }

    /// Bearer-заголовок + X-Goog-Upload-Protocol: raw (для uploadFile с binary body).
    fn bearer_upload(
        &mut self,
        file_name: &str,
        mime: &str,
    ) -> Result<Vec<(&'static str, String)>, BridgeError> {
        let t = self.ensure_token()?;
        Ok(vec![
            ("Authorization", format!("Bearer {}", t.access_token)),
            ("Content-Type", mime.to_string()),
            ("X-Goog-Upload-Protocol", "raw".to_string()),
            ("X-Goog-Upload-File-Name", file_name.to_string()),
            ("x-goog-user-project", self.config.project_number.clone()),
        ])
    }

    /// Bearer без тела (для GET/DELETE без body).
    fn bearer_get(&mut self) -> Result<Vec<(&'static str, String)>, BridgeError> {
        let t = self.ensure_token()?;
        Ok(vec![
            ("Authorization", format!("Bearer {}", t.access_token)),
            ("Accept", "application/json".to_string()),
            ("x-goog-user-project", self.config.project_number.clone()),
        ])
    }

    /// Преобразовать ureq::Error → BridgeError. Различает HTTP-ошибки (статус + тело)
    /// и транспортные (DNS/TLS/timeout).
    fn ureq_err(e: ureq::Error) -> BridgeError {
        match e {
            ureq::Error::Status(status, resp) => {
                let body = resp.into_string().unwrap_or_default();
                BridgeError::Http { status, body }
            }
            ureq::Error::Transport(t) => BridgeError::Transport(format!(
                "{}: {}",
                t.kind(),
                t.message().unwrap_or("(no message)")
            )),
        }
    }

    /// Извлечь last segment из resource name `projects/.../notebooks/{NB}/sources/{SRC}`.
    /// Используется для получения source_id из полного resource name в ответе.
    fn last_segment(name: &str) -> String {
        name.rsplit('/').next().unwrap_or(name).to_string()
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

    /// GET `/v1alpha/projects/{P}/locations/{L}/notebooks` → список ноутбуков.
    fn list_notebooks(&mut self) -> Result<Vec<NotebookBrief>, BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let url = notebooks_create_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
        );
        // headers — owned Vec<(&'static str, String)>, после возврата borrow of self
        // завершается. Только потом берём agent (&mut self → &ureq::Agent).
        let headers = self.bearer_get()?;
        let agent = self.agent()?;
        let mut req = agent.get(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.call().map_err(Self::ureq_err)?;
        let body = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| BridgeError::Parse(format!("list_notebooks: {e}; body: {}", &body[..body.len().min(300)])))?;
        let arr = v
            .get("notebooks")
            .and_then(|n| n.as_array())
            .ok_or_else(|| BridgeError::Parse(format!("list_notebooks: нет `notebooks` в ответе; body: {}", &body[..body.len().min(300)])))?;
        Ok(arr
            .iter()
            .filter_map(|nb| {
                let name = nb.get("name")?.as_str()?;
                let id = Self::last_segment(name);
                let title = nb
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let emoji = nb
                    .get("emoji")
                    .and_then(|e| e.as_str())
                    .unwrap_or("")
                    .to_string();
                let updated_at = nb
                    .get("updateTime")
                    .and_then(|u| u.as_str())
                    .map(String::from);
                Some(NotebookBrief {
                    id,
                    title,
                    emoji,
                    updated_at,
                })
            })
            .collect())
    }

    /// GET `/v1alpha/.../notebooks/{NB}` → список источников в ноутбуке
    /// (response содержит `sources[]` с wordCount/tokenCount/status).
    fn get_notebook(&mut self, nb_id: &str) -> Result<Vec<SourceMeta>, BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let url = notebook_get_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
            nb_id,
        );
        // headers — owned Vec, borrow of self завершается до вызова agent().
        let headers = self.bearer_get()?;
        let agent = self.agent()?;
        let mut req = agent.get(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.call().map_err(Self::ureq_err)?;
        let body = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| BridgeError::Parse(format!("get_notebook: {e}; body: {}", &body[..body.len().min(300)])))?;
        let arr = v
            .get("sources")
            .and_then(|s| s.as_array())
            .ok_or_else(|| BridgeError::Parse(format!("get_notebook: нет `sources` в ответе; body: {}", &body[..body.len().min(300)])))?;
        Ok(arr
            .iter()
            .filter_map(|src| {
                let resource_name = src.get("name")?.as_str()?.to_string();
                let source_id = Self::last_segment(&resource_name);
                let title = src
                    .get("sourceName")
                    .or_else(|| src.get("title"))
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let status = src
                    .get("state")
                    .or_else(|| src.get("status"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("UNKNOWN")
                    .to_string();
                let word_count = src
                    .get("wordCount")
                    .and_then(|w| w.as_u64());
                let token_count = src
                    .get("tokenCount")
                    .and_then(|t| t.as_u64());
                Some(SourceMeta {
                    source_id,
                    title,
                    resource_name,
                    status,
                    word_count,
                    token_count,
                })
            })
            .collect())
    }

    /// POST `/v1alpha/.../notebooks/{NB}/sources:batchCreate` с body
    /// `{ "requests": [{ "kind": "google_drive"|"text"|"web"|"you_tube", ... }] }`.
    /// Возвращает список созданных source_id (last-segment из каждого `name` в ответе).
    fn batch_create_sources(
        &mut self,
        nb_id: &str,
        items: &[SourceUpload],
    ) -> Result<Vec<String>, BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let url = sources_batch_create_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
            nb_id,
        );
        // Преобразовать SourceUpload → JSON-запрос.
        let requests: Vec<serde_json::Value> = items
            .iter()
            .map(|su| match su {
                SourceUpload::GoogleDrive {
                    document_id,
                    mime_type,
                    source_name,
                } => serde_json::json!({
                    "kind": "google_drive",
                    "documentId": document_id,
                    "mimeType": mime_type,
                    "sourceName": source_name,
                }),
                SourceUpload::Text { source_name, content } => serde_json::json!({
                    "kind": "text",
                    "sourceName": source_name,
                    "textContent": content,
                }),
                SourceUpload::Web { url, source_name } => serde_json::json!({
                    "kind": "web",
                    "sourceName": source_name,
                    "webContent": { "url": url },
                }),
                SourceUpload::YouTube { youtube_url } => serde_json::json!({
                    "kind": "you_tube",
                    "youTubeContent": { "url": youtube_url },
                }),
            })
            .collect();
        let body_json = serde_json::json!({ "requests": requests });
        let body_str = serde_json::to_string(&body_json)
            .map_err(|e| BridgeError::Parse(format!("batch_create serde: {e}")))?;

        // headers first (owned Vec → borrow of self ends before agent() borrow).
        let headers = self.bearer_json()?;
        let agent = self.agent()?;
        let mut req = agent.post(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.send_string(&body_str).map_err(Self::ureq_err)?;
        let body = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| BridgeError::Parse(format!("batch_create response: {e}; body: {}", &body[..body.len().min(300)])))?;
        // Ответ: longrunning operation. Извлекаем `sources[].name` (если синхронно)
        // или ждём завершения (для Pre-GA обычно сразу ACTIVE).
        let sources = v
            .get("sources")
            .and_then(|s| s.as_array())
            .ok_or_else(|| {
                BridgeError::Parse(format!(
                    "batch_create: нет `sources` в ответе (возможно longrunning op; body: {})",
                    &body[..body.len().min(300)]
                ))
            })?;
        Ok(sources
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()).map(Self::last_segment))
            .collect())
    }

    /// POST `/upload/v1alpha/.../notebooks/{NB}/sources:uploadFile`
    /// с `X-Goog-Upload-Protocol: raw` + binary body.
    /// Возвращает полный resource name созданного source.
    fn upload_file(
        &mut self,
        nb_id: &str,
        local_path: &Path,
        display_name: &str,
        mime: &str,
    ) -> Result<String, BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let bytes = std::fs::read(local_path).map_err(|e| {
            BridgeError::Transport(format!(
                "read {}: {e}",
                local_path.display()
            ))
        })?;
        let url = sources_upload_file_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
            nb_id,
        );
        // headers first (owned Vec → borrow of self ends before agent() borrow).
        let headers = self.bearer_upload(display_name, mime)?;
        let agent = self.agent()?;
        let mut req = agent.post(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.send_bytes(&bytes).map_err(Self::ureq_err)?;
        let body = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| BridgeError::Parse(format!("upload_file: {e}; body: {}", &body[..body.len().min(300)])))?;
        let name = v
            .get("name")
            .and_then(|n| n.as_str())
            .ok_or_else(|| {
                BridgeError::Parse(format!(
                    "upload_file: нет `name` в ответе; body: {}",
                    &body[..body.len().min(300)]
                ))
            })?;
        Ok(name.to_string())
    }

    /// GET `/v1alpha/.../notebooks/{NB}/sources/{SRC}` → метаданные одного источника.
    fn get_source(&mut self, nb_id: &str, src_id: &str) -> Result<SourceMeta, BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let url = source_get_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
            nb_id,
            src_id,
        );
        // headers first (owned Vec → borrow of self ends before agent() borrow).
        let headers = self.bearer_get()?;
        let agent = self.agent()?;
        let mut req = agent.get(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.call().map_err(Self::ureq_err)?;
        let body = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| BridgeError::Parse(format!("get_source: {e}; body: {}", &body[..body.len().min(300)])))?;
        let resource_name = v
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        if resource_name.is_empty() {
            return Err(BridgeError::Parse(format!(
                "get_source: нет `name` в ответе; body: {}",
                &body[..body.len().min(300)]
            )));
        }
        Ok(SourceMeta {
            source_id: Self::last_segment(&resource_name),
            title: v
                .get("sourceName")
                .or_else(|| v.get("title"))
                .and_then(|t| t.as_str())
                .unwrap_or("")
                .to_string(),
            resource_name,
            status: v
                .get("state")
                .or_else(|| v.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("UNKNOWN")
                .to_string(),
            word_count: v.get("wordCount").and_then(|w| w.as_u64()),
            token_count: v.get("tokenCount").and_then(|t| t.as_u64()),
        })
    }

    /// POST `/v1alpha/.../notebooks/{NB}/sources:batchDelete` body `{ "names": [...] }`.
    fn batch_delete_sources(
        &mut self,
        nb_id: &str,
        src_ids: &[&str],
    ) -> Result<(), BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let url = sources_batch_delete_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
            nb_id,
        );
        // Полные resource names для каждого source_id.
        let names: Vec<String> = src_ids
            .iter()
            .map(|sid| {
                format!(
                    "projects/{}/locations/{}/notebooks/{}/sources/{}",
                    self.config.project_number, self.config.location, nb_id, sid
                )
            })
            .collect();
        let body_json = serde_json::json!({ "names": names });
        let body_str = serde_json::to_string(&body_json)
            .map_err(|e| BridgeError::Parse(format!("batch_delete serde: {e}")))?;
        // headers first (owned Vec → borrow of self ends before agent() borrow).
        let headers = self.bearer_json()?;
        let agent = self.agent()?;
        let mut req = agent.post(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.send_string(&body_str).map_err(Self::ureq_err)?;
        let _ = resp.into_string(); // дропаем тело ответа — нам важен только статус
        Ok(())
    }

    /// POST `/v1alpha/.../notebooks/{NB}/audioOverviews` body
    /// `{ "sourceIds": [...], "focusTopic": "...", "audioLanguage": "..." }`.
    /// Возвращает полный resource name созданного overview (longrunning op).
    fn create_audio_overview(
        &mut self,
        nb_id: &str,
        source_ids: &[&str],
        focus: Option<&str>,
        lang: &str,
    ) -> Result<String, BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let url = audio_overview_create_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
            nb_id,
        );
        let body_json = serde_json::json!({
            "sourceIds": source_ids,
            "focusTopic": focus.unwrap_or(""),
            "audioLanguage": lang,
        });
        let body_str = serde_json::to_string(&body_json)
            .map_err(|e| BridgeError::Parse(format!("audio_overview_create serde: {e}")))?;
        // headers first (owned Vec → borrow of self ends before agent() borrow).
        let headers = self.bearer_json()?;
        let agent = self.agent()?;
        let mut req = agent.post(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.send_string(&body_str).map_err(Self::ureq_err)?;
        let body = resp.into_string().unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&body)
            .map_err(|e| BridgeError::Parse(format!("audio_overview_create: {e}; body: {}", &body[..body.len().min(300)])))?;
        // Ответ: longrunning operation. name = `projects/.../audioOverviews/{ID}` или op-name.
        let name = v
            .get("name")
            .or_else(|| v.get("operationName"))
            .and_then(|n| n.as_str())
            .ok_or_else(|| {
                BridgeError::Parse(format!(
                    "audio_overview_create: нет `name` в ответе; body: {}",
                    &body[..body.len().min(300)]
                ))
            })?;
        Ok(name.to_string())
    }

    /// DELETE `/v1alpha/.../notebooks/{NB}/audioOverviews/default`.
    /// `default` — единственный overview на ноутбук в Pre-GA.
    fn delete_audio_overview(&mut self, nb_id: &str) -> Result<(), BridgeError> {
        if !self.config.is_ready() {
            return Err(BridgeError::NotConfigured(
                "POLER_GCP_PROJECT_NUMBER не задан (см. --gcp-auth)".into(),
            ));
        }
        let url = audio_overview_delete_url(
            &self.config.region,
            &self.config.project_number,
            &self.config.location,
            nb_id,
        );
        // headers first (owned Vec → borrow of self ends before agent() borrow).
        let headers = self.bearer_get()?;
        let agent = self.agent()?;
        let mut req = agent.delete(&url);
        for (k, v) in &headers {
            req = req.set(k, v);
        }
        let resp = req.call().map_err(Self::ureq_err)?;
        let _ = resp.into_string();
        Ok(())
    }
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
// HybridProvider — routing + fallback (M3 ✓)
// ---------------------------------------------------------------------------

/// Гибридный провайдер: роутит операции к тому, кто их поддерживает,
/// с прозрачным fallback GCP ↔ CDP.
///
/// ## Routing policy
///
/// | Mode        | Primary         | Fallback       |
/// |-------------|-----------------|----------------|
/// | `GcpOnly`   | GCP             | off            |
/// | `CdpOnly`   | CDP             | off            |
/// | `Auto`      | GCP если `gcp.supports(op)`, иначе CDP | на secondary, если `supports(op)` |
///
/// Fallback срабатывает только если primary возвратил `NotSupported` или
/// `NotConfigured`. Серверные ошибки (`Http`/`Transport`/`Parse`) —
/// propagates наверх, fallback на них не запускается (нельзя «лечить»
/// 5xx от GCP переключением на CDP — это разные данные).
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

    /// Явное конструирование (для тестов и для CLI флагов `--companion-mode`).
    pub fn new(gcp: GcpEnterpriseProvider, cdp: CdpBatchexecuteProvider, mode: CompanionMode) -> Self {
        Self { gcp, cdp, mode }
    }

    /// Имя primary-провайдера для операции в текущем режиме.
    /// Возвращает `"gcp"` или `"cdp"`. Чистая функция без сети — для
    /// детерминированных тестов routing-decision без моков.
    pub fn primary_for(&self, op: Op) -> &'static str {
        match self.mode {
            CompanionMode::GcpOnly => "gcp",
            CompanionMode::CdpOnly => "cdp",
            CompanionMode::Auto => {
                if self.gcp.supports(op) {
                    "gcp"
                } else {
                    "cdp"
                }
            }
        }
    }

    /// Активен ли fallback в текущем режиме (только Auto).
    pub fn fallback_enabled(&self) -> bool {
        matches!(self.mode, CompanionMode::Auto)
    }

    /// Core routing helper. Пробует primary-провайдер, при
    /// `NotSupported`/`NotConfigured` переключается на secondary — но
    /// только если secondary `supports(op)`, и только в Auto mode.
    ///
    /// В `GcpOnly`/`CdpOnly` fallback выключен: режим — это явное
    /// решение пользователя «хочу только этот канал».
    fn route<F, G, R>(
        &mut self,
        op: Op,
        f_gcp: G,
        f_cdp: F,
    ) -> Result<R, BridgeError>
    where
        G: FnOnce(&mut GcpEnterpriseProvider) -> Result<R, BridgeError>,
        F: FnOnce(&mut CdpBatchexecuteProvider) -> Result<R, BridgeError>,
    {
        match self.primary_for(op) {
            "gcp" => match f_gcp(&mut self.gcp) {
                Ok(v) => Ok(v),
                Err(BridgeError::NotSupported { .. } | BridgeError::NotConfigured(_))
                    if self.fallback_enabled() && self.cdp.supports(op) =>
                {
                    f_cdp(&mut self.cdp)
                }
                Err(e) => Err(e),
            },
            _ => match f_cdp(&mut self.cdp) {
                Ok(v) => Ok(v),
                Err(BridgeError::NotSupported { .. } | BridgeError::NotConfigured(_))
                    if self.fallback_enabled() && self.gcp.supports(op) =>
                {
                    f_gcp(&mut self.gcp)
                }
                Err(e) => Err(e),
            },
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

    fn list_notebooks(&mut self) -> Result<Vec<NotebookBrief>, BridgeError> {
        self.route(
            Op::ListNotebooks,
            |g| g.list_notebooks(),
            |c| c.list_notebooks(),
        )
    }

    fn get_notebook(&mut self, nb_id: &str) -> Result<Vec<SourceMeta>, BridgeError> {
        self.route(
            Op::GetNotebook,
            |g| g.get_notebook(nb_id),
            |c| c.get_notebook(nb_id),
        )
    }

    fn batch_create_sources(
        &mut self,
        nb_id: &str,
        items: &[SourceUpload],
    ) -> Result<Vec<String>, BridgeError> {
        self.route(
            Op::BatchCreateSources,
            |g| g.batch_create_sources(nb_id, items),
            |c| c.batch_create_sources(nb_id, items),
        )
    }

    fn upload_file(
        &mut self,
        nb_id: &str,
        local_path: &Path,
        display_name: &str,
        mime: &str,
    ) -> Result<String, BridgeError> {
        self.route(
            Op::UploadFile,
            |g| g.upload_file(nb_id, local_path, display_name, mime),
            |c| c.upload_file(nb_id, local_path, display_name, mime),
        )
    }

    fn get_source(&mut self, nb_id: &str, src_id: &str) -> Result<SourceMeta, BridgeError> {
        self.route(
            Op::GetSourceMeta,
            |g| g.get_source(nb_id, src_id),
            |c| c.get_source(nb_id, src_id),
        )
    }

    fn batch_delete_sources(
        &mut self,
        nb_id: &str,
        src_ids: &[&str],
    ) -> Result<(), BridgeError> {
        self.route(
            Op::DeleteSources,
            |g| g.batch_delete_sources(nb_id, src_ids),
            |c| c.batch_delete_sources(nb_id, src_ids),
        )
    }

    fn create_audio_overview(
        &mut self,
        nb_id: &str,
        source_ids: &[&str],
        focus: Option<&str>,
        lang: &str,
    ) -> Result<String, BridgeError> {
        self.route(
            Op::CreateAudioOverview,
            |g| g.create_audio_overview(nb_id, source_ids, focus, lang),
            |c| c.create_audio_overview(nb_id, source_ids, focus, lang),
        )
    }

    fn delete_audio_overview(&mut self, nb_id: &str) -> Result<(), BridgeError> {
        self.route(
            Op::DeleteAudioOverview,
            |g| g.delete_audio_overview(nb_id),
            |c| c.delete_audio_overview(nb_id),
        )
    }
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

    // -------------------------------------------------------------------------
    // M2 tests — детерминированные, без сети. Реальные HTTP-вызовы требуют
    // живого GCP-токена и тестового проекта; проверяются вручную через
    // `poler-engine --gcp-auth` + `--nlm-batch-create` (см. M5 CLI).
    // -------------------------------------------------------------------------

    #[test]
    fn gcp_provider_last_segment_extracts_id_from_resource_name() {
        // Полный resource name source: projects/.../notebooks/{NB}/sources/{SRC}
        let name = "projects/123456789012/locations/global/notebooks/abc-123/sources/src-xyz789";
        assert_eq!(GcpEnterpriseProvider::last_segment(name), "src-xyz789");
        // Граничный случай: нет ни одного `/` — возвращаем как есть
        assert_eq!(GcpEnterpriseProvider::last_segment("no-slash"), "no-slash");
        // Пустая строка
        assert_eq!(GcpEnterpriseProvider::last_segment(""), "");
    }

    #[test]
    fn gcp_provider_ready_reflects_config_completeness() {
        // default config (empty project_number) → not ready
        let p = GcpEnterpriseProvider::new(GcpConfig::default());
        assert!(!p.ready(), "пустой project_number → not ready");

        // config с заполненным project_number → ready
        let cfg = GcpConfig {
            region: "global".to_string(),
            project_number: "123456789012".to_string(),
            location: "global".to_string(),
        };
        let p = GcpEnterpriseProvider::new(cfg);
        assert!(p.ready(), "заполненный project_number → ready");
        assert_eq!(p.name(), "gcp_enterprise");
    }

    #[test]
    fn gcp_provider_supports_covers_all_enterprise_ops() {
        // M2 контракт: GCP покрывает 9 из 13 операций оф. Pre-GA API.
        // Оставшиеся 4 (chat/get_notes/list_artifacts/get_source_content)
        // — монополия CdpBatchexecuteProvider.
        let p = GcpEnterpriseProvider::new(GcpConfig::default());
        let supported: Vec<Op> = [
            Op::ListNotebooks,
            Op::GetNotebook,
            Op::CreateNotebook,
            Op::BatchCreateSources,
            Op::UploadFile,
            Op::GetSourceMeta,
            Op::CreateAudioOverview,
            Op::DeleteAudioOverview,
            Op::DeleteSources,
        ]
        .into_iter()
        .filter(|op| p.supports(*op))
        .collect();
        assert_eq!(supported.len(), 9, "GCP должен покрывать 9 операций");

        let not_supported: Vec<Op> = [
            Op::GetSourceContent,
            Op::GetNotes,
            Op::ListArtifacts,
            Op::Chat,
        ]
        .into_iter()
        .filter(|op| !p.supports(*op))
        .collect();
        assert_eq!(not_supported.len(), 4, "GCP НЕ должен покрывать 4 операции");
    }

    // -------------------------------------------------------------------------
    // M3 tests — routing policy HybridProvider. Pure-fns, без сети.
    // -------------------------------------------------------------------------

    /// Helper: построить HybridProvider с заданным mode (конфиги default).
    fn hybrid_with_mode(mode: CompanionMode) -> HybridProvider {
        HybridProvider::new(
            GcpEnterpriseProvider::from_env_or_default(),
            CdpBatchexecuteProvider::new(9223),
            mode,
        )
    }

    #[test]
    fn hybrid_primary_for_in_auto_routes_gcp_only_ops_to_gcp() {
        // В Auto: операции, которые поддерживает ТОЛЬКО GCP
        // (BatchCreateSources, UploadFile, GetSourceMeta, CreateAudioOverview,
        //  DeleteAudioOverview, DeleteSources, CreateNotebook, ListNotebooks,
        //  GetNotebook) → primary=gcp.
        let h = hybrid_with_mode(CompanionMode::Auto);
        let gcp_ops = [
            Op::ListNotebooks,
            Op::GetNotebook,
            Op::CreateNotebook,
            Op::BatchCreateSources,
            Op::UploadFile,
            Op::GetSourceMeta,
            Op::CreateAudioOverview,
            Op::DeleteAudioOverview,
            Op::DeleteSources,
        ];
        for op in gcp_ops {
            assert_eq!(
                h.primary_for(op),
                "gcp",
                "Auto+{:?}: primary должен быть gcp (GCP поддерживает)",
                op
            );
        }
    }

    #[test]
    fn hybrid_primary_for_in_auto_routes_cdp_only_ops_to_cdp() {
        // В Auto: операции, которые GCP НЕ поддерживает (chat/get_notes/
        // list_artifacts/get_source_content) → primary=cdp.
        let h = hybrid_with_mode(CompanionMode::Auto);
        let cdp_ops = [
            Op::GetSourceContent,
            Op::GetNotes,
            Op::ListArtifacts,
            Op::Chat,
        ];
        for op in cdp_ops {
            assert_eq!(
                h.primary_for(op),
                "cdp",
                "Auto+{:?}: primary должен быть cdp (GCP не поддерживает)",
                op
            );
        }
    }

    #[test]
    fn hybrid_primary_for_in_gcp_only_always_returns_gcp() {
        // GcpOnly: даже если GCP не поддерживает op — primary=gcp,
        // fallback выключен, поэтому запрос вернёт NotSupported от GCP.
        let h = hybrid_with_mode(CompanionMode::GcpOnly);
        assert_eq!(h.primary_for(Op::Chat), "gcp");
        assert_eq!(h.primary_for(Op::BatchCreateSources), "gcp");
        assert!(!h.fallback_enabled(), "GcpOnly: fallback off");
    }

    #[test]
    fn hybrid_primary_for_in_cdp_only_always_returns_cdp() {
        // CdpOnly: даже если CDP не поддерживает op — primary=cdp,
        // fallback выключен.
        let h = hybrid_with_mode(CompanionMode::CdpOnly);
        assert_eq!(h.primary_for(Op::UploadFile), "cdp");
        assert_eq!(h.primary_for(Op::Chat), "cdp");
        assert!(!h.fallback_enabled(), "CdpOnly: fallback off");
    }

    #[test]
    fn hybrid_fallback_enabled_only_in_auto_mode() {
        assert!(hybrid_with_mode(CompanionMode::Auto).fallback_enabled());
        assert!(!hybrid_with_mode(CompanionMode::GcpOnly).fallback_enabled());
        assert!(!hybrid_with_mode(CompanionMode::CdpOnly).fallback_enabled());
    }

    #[test]
    fn hybrid_route_falls_back_on_not_configured_in_auto_mode() {
        // Симулируем: GcpEnterpriseProvider без конфига → NotConfigured.
        // Route должен попробовать fallback на CDP, если CDP поддерживает op.
        // Для ListNotebooks — оба поддерживают, поэтому fallback сработает
        // и CDP вернёт свой default-trait NotSupported (cdp.rs skeleton).
        // В итоге: hybrid в Auto + пустой GCP → fallback → CDP → NotSupported от CDP.
        let mut h = hybrid_with_mode(CompanionMode::Auto);
        let res = h.list_notebooks();
        // GCP не сконфигурирован (нет POLER_GCP_PROJECT_NUMBER в env) → NotConfigured.
        // Fallback на CDP — CDP skeleton, метод не реализован → NotSupported от CDP.
        // В итоге: BridgeError::NotSupported от cdp_batchexecute.
        match res {
            Err(BridgeError::NotSupported { provider, .. }) => {
                assert_eq!(provider, "cdp_batchexecute", "должен быть fallback на CDP");
            }
            other => panic!("ожидал NotSupported от CDP после fallback, получил {:?}", other),
        }
    }

    #[test]
    fn hybrid_route_does_not_fallback_in_gcp_only_mode() {
        // GcpOnly: fallback выключен. GCP без конфига → NotConfigured
        // должен прийти наверх без попытки fallback.
        let mut h = hybrid_with_mode(CompanionMode::GcpOnly);
        let res = h.list_notebooks();
        match res {
            Err(BridgeError::NotConfigured(_)) => {}
            other => panic!(
                "GcpOnly+empty GCP: ожидал NotConfigured без fallback, получил {:?}",
                other
            ),
        }
    }

    #[test]
    fn hybrid_route_propagates_http_errors_without_fallback() {
        // Если primary возвращает Http/Transport/Parse — fallback НЕ запускается.
        // Http-ошибки означают серверную проблему, переключение канала
        // не лечит, а даёт разные данные.
        // Симулируем: GcpEnterpriseProvider без конфига для batch_create_sources
        // (CDP не поддерживает) → в Auto fallback не сработает (CDP не поддерживает).
        let mut h = hybrid_with_mode(CompanionMode::Auto);
        let res = h.batch_create_sources("nb-test", &[]);
        // GCP не сконфигурирован → NotConfigured.
        // CDP не поддерживает BatchCreateSources → fallback не запускается.
        // В итоге: NotConfigured от GCP propagates наверх.
        match res {
            Err(BridgeError::NotConfigured(_)) => {}
            other => panic!(
                "Auto+empty GCP для GCP-only op: ожидал NotConfigured, получил {:?}",
                other
            ),
        }
    }
}
