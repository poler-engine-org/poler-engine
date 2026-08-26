//! # poler-shell v0.15.0 — интерактивный терминал poler-engine
//!
//! Две поверхности для человеческого удобства поверх существующих режимов
//! движка (ядро `poler_engine::*` не трогается):
//!
//! 1. **REPL `poler-engine --shell`** (rustyline) — быстрый командный режим
//!    с Tab-completion и историей `↑/↓`, без перезапуска процесса:
//!    ```text
//!    poler> search "Касіопея Astra-Nic" --top 5
//!    poler> nlm ask 704f2610... "Параметры Планковской геодезической"
//!    poler> nlm sync 704f2610...
//!    poler> stats
//!    poler> set format json
//!    poler> quit
//!    ```
//! 2. **TUI Dashboard `poler-engine --tui`** (ratatui + crossterm) — 3-панельный
//!    layout: левая (ноутбуки), правая верх (поле ввода), правая нижняя
//!    (результаты с прокруткой). Tab — смена фокуса, Esc — выход.
//!
//! ## Архитектурные инварианты v0.15.0
//!
//! - **Ноль изменений в ядре** — `poler_engine::*` остаётся как в v0.14.0.
//!   Shell только заимствует `WebIndex`/`NlmSession`/`nlm_ingest`/`nlm::*`
//!   и форматирует вывод для человека.
//! - **Ленивое открытие ресурсов** — `WebIndex` и `NlmSession` открываются
//!   только при первом использовании (первый `search`/`stats` открывает БД,
//!   первый `nlm list/notes/...` открывает RPC-сессию). После этого
//!   переиспользуются до выхода из шелла.
//! - **История команд** сохраняется в `~/.cache/poler-engine/shell-history.txt`
//!   (до 2000 команд, max_history по умолчанию в rustyline).
//! - **MCP `poler_nlm` 9 actions** из v0.14.0 остаются как есть (shell не
//!   трогает MCP-сервер).

pub mod commands;
pub mod completer;
pub mod state;
pub mod tui;

pub use commands::{dispatch, run_shell, tokenize, CmdResult};
pub use state::{ShellState, OutputFormat};
pub use tui::run_tui;

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn tokenize_handles_quoted_args() {
        let t = tokenize("nlm ask 704f \"вопрос с пробелами\"");
        assert_eq!(t, vec!["nlm", "ask", "704f", "вопрос с пробелами"]);
    }

    #[test]
    fn state_default_format_is_md_for_humans() {
        let s = ShellState::new(PathBuf::from("/tmp/x.db"));
        assert_eq!(s.format, OutputFormat::Md, "interactive shell defaults to markdown for humans");
    }

    #[test]
    fn commands_dispatch_unknown_returns_message() {
        let mut s = ShellState::new(PathBuf::from("/tmp/x.db"));
        match dispatch(&mut s, "zzzbadcmd") {
            CmdResult::Done(out) => assert!(out.contains("неизвестная команда")),
            _ => panic!(),
        }
    }
}
