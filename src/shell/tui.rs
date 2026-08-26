//! TUI Dashboard poler-shell (ratatui + crossterm). Минимальная реализация
//! v0.15.0: 3-панельный layout с переключением фокуса Tab.
//!
//! Layout (когда terminal >= 100×30):
//! ```text
//! ┌──────────────────┬──────────────────────────────────────────┐
//! │ Butkи/Репозитории │ Поле ввода (rustyline-семантика)           │
//! │ (1/4 ширины)      │ (правая верхняя, 1/3 высоты)               │
//! │                   ├──────────────────────────────────────────┤
//! │                   │ Результаты (правая нижняя, 2/3 высоты)    │
//! │                   │ скроллятся PgUp/PgDn                       │
//! └──────────────────┴──────────────────────────────────────────┘
//! │status: poler 0.15.0  db:/path/to/web-index.db  fmt:md  top:10│
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Фокус: Tab переключает left|input|output; Esc — выход; Enter в input —
//! выполнить команду; PgUp/PgDn в output — скроллинг.

use std::io::{self, Write};
use std::path::PathBuf;

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;

use super::commands::{dispatch, CmdResult};
use super::state::ShellState;

/// Запустить TUI-режим (`poler-engine --tui`).
pub fn run_tui(db_path: PathBuf) -> std::process::ExitCode {
    let mut state = ShellState::new(db_path.clone());

    // Подготовка терминала
    if let Err(e) = enable_raw_mode() {
        eprintln!("poler-shell --tui: raw mode: {e}");
        return std::process::ExitCode::from(2);
    }
    let mut stdout = io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen);
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            let _ = disable_raw_mode();
            eprintln!("poler-shell --tui: terminal: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    // Начальное состояние
    let mut input_buf = String::new();
    let mut input_history: Vec<String> = Vec::new();
    let mut input_history_idx: Option<usize> = None;
    let mut output_lines: Vec<String> = vec![
        "poler-shell TUI Dashboard v0.15.0".into(),
        "  ↑↓ — история ввода; Enter — выполнить; Tab — сменить фокус; Esc — выход".into(),
        "  PgUp/PgDn — прокрутка результата; команды как в REPL (`help`).".into(),
        String::new(),
    ];
    let mut notebooks: Vec<String> = vec!["(нажмите 'r' для списка 87 ноутбуков)".into()];
    let mut nb_state = ListState::default();
    nb_state.select(Some(0));
    let mut output_state = ListState::default();
    output_state.select(None);
    let mut focus = Focus::Input;
    let mut should_quit = false;

    // Стартовый приветственный вывод
    state.set_output(help_text());

    while !should_quit {
        // Получить снапшот layout
        let _ = terminal.draw(|f| {
            let area = f.area();
            // Layout: правая часть делится по высоте на 1/3 input + 2/3 output
            // левая занимает 1/4 ширины; status-bar 1 строка снизу
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(1)])
                .split(area);
            let main_area = chunks[0];
            let status_area = chunks[1];

            let main_cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
                .split(main_area);
            let nb_area = main_cols[0];
            let right_area = main_cols[1];

            let right_rows = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Length(3), Constraint::Min(5)])
                .split(right_area);
            let input_area = right_rows[0];
            let output_area = right_rows[1];

            // Левая панель — ноутбуки
            let nb_items: Vec<ListItem> = notebooks.iter().map(|s| ListItem::new(Line::from(s.clone()))).collect();
            let nb_block = Block::default()
                .borders(Borders::ALL)
                .title("Notebooks (r=refresh)")
                .border_style(if matches!(focus, Focus::Notebooks) {
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                });
            let nb_widget = List::new(nb_items)
                .block(nb_block)
                .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));
            f.render_stateful_widget(nb_widget, nb_area, &mut nb_state.clone());

            // Правая верхняя — ввод
            let input_block = Block::default()
                .borders(Borders::ALL)
                .title("Input (Enter=run, ↑↓=history)")
                .border_style(if matches!(focus, Focus::Input) {
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                });
            let input_para = Paragraph::new(format!("poler> {}", input_buf))
                .block(input_block)
                .style(Style::default().fg(Color::White));
            f.render_widget(input_para, input_area);

            // Правая нижняя — вывод
            let out_items: Vec<ListItem> = output_lines
                .iter()
                .map(|s| ListItem::new(Line::from(s.clone())))
                .collect();
            let out_block = Block::default()
                .borders(Borders::ALL)
                .title("Output (PgUp/PgDn=scroll)")
                .border_style(if matches!(focus, Focus::Output) {
                    Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::default()
                });
            let out_widget = List::new(out_items)
                .block(out_block)
                .highlight_style(Style::default().fg(Color::Black).bg(Color::Green));
            f.render_stateful_widget(out_widget, output_area, &mut output_state.clone());

            // Status bar (1 строка)
            let status_text = format!(
                " poler-shell {}  │  db: {:?}  │  fmt: {:?}  │  top: {}  │  focus: {}  ",
                env!("CARGO_PKG_VERSION"),
                state.db_path(),
                state.format,
                state.top,
                focus.as_str(),
            );
            let status_line = Line::from(Span::styled(
                status_text,
                Style::default().fg(Color::Black).bg(Color::DarkGray),
            ));
            let status_para = Paragraph::new(status_line).wrap(Wrap { trim: false });
            f.render_widget(status_para, status_area);
        });

        // События клавиатуры
        let ev = match event::read() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("poler-shell --tui: event: {e}");
                break;
            }
        };
        if let Event::Key(k) = ev {
            match (k.code, k.modifiers) {
                (KeyCode::Esc, _) => {
                    should_quit = true;
                }
                (KeyCode::Tab, _) => {
                    focus = focus.next();
                }
                (KeyCode::BackTab, _) => {
                    focus = focus.prev();
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    should_quit = true;
                }
                (KeyCode::Char('r'), _) if matches!(focus, Focus::Notebooks) => {
                    // Обновить список ноутбуков
                    output_lines.push("poler> nlm list ...".into());
                    let r = dispatch(&mut state, "nlm list");
                    if let CmdResult::Done(out) = r {
                        notebooks = out.lines().map(String::from).take(200).collect();
                        if notebooks.is_empty() {
                            notebooks = vec!["(пусто)".into()];
                        }
                    }
                }
                (KeyCode::PageUp, _) if matches!(focus, Focus::Output) => {
                    let idx = output_state.selected().unwrap_or(0);
                    output_state.select(Some(idx.saturating_sub(5)));
                }
                (KeyCode::PageDown, _) if matches!(focus, Focus::Output) => {
                    let idx = output_state.selected().unwrap_or(0);
                    let max = output_lines.len().saturating_sub(1);
                    output_state.select(Some((idx + 5).min(max)));
                }
                (KeyCode::Up, _) if matches!(focus, Focus::Input) => {
                    if !input_history.is_empty() {
                        input_history_idx = Some(match input_history_idx {
                            None => input_history.len() - 1,
                            Some(i) if i > 0 => i - 1,
                            Some(i) => i,
                        });
                        if let Some(i) = input_history_idx {
                            input_buf = input_history[i].clone();
                        }
                    }
                }
                (KeyCode::Down, _) if matches!(focus, Focus::Input) => {
                    if !input_history.is_empty() {
                        input_history_idx = match input_history_idx {
                            None => None,
                            Some(i) if i + 1 < input_history.len() => Some(i + 1),
                            _ => None,
                        };
                        input_buf = match input_history_idx {
                            Some(i) => input_history[i].clone(),
                            None => String::new(),
                        };
                    }
                }
                (KeyCode::Up, _) if matches!(focus, Focus::Notebooks) => {
                    let idx = nb_state.selected().unwrap_or(0);
                    nb_state.select(Some(idx.saturating_sub(1)));
                }
                (KeyCode::Down, _) if matches!(focus, Focus::Notebooks) => {
                    let idx = nb_state.selected().unwrap_or(0);
                    let max = notebooks.len().saturating_sub(1);
                    nb_state.select(Some((idx + 1).min(max)));
                }
                (KeyCode::Char(c), _) if matches!(focus, Focus::Input) => {
                    input_buf.push(c);
                }
                (KeyCode::Backspace, _) if matches!(focus, Focus::Input) => {
                    input_buf.pop();
                }
                (KeyCode::Enter, _) if matches!(focus, Focus::Input) => {
                    let line = input_buf.clone();
                    output_lines.push(format!("poler> {}", line));
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    input_history.push(trimmed.to_string());
                    input_history_idx = None;
                    input_buf.clear();

                    // Спец-выход TUI
                    if matches!(trimmed, "quit" | "exit" | "q") {
                        should_quit = true;
                        continue;
                    }
                    let r = dispatch(&mut state, &line);
                    match r {
                        CmdResult::Quit => {
                            should_quit = true;
                        }
                        CmdResult::Empty => {}
                        CmdResult::Done(out) => {
                            // Каждый абзац — отдельная строка для скроллинга
                            for l in out.lines() {
                                output_lines.push(l.to_string());
                            }
                            // Доп. пустая строка как разделитель
                            output_lines.push(String::new());
                            // Скролл вниз
                            let max = output_lines.len().saturating_sub(1);
                            output_state.select(Some(max));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Восстановление терминала
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen);
    let _ = stdout.flush();

    std::process::ExitCode::SUCCESS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Notebooks,
    Input,
    Output,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Notebooks => Focus::Input,
            Focus::Input => Focus::Output,
            Focus::Output => Focus::Notebooks,
        }
    }
    fn prev(self) -> Self {
        match self {
            Focus::Notebooks => Focus::Output,
            Focus::Input => Focus::Notebooks,
            Focus::Output => Focus::Input,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Focus::Notebooks => "notebooks",
            Focus::Input => "input",
            Focus::Output => "output",
        }
    }
}

fn help_text() -> String {
    let mut s = String::new();
    s.push_str("poler-shell TUI Dashboard v0.15.0\n\n");
    s.push_str("Команды (как в REPL):\n");
    s.push_str("  search \"<query>\" --top 5\n");
    s.push_str("  nlm list | nlm sync [<NB_ID>] | nlm ask <NB_ID> \"вопрос\"\n");
    s.push_str("  stats | set format md|json | set top 20\n");
    s.push('\n');
    s.push_str("Управление:\n");
    s.push_str("  Tab/BackTab — сменить фокус\n");
    s.push_str("  ↑/↓ в input — история команд\n");
    s.push_str("  ↑/↓ в notebooks — навигация\n");
    s.push_str("  'r' в notebooks — обновить список (nlm list)\n");
    s.push_str("  PgUp/PgDn в output — скроллинг\n");
    s.push_str("  Esc или Ctrl+C — выход\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_cycle_next() {
        assert_eq!(Focus::Notebooks.next(), Focus::Input);
        assert_eq!(Focus::Input.next(), Focus::Output);
        assert_eq!(Focus::Output.next(), Focus::Notebooks);
    }

    #[test]
    fn focus_cycle_prev() {
        assert_eq!(Focus::Notebooks.prev(), Focus::Output);
        assert_eq!(Focus::Output.prev(), Focus::Input);
        assert_eq!(Focus::Input.prev(), Focus::Notebooks);
    }

    #[test]
    fn focus_as_str_correct() {
        assert_eq!(Focus::Notebooks.as_str(), "notebooks");
        assert_eq!(Focus::Input.as_str(), "input");
        assert_eq!(Focus::Output.as_str(), "output");
    }
}
