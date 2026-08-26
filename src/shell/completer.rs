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
    ("crawl", "crawl <URL> [--depth N] [--max M] [--cross] [--delay-ms N]"),
    ("impact", "impact <PATH> <SYMBOL> [--depth N] [--cache <DB>]"),
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

    // v0.15.1: crawl/impact — завершаем флаги (--depth/--max/...)
    // после первого слова. После флага со значением можно продолжать.
    if first == "crawl" {
        return complete_crawl_flags(prefix, &tokens, in_first_token);
    }
    if first == "impact" {
        return complete_impact_flags(prefix, &tokens, in_first_token);
    }

    (prefix.len(), Vec::new())
}

/// v0.15.1: Завершение флагов `crawl`. Если курсор в начале нового токена
/// (префикс заканчивается пробелом) и предыдущий токен — value-флаг
/// (требует значение, например --depth), то не предлагаем флаги (ждём число).
/// Иначе предлагаем флаги, начинающиеся с введённого префикса.
fn complete_crawl_flags(prefix: &str, tokens: &[&str], in_first_token: bool) -> (usize, Vec<String>) {
    const FLAGS: &[&str] = &[
        "--depth", "--max", "--cross", "--delay-ms", "--wait-ms", "--cdp-port", "--help",
    ];
    // value-флаги: после них ждётся число, а не другой флаг
    const VALUE_FLAGS: &[&str] = &["--depth", "--max", "--delay-ms", "--wait-ms", "--cdp-port"];

    // Текущий токен (если курсор в нём) или пустой (если курсор после пробела)
    let cur = if in_first_token { tokens.last().copied().unwrap_or("") } else { "" };

    // Если курсор сразу после пробела и предыдущий токен — value-флаг, не дополняем.
    if !in_first_token {
        if let Some(prev) = tokens.last() {
            if VALUE_FLAGS.contains(prev) {
                return (prefix.len(), Vec::new());
            }
        }
        // после пробела — предлагать все флаги
        let cands: Vec<String> = FLAGS.iter().map(|s| (*s).to_string()).collect();
        let replace_from = prefix.len();
        return (replace_from, cands);
    }

    // Курсор в токене — фильтруем по префиксу (только если начинается с --)
    if !cur.starts_with('-') {
        return (prefix.len(), Vec::new());
    }
    let cands: Vec<String> = FLAGS
        .iter()
        .filter(|f| f.starts_with(cur))
        .map(|s| (*s).to_string())
        .collect();
    let replace_from = prefix.len() - cur.len();
    (replace_from, cands)
}

/// v0.15.1: Завершение флагов `impact`. Аналогично crawl, но свой набор.
fn complete_impact_flags(prefix: &str, tokens: &[&str], in_first_token: bool) -> (usize, Vec<String>) {
    const FLAGS: &[&str] = &["--depth", "--cache", "--max-file-bytes", "--help"];
    const VALUE_FLAGS: &[&str] = &["--depth", "--cache", "--max-file-bytes"];

    let cur = if in_first_token { tokens.last().copied().unwrap_or("") } else { "" };

    if !in_first_token {
        if let Some(prev) = tokens.last() {
            if VALUE_FLAGS.contains(prev) {
                return (prefix.len(), Vec::new());
            }
        }
        let cands: Vec<String> = FLAGS.iter().map(|s| (*s).to_string()).collect();
        let replace_from = prefix.len();
        return (replace_from, cands);
    }

    if !cur.starts_with('-') {
        return (prefix.len(), Vec::new());
    }
    let cands: Vec<String> = FLAGS
        .iter()
        .filter(|f| f.starts_with(cur))
        .map(|s| (*s).to_string())
        .collect();
    let replace_from = prefix.len() - cur.len();
    (replace_from, cands)
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

/// v0.15.1: PolerCompleter реализует все 4 трейта (Completer + Hinter +
/// Highlighter + Validator). Helper — это supertrait, который требует всех
/// четырёх, но blanket impl в rustyline НЕ предусмотрен — нужно явное объявление
/// `impl Helper for PolerCompleter {}`. Это и позволяет подключить его в
/// `Editor::<PolerCompleter, _>::new()` в `commands::run_shell` —
/// Tab-completion, hints, validation включены.
impl rustyline::Helper for PolerCompleter {}

#[cfg(test)]
fn _poler_completer_implements_helper() {
    fn assert_helper<H: rustyline::Helper>(_h: H) {}
    assert_helper(PolerCompleter::default());
}

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

    // v0.15.1: crawl/impact completion tests

    #[test]
    fn complete_crawl_first_token() {
        let (start, cands) = complete_prefix("cra");
        assert!(cands.iter().any(|c| c == "crawl"));
        assert_eq!(start, 0);
    }

    #[test]
    fn complete_crawl_flags_after_space() {
        let (_, cands) = complete_prefix("crawl https://example.com ");
        assert!(cands.iter().any(|c| c == "--depth"));
        assert!(cands.iter().any(|c| c == "--max"));
        assert!(cands.iter().any(|c| c == "--cross"));
        assert!(cands.iter().any(|c| c == "--help"));
    }

    #[test]
    fn complete_crawl_flag_prefix() {
        let (start, cands) = complete_prefix("crawl https://example.com --de");
        assert!(cands.iter().any(|c| c == "--depth"));
        assert!(!cands.iter().any(|c| c == "--max"));
        assert!(start > 0, "replace_from must point to '--' position");
    }

    #[test]
    fn complete_crawl_after_value_flag_returns_empty() {
        // После --depth (value-флаг) ждём число, не флаги
        let (_, cands) = complete_prefix("crawl https://example.com --depth ");
        assert!(cands.is_empty(), "after --depth (value flag) we expect no flag completions");
    }

    #[test]
    fn complete_crawl_no_flag_completion_for_url() {
        // Курсор в URL (не --) — не предлагаем флаги
        let (_, cands) = complete_prefix("crawl https://example.com");
        assert!(cands.is_empty(), "no flag completions when typing URL");
    }

    #[test]
    fn complete_crawl_max_flag() {
        let (start, cands) = complete_prefix("crawl https://example.com --m");
        assert!(cands.iter().any(|c| c == "--max"));
        assert!(!cands.iter().any(|c| c == "--depth"));
        assert!(start > 0);
    }

    #[test]
    fn complete_impact_first_token() {
        let (start, cands) = complete_prefix("imp");
        assert!(cands.iter().any(|c| c == "impact"));
        assert_eq!(start, 0);
    }

    #[test]
    fn complete_impact_flags_after_space() {
        let (_, cands) = complete_prefix("impact ./src main ");
        assert!(cands.iter().any(|c| c == "--depth"));
        assert!(cands.iter().any(|c| c == "--cache"));
        assert!(cands.iter().any(|c| c == "--help"));
    }

    #[test]
    fn complete_impact_flag_prefix() {
        let (start, cands) = complete_prefix("impact ./src main --c");
        assert!(cands.iter().any(|c| c == "--cache"));
        assert!(!cands.iter().any(|c| c == "--depth"));
        assert!(start > 0);
    }

    #[test]
    fn complete_impact_after_value_flag_returns_empty() {
        let (_, cands) = complete_prefix("impact ./src main --depth ");
        assert!(cands.is_empty(), "after --depth we expect no flag completions");
    }

    #[test]
    fn complete_impact_no_flag_completion_for_symbol() {
        // Курсор в symbol (не --) — не предлагаем флаги
        let (_, cands) = complete_prefix("impact ./src mai");
        assert!(cands.is_empty());
    }

    #[test]
    fn hint_for_crawl_command() {
        let h = hint_for_line("crawl ");
        assert!(h.is_some());
        assert!(h.unwrap().contains("URL"));
    }

    #[test]
    fn hint_for_impact_command() {
        let h = hint_for_line("impact ");
        assert!(h.is_some());
        assert!(h.unwrap().contains("PATH"));
    }
}
