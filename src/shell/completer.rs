//! Tab-completion для REPL poler-shell. Реализует `rustyline::Completer`
//! поверх списка команд `ShellState::commands()` + `nlm_subcommands()` +
//! `set_keys()`. Завершает первое слово команды и подкоманды `nlm`/`set`.

use rustyline::completion::Completer;

use super::state::ShellState;

/// Комплетер poler-shell: завершает команды и подкоманды NLM/set.
pub struct PolerCompleter;

impl Default for PolerCompleter {
    fn default() -> Self {
        Self
    }
}

static CMD_HINTS: &[(&str, &str)] = &[
    ("search", "search \"<query>\" [--top N]"),
    ("web", "web \"<query>\" [--top N]"),
    ("stats", "stats"),
    ("nlm", "nlm list|notes|artifacts|source|account|ask|sync"),
    ("sync", "sync (синк NLM в web-index)"),
    ("set", "set format|top <value>"),
    ("help", "help"),
    ("quit", "quit"),
    ("exit", "exit"),
    ("version", "version"),
];

/// Чистая функция: вернуть список кандидатов для замены `prefix`.
/// Тестируется без rustyline Context (через `complete_prefix`).
pub fn complete_prefix(prefix: &str) -> (usize, Vec<String>) {
    let tokens = prefix.split_whitespace().collect::<Vec<_>>();
    if tokens.is_empty() {
        return (
            0,
            ShellState::commands().iter().map(|s| s.to_string()).collect(),
        );
    }
    let first = tokens[0];
    let in_first_token = !prefix.ends_with(' ');

    // Печатаем первое слово, курсор в нём
    if tokens.len() == 1 && in_first_token {
        let cands: Vec<String> = ShellState::commands()
            .iter()
            .filter(|c| c.starts_with(first))
            .map(|c| (*c).to_string())
            .collect();
        return (prefix.len() - first.len(), cands);
    }

    // Команда `nlm` — завершаем подкоманду
    if first == "nlm" {
        let sub_prefix = if tokens.len() >= 2 && in_first_token {
            tokens[1]
        } else {
            ""
        };
        let cands: Vec<String> = ShellState::nlm_subcommands()
            .iter()
            .filter(|s| s.starts_with(sub_prefix))
            .map(|s| (*s).to_string())
            .collect();
        let replace_from = prefix.find("nlm ").map(|i| i + 4).unwrap_or(prefix.len());
        return (replace_from, cands);
    }

    // Команда `set` — завершаем ключ
    if first == "set" {
        let sub_prefix = if tokens.len() >= 2 && in_first_token {
            tokens[1]
        } else {
            ""
        };
        let cands: Vec<String> = ShellState::set_keys()
            .iter()
            .filter(|s| s.starts_with(sub_prefix))
            .map(|s| (*s).to_string())
            .collect();
        let replace_from = prefix.find("set ").map(|i| i + 4).unwrap_or(prefix.len());
        return (replace_from, cands);
    }

    (prefix.len(), Vec::new())
}

impl Completer for PolerCompleter {
    type Candidate = String;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &rustyline::Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Self::Candidate>)> {
        let prefix = &line[..pos.min(line.len())];
        Ok(complete_prefix(prefix))
    }
}

impl rustyline::hint::Hinter for PolerCompleter {
    type Hint = String;

    fn hint(&self, line: &str, _pos: usize, _ctx: &rustyline::Context<'_>) -> Option<Self::Hint> {
        hint_for_line(line)
    }
}

/// Чистая функция: вернуть подсказку для строки ввода (используется и Hinter'ом).
pub fn hint_for_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    let first = trimmed.split_whitespace().next()?;
    CMD_HINTS.iter().find_map(|(k, v)| {
        if first == *k {
            Some(format!("  # {}", v))
        } else {
            None
        }
    })
}



impl rustyline::highlight::Highlighter for PolerCompleter {}

impl rustyline::validate::Validator for PolerCompleter {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_first_token_search() {
        let (start, cands) = complete_prefix("sear");
        assert!(cands.iter().any(|c| c == "search"), "search should complete from 'sear'");
        assert_eq!(start, 0);
    }

    #[test]
    fn complete_nlm_subcommands() {
        let (start, cands) = complete_prefix("nlm l");
        assert!(cands.iter().any(|c| c == "list"));
        assert!(!cands.iter().any(|c| c == "account"), "account doesn't start with 'l'");
        assert!(start >= 4);
    }

    #[test]
    fn complete_nlm_after_space() {
        // "nlm " — пробел после nlm, ожидаем все подкоманды
        let (_, cands) = complete_prefix("nlm ");
        assert!(cands.iter().any(|c| c == "list"));
        assert!(cands.iter().any(|c| c == "ask"));
        assert!(cands.iter().any(|c| c == "sync"));
    }

    #[test]
    fn complete_set_keys() {
        let (start, cands) = complete_prefix("set fo");
        assert!(cands.iter().any(|c| c == "format"));
        assert!(start >= 4);
    }

    #[test]
    fn complete_set_after_space() {
        let (_, cands) = complete_prefix("set ");
        assert!(cands.iter().any(|c| c == "format"));
        assert!(cands.iter().any(|c| c == "top"));
    }

    #[test]
    fn complete_empty_line_returns_all_commands() {
        let (_, cands) = complete_prefix("");
        assert!(cands.len() >= 8);
        assert!(cands.iter().any(|c| c == "search"));
        assert!(cands.iter().any(|c| c == "nlm"));
        assert!(cands.iter().any(|c| c == "quit"));
    }

    #[test]
    fn complete_unknown_prefix_returns_empty() {
        let (_, cands) = complete_prefix("zzz");
        assert!(cands.is_empty(), "unknown prefix should yield no completions");
    }

    #[test]
    fn hint_for_search_command() {
        let h = hint_for_line("search ");
        assert!(h.is_some());
        assert!(h.unwrap().contains("search"));
    }

    #[test]
    fn hint_empty_line_is_none() {
        assert!(hint_for_line("").is_none());
    }

    #[test]
    fn hint_for_nlm_command() {
        let h = hint_for_line("nlm ");
        assert!(h.is_some());
        assert!(h.unwrap().contains("list"));
    }
}
