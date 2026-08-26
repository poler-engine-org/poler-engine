//! Shell-state: shared state for the interactive `poler>` REPL and `--tui`
//! Dashboard. Holds lazily-opened `WebIndex` + `NlmSession` so multiple
//! commands in one session reuse the same DB connection and NLM RPC session
//! (saves ~1 s per command over the standalone CLI dispatch).
//!
//! Архитектурный инвариант v0.15.0: ядро движка (`poler_engine::*`) не
//! трогается — `Shell` только собирает указатели на существующие функции
//! и форматирует вывод.

use std::path::PathBuf;
use std::sync::OnceLock;

use crate::google::nlm::NlmSession;
use crate::web::WebIndex;
use crate::notes::{self, NoteSource};
use crate::sources;

/// Путь к каталогу кэша poler-engine (`~/.cache/poler-engine/`).
pub fn cache_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    PathBuf::from(home).join(".cache").join("poler-engine")
}

/// Путь к файлу истории команд REPL (`~/.cache/poler-engine/shell-history.txt`).
pub fn history_path() -> PathBuf {
    cache_dir().join("shell-history.txt")
}

/// Состояние одной интерактивной сессии `poler-shell`.
///
/// `db_path` фиксируется при запуске (по умолчанию
/// `crate::web::default_db_path()`); `WebIndex` и `NlmSession`
/// открываются **лениво** — первый `search`/`stats`/`nlm sync` открывает БД,
/// первый `nlm list/notes/...` открывает RPC-сессию. После этого
/// переиспользуются до выхода из шелла.
pub struct ShellState {
    db_path: PathBuf,
    /// Ленимо открываемый web-index. None = ещё не открывали.
    ix: Option<WebIndex>,
    /// Ленимо открываемая сессия NotebookLM. None = ещё не открывали.
    nlm: Option<NlmSession>,
    /// Ленимо открываемое соединение к poler_notes (та же БД, другая таблица).
    notes_conn: Option<rusqlite::Connection>,
    /// Ленимо открываемое соединение к poler_sources (та же БД).
    sources_conn: Option<rusqlite::Connection>,
    /// Текущий формат вывода (переключается `poler> set format md|json|ai-json`).
    pub format: OutputFormat,
    /// Top-K по умолчанию для `search` (переключается `set top N`).
    pub top: usize,
    /// Последний ответ команды (для TUI: правая нижняя панель показывает это).
    pub last_output: String,
    /// v0.17.0: Последний ответ `nlm ask` для Ctrl+S → notes::add_note(source=ai-reply).
    pub last_ai_reply: Option<(String, Option<String>)>,
    /// v0.17.0: ID текущего активного ноутбука (после клика в TUI).
    pub active_notebook_id: Option<String>,
}

/// Формат вывода, как в CLI `--format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[derive(Default)]
pub enum OutputFormat {
    /// JSON для LLM-агентов (по умолчанию).
    AiJson,
    /// Markdown для человека.
    #[default]
    Md,
    /// Простой текст.
    Simple,
}


impl ShellState {
    /// Создать состояние шелла с путём к web-index.db.
    pub fn new(db_path: PathBuf) -> Self {
        Self {
            db_path,
            ix: None,
            nlm: None,
            notes_conn: None,
            sources_conn: None,
            format: OutputFormat::default(),
            top: 10,
            last_output: String::new(),
            last_ai_reply: None,
            active_notebook_id: None,
        }
    }

    /// Путь к web-index.db (для статус-бара TUI).
    pub fn db_path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Список доступных команд для Tab-completion и `help`.
    pub fn commands() -> &'static [&'static str] {
        static COMMANDS: OnceLock<Vec<&'static str>> = OnceLock::new();
        COMMANDS.get_or_init(|| {
            vec![
                "help", "quit", "exit", "?",
                "search", "web", "stats",
                "nlm", "sync",
                "crawl", "impact",
                "set", "version",
                // v0.16.0: Unified VCS & Data Mesh
                "gh", "gl", "gt", "gix",
                // v0.17.0: Notes & Sources CRUD
                "notes", "sources",
            ]
        })
    }

    /// Подкоманды `nlm ...` для Tab-completion.
    pub fn nlm_subcommands() -> &'static [&'static str] {
        &["list", "notes", "artifacts", "source", "account", "ask", "sync", "shot", "media"]
    }

    /// v0.16.0: Подкоманды `gh`/`gl`/`gt` (одинаковые для всех REST-адаптеров).
    pub fn vcs_subcommands() -> &'static [&'static str] {
        &["search", "repos", "commits", "issues"]
    }

    /// v0.16.0: Подкоманды `gix` (Pure-Rust git).
    pub fn gix_subcommands() -> &'static [&'static str] {
        &["log", "clone"]
    }

    /// v0.16.0: Схемы VCS для `sync vcs <scheme>` completion.
    pub fn vcs_schemes() -> &'static [&'static str] {
        &["gh", "gl", "gt", "gix", "all"]
    }

    /// Именованные параметры `set ...` для Tab-completion.
    pub fn set_keys() -> &'static [&'static str] {
        &["format", "top"]
    }

    /// v0.17.0: Подкоманды `notes ...` для Tab-completion.
    pub fn notes_subcommands() -> &'static [&'static str] {
        &["list", "add", "show", "edit", "rm", "save-from-ai"]
    }

    /// v0.17.0: Подкоманды `sources ...` для Tab-completion.
    pub fn sources_subcommands() -> &'static [&'static str] {
        &["list", "add", "rm", "test", "open"]
    }

    /// v0.17.0: Подкоманды `sources add --kind ...`.
    pub fn source_kinds() -> &'static [&'static str] {
        &["file", "url", "repo"]
    }

    /// Заимствовать `&mut WebIndex`, открыв ленимо если нужно.
    pub fn ensure_index(&mut self) -> Result<&mut WebIndex, String> {
        if self.ix.is_none() {
            let ix = WebIndex::open(&self.db_path)
                .map_err(|e| format!("web-index {:?}: {e}", self.db_path))?;
            self.ix = Some(ix);
        }
        Ok(self.ix.as_mut().expect("ix just set"))
    }

    /// Заимствовать `&mut NlmSession`, открыв ленимо если нужно.
    pub fn ensure_nlm(&mut self) -> Result<&mut NlmSession, String> {
        if self.nlm.is_none() {
            let s = NlmSession::open().map_err(|e| format!("NlmSession::open: {e}"))?;
            self.nlm = Some(s);
        }
        Ok(self.nlm.as_mut().expect("nlm just set"))
    }

    /// Установить формат вывода по имени.
    pub fn set_format(&mut self, name: &str) -> Result<(), String> {
        self.format = match name.to_ascii_lowercase().as_str() {
            "md" | "markdown" => OutputFormat::Md,
            "json" | "ai-json" | "aijson" => OutputFormat::AiJson,
            "simple" | "text" | "txt" => OutputFormat::Simple,
            other => return Err(format!("неизвестный формат: {other} (md|json|simple)")),
        };
        Ok(())
    }

    /// Записать последний вывод (для отображения в TUI).
    pub fn set_output(&mut self, s: impl Into<String>) {
        self.last_output = s.into();
    }

    /// v0.17.0: Ленимо открыть соединение к poler_notes (та же БД, таблица poler_notes).
    pub fn ensure_notes_conn(&mut self) -> Result<&mut rusqlite::Connection, String> {
        if self.notes_conn.is_none() {
            self.notes_conn = Some(notes::open(&self.db_path)?);
        }
        Ok(self.notes_conn.as_mut().expect("notes_conn just set"))
    }

    /// v0.17.0: Ленимо открыть соединение к poler_sources (та же БД).
    pub fn ensure_sources_conn(&mut self) -> Result<&mut rusqlite::Connection, String> {
        if self.sources_conn.is_none() {
            self.sources_conn = Some(sources::open(&self.db_path)?);
        }
        Ok(self.sources_conn.as_mut().expect("sources_conn just set"))
    }

    /// v0.17.0: Запомнить последний ответ `nlm ask` для последующего Ctrl+S.
    /// Аргументы: (ответ, опц. notebook_id).
    pub fn remember_ai_reply(&mut self, reply: impl Into<String>, notebook_id: Option<String>) {
        self.last_ai_reply = Some((reply.into(), notebook_id));
    }

    /// v0.17.0: Сохранить последний AI-ответ как заметку (source=ai-reply).
    /// Возвращает id новой заметки.
    pub fn save_last_ai_reply_as_note(&mut self, title: &str) -> Result<i64, String> {
        let (reply, nb) = self
            .last_ai_reply
            .clone()
            .ok_or_else(|| "нет последнего AI-ответа (сначала выполните `nlm ask`)".to_string())?;
        let conn = self.ensure_notes_conn()?;
        notes::add_note(
            conn,
            title,
            &reply,
            &["ai-reply".into()],
            NoteSource::AiReply,
            nb.as_deref(),
        )
    }

    /// v0.17.0: Установить активный ноутбук (после клика в TUI).
    pub fn set_active_notebook(&mut self, id: Option<String>) {
        self.active_notebook_id = id;
    }

    /// v0.17.0: Получить активный ноутбук.
    pub fn active_notebook(&self) -> Option<&str> {
        self.active_notebook_id.as_deref()
    }
}



impl ShellState {
    /// Одномоментно заимствовать `&mut WebIndex` и `&mut NlmSession` для
    /// команд, которым нужны оба (например, `nlm sync`). Обходит ошибку
    /// borrow-checker'а "cannot borrow state as mut twice" — внутри closure
    /// оба `&mut` живут одновременно.
    ///
    /// Лениво открывает ресурсы при необходимости. Если один открылся, а
    /// второй нет — оставляет первый открытым для последующих команд.
    pub fn with_nlm_index<R, E>(
        &mut self,
        f: impl FnOnce(&mut WebIndex, &mut NlmSession) -> Result<R, E>,
    ) -> Result<R, E>
    where
        E: From<String>,
    {
        if self.ix.is_none() {
            match WebIndex::open(&self.db_path) {
                Ok(i) => self.ix = Some(i),
                Err(e) => return Err(format!("web-index {:?}: {e}", self.db_path).into()),
            }
        }
        if self.nlm.is_none() {
            match NlmSession::open() {
                Ok(n) => self.nlm = Some(n),
                Err(e) => return Err(format!("NlmSession::open: {e}").into()),
            }
        }
        // take + put-back patern: безопасно получаем два &mut одновременно
        let mut ix = self.ix.take().expect("ix just ensured");
        let mut nlm = self.nlm.take().expect("nlm just ensured");
        let r = f(&mut ix, &mut nlm);
        self.ix = Some(ix);
        self.nlm = Some(nlm);
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_state_lazy_open() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("test.db");
        let mut s = ShellState::new(db.clone());
        assert!(s.ix.is_none(), "ix should be None until first call");
        let _ = s.ensure_index();
        assert!(s.ix.is_some(), "ix should be Some after ensure_index");
    }

    #[test]
    fn set_format_parsing() {
        let mut s = ShellState::new(PathBuf::from("/tmp/x.db"));
        assert_eq!(s.format, OutputFormat::Md);
        s.set_format("json").unwrap();
        assert_eq!(s.format, OutputFormat::AiJson);
        s.set_format("markdown").unwrap();
        assert_eq!(s.format, OutputFormat::Md);
        s.set_format("text").unwrap();
        assert_eq!(s.format, OutputFormat::Simple);
        assert!(s.set_format("xml").is_err(), "xml should be rejected");
    }

    #[test]
    fn commands_list_is_stable() {
        let cmds = ShellState::commands();
        assert!(cmds.contains(&"search"));
        assert!(cmds.contains(&"nlm"));
        assert!(cmds.contains(&"quit"));
        assert!(cmds.contains(&"exit"));
    }

    #[test]
    fn nlm_subcommands_complete_set() {
        let sub = ShellState::nlm_subcommands();
        assert!(sub.contains(&"list"));
        assert!(sub.contains(&"notes"));
        assert!(sub.contains(&"sync"));
        assert!(sub.contains(&"ask"));
    }
}
