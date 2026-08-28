//! # poler-shell TUI v0.17.3 — MiMo Code-style 4-pane Dashboard + Companion Bridge (M2+M3+M4)
//!
//! Полная переработка TUI: 4-панельный layout с поддержкой мыши,
//! drag-select, встроенным редактором заметок (tui-textarea) и
//! палитрой `?` с 10 готовыми сценариями.
//!
//! ```text
//! ┌──────────────┬──────────────────────────────────┬──────────────┐
//! │ REPO / NB    │ CHAT & RESPONSES  (50% height)    │ NOTES (CRUD) │
//! │ (25% width)  │ • Чистый ответ без мусора         │ (25% width)  │
//! │              │ • Drag-select мышью → Ctrl+Y      │ • Ctrl+N     │
//! │ nlm list     │ • Клик по ноутбуку → активация    │ • Ctrl+S     │
//! │ gh repos     ├──────────────────────────────────┤ │ (save AI)   │
//! │ gix log      │ INPUT BOX (25% height)            ├──────────────┤
//! │              │ • ↑/↓ history • Tab completion     │ SOURCES CRUD │
//! │              │ • Enter — выполнить              │ • list/add/rm│
//! ├──────────────┴──────────────────────────────────┴──────────────┤
//! │ poler-shell 0.17.3  db:web-index.db  fmt:md  top:10  F2:Chat   │
//! └────────────────────────────────────────────────────────────────┘
//! ```
//!
//! Управление:
//! - **Tab/BackTab** — смена фокуса между панелями
//! - **↑/↓** — навигация в списках / история ввода
//! - **PgUp/PgDn** — прокрутка Chat panel
//! - **Enter** — выполнить команду (Input) / открыть источник (Sources)
//! - **Ctrl+N** — новая заметка (встроенный tui-textarea редактор)
//! - **Ctrl+S** — сохранить последний AI-ответ как заметку
//! - **Ctrl+Y** — копировать текущее выделение в буфер
//! - **?** — палитра 10 сценариев
//! - **Ctrl+E** — редактировать выделенную заметку
//! - **Ctrl+D** — удалить выделенную заметку/источник (с подтверждением)
//! - **Ctrl+T** — тестировать выбранный источник (Sources panel)
//! - **Esc / Ctrl+C** — выход
//!
//! ## M4: Enter-handler на источнике (Sources panel)
//!
//! Источник из `poler_sources` (file/url/repo) маппится в
//! `companion::SourceKind` и через `enter_action()` превращается в
//! `EnterAction`:
//!
//! | Источник | SourceKind | EnterAction |
//! |---|---|---|
//! | `file` (`/path/to/x.md`) | `FileUpload { local_path }` | `EditLocal(path)` → `$EDITOR` |
//! | `url`  (`https://...`) | `Web { url }` | `OpenUrl(url)` → `xdg-open` |
//! | `repo` (`owner/name`) | `Web { "https://github.com/{owner/name}" }` | `OpenUrl` |

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Instant;

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyModifiers,
    MouseEvent,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap,
};
use ratatui::Terminal;
use tui_textarea::TextArea;

use super::commands::{dispatch, CmdResult};
use super::help;
use super::mouse::{self, MouseAction, SelectionRect};
use super::state::ShellState;
use super::transcript::{self, ChatEntry};

/// Запустить TUI-режим (`poler-engine --tui`).
pub fn run_tui(db_path: PathBuf) -> std::process::ExitCode {
    let mut state = ShellState::new(db_path.clone());

    // Подготовка терминала
    if let Err(e) = enable_raw_mode() {
        eprintln!("poler-shell --tui: raw mode: {e}");
        return std::process::ExitCode::from(2);
    }
    let mut stdout = io::stdout();
    let _ = execute!(stdout, EnterAlternateScreen, EnableMouseCapture);
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            let _ = disable_raw_mode();
            eprintln!("poler-shell --tui: terminal: {e}");
            return std::process::ExitCode::from(2);
        }
    };

    // Состояние TUI
    let mut input_buf = String::new();
    let mut input_history: Vec<String> = Vec::new();
    let mut input_history_idx: Option<usize> = None;
    let mut output_lines: Vec<String> = vec![
        "poler-shell TUI Dashboard v0.17.3 — MiMo Code-style + Companion Bridge (M2+M3+M4)".into(),
        "  ↑↓ — история ввода; Enter — выполнить; Tab — сменить фокус; Esc — выход".into(),
        "  Ctrl+N — новая заметка; Ctrl+S — сохранить AI-ответ; ? — палитра".into(),
        "  Drag мышью по Chat panel → Ctrl+Y → буфер обмена".into(),
        String::new(),
    ];
    let mut output_scroll: usize = 0;
    let mut output_state = ListState::default();
    output_state.select(None);

    // Список ноутбуков (левая панель)
    let mut notebooks: Vec<String> = vec!["(нажмите 'r' для nlm list)".into()];
    let mut notebook_ids: Vec<String> = Vec::new();
    let mut nb_state = ListState::default();
    nb_state.select(Some(0));

    // Список заметок (правая верхняя)
    let mut notes_items: Vec<String> = vec!["(нет заметок — Ctrl+N)".into()];
    let mut notes_state = ListState::default();
    notes_state.select(Some(0));

    // Список источников (правая нижняя)
    let mut sources_items: Vec<String> = vec!["(нет источников — sources add)".into()];
    let mut sources_state = ListState::default();
    sources_state.select(Some(0));

    let mut focus = Focus::Input;
    let mut should_quit = false;

    // Drag-select
    let mut selection = SelectionRect::new();
    let mut last_click_time: Option<Instant> = None;
    let mut last_click_pos: Option<(u16, u16)> = None;

    // Режим: Normal / Palette / NoteEditor
    let mut mode: Mode = Mode::Normal;
    let mut palette_state = ListState::default();
    palette_state.select(Some(0));

    // Стартовый приветственный вывод
    state.set_output(help::help_overview());
    // Поместить help в output для немедленного отображения
    for l in help::help_overview().lines() {
        output_lines.push(l.to_string());
    }
    output_lines.push(String::new());
    output_scroll = output_lines.len().saturating_sub(1);

    // Главная петля событий
    while !should_quit {
        // Снапшот layout — нужен для hit-testing мыши
        let term_size = terminal.size().unwrap_or_default();
        let layout_snapshot = compute_layout(Rect::new(0, 0, term_size.width, term_size.height));
        let _ = terminal.draw(|f| {
            render_ui(
                f,
                &layout_snapshot,
                &input_buf,
                &output_lines,
                &output_state,
                &notebooks,
                &nb_state,
                &notes_items,
                &notes_state,
                &sources_items,
                &sources_state,
                focus,
                &selection,
                &state,
                &mode,
                &palette_state,
            );
        });

        // События
        let ev = match event::read() {
            Ok(e) => e,
            Err(e) => {
                eprintln!("poler-shell --tui: event: {e}");
                break;
            }
        };

        // Сначала режимные обработчики
        let new_mode = match &mut mode {
            Mode::NoteEditor(editor_state) => {
                match handle_note_editor_event(ev.clone(), editor_state) {
                    NoteEditorResult::Continue => continue,
                    NoteEditorResult::Save(title, body) => {
                        // Сохранить как новую заметку
                        let nb_id = state.active_notebook().map(String::from);
                        match state.ensure_notes_conn() {
                            Ok(conn) => {
                                match crate::notes::add_note(
                                    conn,
                                    &title,
                                    &body,
                                    &[],
                                    crate::notes::NoteSource::Manual,
                                    nb_id.as_deref(),
                                ) {
                                    Ok(id) => {
                                        output_lines.push(format!("✓ Сохранена заметка #{id} «{title}»"));
                                        refresh_notes_list(&mut state, &mut notes_items);
                                    }
                                    Err(e) => output_lines.push(format!("❌ {e}")),
                                }
                            }
                            Err(e) => output_lines.push(format!("❌ {e}")),
                        }
                        output_scroll = output_lines.len().saturating_sub(1);
                        Mode::Normal
                    }
                    NoteEditorResult::Cancel => Mode::Normal,
                }
            }
            Mode::Palette => {
                let mut keep_palette = true;
                if let Event::Key(k) = ev {
                    match (k.code, k.modifiers) {
                        (KeyCode::Esc, _) | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            keep_palette = false;
                        }
                        (KeyCode::Up, _) => {
                            let i = palette_state.selected().unwrap_or(0);
                            palette_state.select(Some(i.saturating_sub(1)));
                        }
                        (KeyCode::Down, _) => {
                            let i = palette_state.selected().unwrap_or(0);
                            let max = help::palette_scenarios().len().saturating_sub(1);
                            palette_state.select(Some((i + 1).min(max)));
                        }
                        (KeyCode::Enter, _) => {
                            let sc = &help::palette_scenarios()[palette_state.selected().unwrap_or(0)];
                            input_buf.clear();
                            input_buf.push_str(sc.cmd);
                            focus = Focus::Input;
                            keep_palette = false;
                        }
                        _ => {}
                    }
                }
                if let Event::Mouse(me) = ev {
                    if let MouseAction::DragEnd { col, row } = mouse::parse_event(me) {
                        if let Some(area) = layout_snapshot.palette_area() {
                            if mouse::hit(&area, col, row) {
                                let local_row = row.saturating_sub(area.y) as usize;
                                let scenarios = help::palette_scenarios();
                                if local_row < scenarios.len() {
                                    palette_state.select(Some(local_row));
                                }
                            }
                        }
                    }
                }
                if keep_palette {
                    continue;
                } else {
                    Mode::Normal
                }
            }
            Mode::Transcript(ts) => {
                // v0.17.4: Transcript / Response View.
                let mut next: Option<Mode> = None;
                if let Event::Key(k) = ev {
                    match (k.code, k.modifiers) {
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            next = Some(Mode::Normal);
                        }
                        (KeyCode::Esc, _) => {
                            if ts.viewing.is_some() {
                                // Response View → назад к ленте
                                ts.viewing = None;
                                ts.view_scroll = 0;
                                ts.status.clear();
                            } else {
                                next = Some(Mode::Normal);
                            }
                        }
                        (KeyCode::Up, _) => {
                            if ts.viewing.is_some() {
                                ts.view_scroll_up();
                            } else {
                                ts.move_up();
                            }
                        }
                        (KeyCode::Down, _) => {
                            if ts.viewing.is_some() {
                                ts.view_scroll_down();
                            } else {
                                ts.move_down();
                            }
                        }
                        (KeyCode::PageUp, _) => {
                            if ts.viewing.is_some() {
                                ts.view_page_up();
                            } else {
                                for _ in 0..5 {
                                    ts.move_up();
                                }
                            }
                        }
                        (KeyCode::PageDown, _) => {
                            if ts.viewing.is_some() {
                                ts.view_page_down();
                            } else {
                                for _ in 0..5 {
                                    ts.move_down();
                                }
                            }
                        }
                        (KeyCode::Home, _) => {
                            if ts.viewing.is_none() && !ts.entries.is_empty() {
                                ts.list.select(Some(0));
                            }
                        }
                        (KeyCode::End, _) => {
                            if ts.viewing.is_none() && !ts.entries.is_empty() {
                                ts.list.select(Some(ts.entries.len() - 1));
                            }
                        }
                        (KeyCode::Enter, _) => {
                            if ts.viewing.is_none() {
                                ts.open_selected();
                            }
                        }
                        (KeyCode::Char('y'), _) => {
                            // Копировать ответ (текущий в Response View или
                            // выбранный в ленте) в буфер обмена.
                            let src = if ts.viewing.is_some() {
                                ts.viewing_entry()
                            } else {
                                ts.selected()
                            };
                            if let Some(e) = src {
                                match mouse::copy_to_clipboard(&e.answer) {
                                    Ok(()) => ts.status = format!(
                                        "✓ Скопировано {} символов ответа #{}",
                                        e.answer.chars().count(),
                                        e.id
                                    ),
                                    Err(err) => ts.status = format!("❌ clipboard: {err}"),
                                }
                            }
                        }
                        (KeyCode::Char('r'), _) => {
                            ts.reload(&mut state);
                        }
                        (KeyCode::Char('d'), _) => {
                            // Удалить выбранную пару из ленты.
                            if ts.viewing.is_none() {
                                if let Some(e) = ts.selected() {
                                    let id = e.id;
                                    match state.ensure_notes_conn() {
                                        Ok(conn) => match transcript::delete_entry(conn, id) {
                                            Ok(()) => {
                                                ts.reload(&mut state);
                                                ts.status = format!("✓ Пара #{id} удалена");
                                            }
                                            Err(err) => ts.status = format!("❌ {err}"),
                                        },
                                        Err(err) => ts.status = format!("❌ {err}"),
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if let Event::Mouse(me) = ev {
                    if let MouseAction::DragEnd { col, row } = mouse::parse_event(me) {
                        if let Some(area) = layout_snapshot.transcript_area() {
                            if mouse::hit(&area, col, row) {
                                // Клик по строке ленты → выбор + открыть ответ.
                                let local = row.saturating_sub(area.y + 1) as usize;
                                if local < ts.entries.len() {
                                    ts.list.select(Some(local));
                                    ts.open_selected();
                                }
                            }
                        }
                    }
                }
                match next {
                    Some(m) => m,
                    None => continue,
                }
            }
            Mode::Normal => {
                // Не меняем режим, переходим к обычной обработке событий
                Mode::Normal
            }
        };
        if std::mem::discriminant(&new_mode) != std::mem::discriminant(&mode) {
            mode = new_mode;
            continue;
        }

        // Mode::Normal — обычная обработка событий
        match ev {
            Event::Key(k) => {
                handle_key_event(
                    k,
                    &mut state,
                    &mut input_buf,
                    &mut input_history,
                    &mut input_history_idx,
                    &mut output_lines,
                    &mut output_scroll,
                    &mut output_state,
                    &mut notebooks,
                    &mut notebook_ids,
                    &mut nb_state,
                    &mut notes_items,
                    &mut notes_state,
                    &mut sources_items,
                    &mut sources_state,
                    &mut focus,
                    &mut should_quit,
                    &mut mode,
                    &mut selection,
                );
            }
            Event::Mouse(me) => {
                handle_mouse_event(
                    me,
                    &layout_snapshot,
                    &mut state,
                    &mut input_buf,
                    &mut input_history,
                    &mut input_history_idx,
                    &mut output_lines,
                    &mut output_scroll,
                    &mut output_state,
                    &mut notebooks,
                    &mut notebook_ids,
                    &mut nb_state,
                    &mut notes_items,
                    &mut notes_state,
                    &mut sources_items,
                    &mut sources_state,
                    &mut focus,
                    &mut selection,
                    &mut last_click_time,
                    &mut last_click_pos,
                );
            }
            Event::Resize(_, _) => {
                // ratatui автоматически перерисует на следующей итерации
            }
            _ => {}
        }
    }

    // Восстановление терминала
    let _ = disable_raw_mode();
    let mut stdout = io::stdout();
    let _ = execute!(stdout, LeaveAlternateScreen, DisableMouseCapture);
    let _ = stdout.flush();

    std::process::ExitCode::SUCCESS
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Notebooks,
    Input,
    Output,
    Notes,
    Sources,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Notebooks => Focus::Input,
            Focus::Input => Focus::Output,
            Focus::Output => Focus::Notes,
            Focus::Notes => Focus::Sources,
            Focus::Sources => Focus::Notebooks,
        }
    }
    fn prev(self) -> Self {
        match self {
            Focus::Notebooks => Focus::Sources,
            Focus::Input => Focus::Notebooks,
            Focus::Output => Focus::Input,
            Focus::Notes => Focus::Output,
            Focus::Sources => Focus::Notes,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Focus::Notebooks => "notebooks",
            Focus::Input => "input",
            Focus::Output => "chat",
            Focus::Notes => "notes",
            Focus::Sources => "sources",
        }
    }
}

#[derive(Debug)]
enum Mode {
    Normal,
    Palette,
    NoteEditor(NoteEditorState),
    /// v0.17.4: Transcript / Response View — лента чата `nlm ask`
    /// (F3). `viewing == None` — лента пар; `Some(i)` — полный ответ
    /// записи `entries[i]` (Response View).
    Transcript(TranscriptState),
}

/// Состояние окна ленты чата: пары вопрос→ответ + просмотровый режим.
#[derive(Debug)]
struct TranscriptState {
    /// Пары ленты (старые сверху, новые снизу — feed-порядок).
    entries: Vec<ChatEntry>,
    /// Выбранная строка ленты.
    list: ListState,
    /// Индекс записи в Response View (None — показываем ленту).
    viewing: Option<usize>,
    /// Вертикальная прокрутка ответа.
    view_scroll: u16,
    /// Всего записей в БД (лента показывает последние TRANSCRIPT_LIMIT).
    total: i64,
    /// Строка статуса последней операции (копирование и т.п.).
    status: String,
}

/// Сколько последних пар грузим в окно.
const TRANSCRIPT_LIMIT: usize = 200;
/// Шаг прокрутки PgUp/PgDn в Response View.
const VIEW_PAGE: u16 = 12;

impl TranscriptState {
    fn new(state: &mut ShellState) -> Self {
        let mut ts = TranscriptState {
            entries: Vec::new(),
            list: ListState::default(),
            viewing: None,
            view_scroll: 0,
            total: 0,
            status: String::new(),
        };
        ts.reload(state);
        ts
    }

    /// Перечитать ленту из БД (r). Выбор — на самой свежей паре.
    fn reload(&mut self, state: &mut ShellState) {
        self.status.clear();
        match state.ensure_notes_conn() {
            Ok(conn) => match transcript::list_entries(conn, TRANSCRIPT_LIMIT) {
                Ok(feed) => {
                    self.total = transcript::count(conn).unwrap_or(feed.len() as i64);
                    let n = feed.len();
                    self.entries = feed;
                    self.list = ListState::default();
                    if n > 0 {
                        self.list.select(Some(n - 1));
                    }
                    if self.total as usize > n {
                        self.status = format!(
                            "показаны последние {n} из {} пар",
                            self.total
                        );
                    }
                }
                Err(e) => self.status = format!("❌ {e}"),
            },
            Err(e) => self.status = format!("❌ {e}"),
        }
        self.viewing = None;
        self.view_scroll = 0;
    }

    fn move_up(&mut self) {
        let i = self.list.selected().unwrap_or(0);
        self.list.select(Some(i.saturating_sub(1)));
    }

    fn move_down(&mut self) {
        let i = self.list.selected().unwrap_or(0);
        let max = self.entries.len().saturating_sub(1);
        self.list.select(Some((i + 1).min(max)));
    }

    /// Ответ выбранной записи (лента).
    fn selected(&self) -> Option<&ChatEntry> {
        self.list.selected().and_then(|i| self.entries.get(i))
    }

    /// Открыть Response View выбранной пары.
    fn open_selected(&mut self) {
        if self.list.selected().is_some() && !self.entries.is_empty() {
            self.viewing = self.list.selected();
            self.view_scroll = 0;
            self.status.clear();
        }
    }

    /// Ответ в просмотровом режиме.
    fn viewing_entry(&self) -> Option<&ChatEntry> {
        self.viewing.and_then(|i| self.entries.get(i))
    }

    fn view_scroll_up(&mut self) {
        self.view_scroll = self.view_scroll.saturating_sub(1);
    }

    fn view_scroll_down(&mut self) {
        self.view_scroll = self.view_scroll.saturating_add(1).min(65_535);
    }

    fn view_page_up(&mut self) {
        self.view_scroll = self.view_scroll.saturating_sub(VIEW_PAGE);
    }

    fn view_page_down(&mut self) {
        self.view_scroll = self.view_scroll.saturating_add(VIEW_PAGE).min(65_535);
    }
}

#[derive(Debug)]
struct NoteEditorState {
    title_input: String,
    title_active: bool, // true = редактируем title, false = редактируем body
    body: TextArea<'static>,
}

#[derive(Debug)]
enum NoteEditorResult {
    Continue,
    Save(String, String),
    Cancel,
}

/// Снапшот вычисленных прямоугольников layout для hit-testing мыши.
#[derive(Debug, Clone, Default)]
struct LayoutSnapshot {
    /// v0.17.4: прямоугольник окна Transcript (оверлей F3) для кликов.
    transcript: Option<Rect>,
    nb_area: Rect,
    chat_area: Rect,
    input_area: Rect,
    notes_area: Rect,
    sources_area: Rect,
    status_area: Rect,
    palette_area: Option<Rect>,
}

impl LayoutSnapshot {
    /// Прямоугольник окна Transcript (совпадает с рендером centered_rect).
    fn transcript_area(&self) -> Option<Rect> {
        self.transcript
    }
    fn palette_area(&self) -> Option<Rect> {
        self.palette_area
    }
}

fn compute_layout(area: Rect) -> LayoutSnapshot {
    // Layout:
    // ┌──────┬─────────────────┬──────┐
    // │ NB   │ Chat (50% h)     │ Notes│
    // │      ├─────────────────┼──────┤
    // │      │ Input (25% h)    │Src   │
    // ├──────┴─────────────────┴──────┤
    // │ status bar                    │
    // └───────────────────────────────┘
    // Ширина: 25% / 50% / 25%
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    let main = outer[0];
    let status = outer[1];

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(50),
            Constraint::Percentage(25),
        ])
        .split(main);
    let nb_area = cols[0];
    let center = cols[1];
    let right = cols[2];

    let center_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Min(3)])
        .split(center);
    let chat_area = center_rows[0];
    let input_area = center_rows[1];

    let right_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(right);
    let notes_area = right_rows[0];
    let sources_area = right_rows[1];

    LayoutSnapshot {
        nb_area,
        chat_area,
        input_area,
        notes_area,
        sources_area,
        status_area: status,
        palette_area: None,
        transcript: Some(centered_rect(88, 84, area)),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_ui(
    f: &mut ratatui::Frame,
    layout: &LayoutSnapshot,
    input_buf: &str,
    output_lines: &[String],
    output_state: &ListState,
    notebooks: &[String],
    nb_state: &ListState,
    notes_items: &[String],
    notes_state: &ListState,
    sources_items: &[String],
    sources_state: &ListState,
    focus: Focus,
    selection: &SelectionRect,
    state: &ShellState,
    mode: &Mode,
    palette_state: &ListState,
) {
    // Левая панель — репозитории/ноутбуки
    let nb_items: Vec<ListItem> = notebooks
        .iter()
        .map(|s| ListItem::new(Line::from(s.clone())))
        .collect();
    let nb_block = Block::default()
        .borders(Borders::ALL)
        .title("Repo / Notebooks (r=refresh)")
        .border_style(if matches!(focus, Focus::Notebooks) {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        });
    let nb_widget = List::new(nb_items)
        .block(nb_block)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    f.render_stateful_widget(nb_widget, layout.nb_area, &mut nb_state.clone());

    // Центр верх — Chat / Output (с drag-select overlay)
    let chat_block = Block::default()
        .borders(Borders::ALL)
        .title("Chat & Responses (drag-select → Ctrl+Y)")
        .border_style(if matches!(focus, Focus::Output) {
            Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        });
    let chat_inner = chat_block.inner(layout.chat_area);
    // Рендерим текст как один Paragraph (с Wrap)
    let chat_text = output_lines.join("\n");
    let chat_para = Paragraph::new(chat_text).wrap(Wrap { trim: false });
    f.render_widget(chat_para, chat_inner);

    // Drag-select overlay — рамка выделения
    if let Some((min_col, min_row, max_col, max_row)) = selection.bbox() {
        let sel_area = Rect::new(min_col, min_row, max_col - min_col + 1, max_row - min_row + 1);
        let sel_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        f.render_widget(sel_block, sel_area);
    }

    // Border Chat panel — рисуем ПОВЕРХ overlay
    f.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title("Chat & Responses (drag-select → Ctrl+Y)")
            .border_style(if matches!(focus, Focus::Output) {
                Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            }),
        layout.chat_area,
    );

    // Центр низ — Input
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title("Input (Enter=run, ↑↓=history, ?=palette)")
        .border_style(if matches!(focus, Focus::Input) {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        });
    let input_para = Paragraph::new(format!("poler> {}", input_buf))
        .block(input_block)
        .style(Style::default().fg(Color::White));
    f.render_widget(input_para, layout.input_area);

    // Правая верх — Notes
    let notes_block = Block::default()
        .borders(Borders::ALL)
        .title("Notes (Ctrl+N=new, Ctrl+S=save AI)")
        .border_style(if matches!(focus, Focus::Notes) {
            Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        });
    let notes_list_items: Vec<ListItem> = notes_items
        .iter()
        .map(|s| ListItem::new(Line::from(s.clone())))
        .collect();
    let notes_widget = List::new(notes_list_items)
        .block(notes_block)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Magenta));
    f.render_stateful_widget(notes_widget, layout.notes_area, &mut notes_state.clone());

    // Правая низ — Sources
    let sources_block = Block::default()
        .borders(Borders::ALL)
        .title("Sources (click=open, Ctrl+T=test)")
        .border_style(if matches!(focus, Focus::Sources) {
            Style::default().fg(Color::Blue).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        });
    let sources_list_items: Vec<ListItem> = sources_items
        .iter()
        .map(|s| ListItem::new(Line::from(s.clone())))
        .collect();
    let sources_widget = List::new(sources_list_items)
        .block(sources_block)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Blue));
    f.render_stateful_widget(sources_widget, layout.sources_area, &mut sources_state.clone());

    // Status bar
    let status_text = format!(
        " poler-shell {}  │  db: {:?}  │  fmt: {:?}  │  top: {}  │  focus: {}  │  F2=Chat  F3=лента чата  ?=palette  Ctrl+N=note  Ctrl+S=save AI",
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
    f.render_widget(status_para, layout.status_area);

    // Mode overlays
    match mode {
        Mode::Palette => {
            render_palette_overlay(f, palette_state);
        }
        Mode::NoteEditor(editor_state) => {
            render_note_editor_overlay(f, editor_state);
        }
        Mode::Transcript(ts) => {
            render_transcript_overlay(f, ts);
        }
        Mode::Normal => {}
    }
}

/// v0.17.4: окно Transcript / Response View.
///
/// Лента (viewing == None): список пар `#id [время] NB вопрос → N симв.`
/// в feed-порядке (новые снизу). Response View (Some): вопрос в шапке,
/// полный ответ с прокруткой — восстановление «Історія чату | Відповідь»
/// Ask-вкладки Web GUI (удалён в v0.17.0).
fn render_transcript_overlay(f: &mut ratatui::Frame, ts: &TranscriptState) {
    let area = centered_rect(88, 84, f.size());
    f.render_widget(Clear, area);

    if let Some(idx) = ts.viewing {
        if let Some(e) = ts.entries.get(idx) {
            // ----- Response View -----
            let block = Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    " Відповідь #{} · {} · {} симв. ",
                    e.id,
                    transcript::format_ts(e.created_at),
                    e.answer.chars().count()
                ))
                .border_style(Style::default().fg(Color::Green).add_modifier(Modifier::BOLD));
            let inner = {
                let b = block.inner(area);
                f.render_widget(&block, area);
                b
            };
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3), // вопрос
                    Constraint::Min(3),    // ответ
                    Constraint::Length(2), // статус/подсказки
                ])
                .split(inner);
            // Вопрос (шапка).
            let nb = e.notebook_id.as_deref().unwrap_or("—").to_string();
            let q_text = format!("❯ {}  [{}]", e.question, nb);
            let q_block = Block::default()
                .borders(Borders::ALL)
                .title(" Питання ")
                .border_style(Style::default().fg(Color::Yellow));
            let q_para = Paragraph::new(q_text)
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(Color::Yellow));
            f.render_widget(q_para, q_block.inner(chunks[0]));
            f.render_widget(q_block, chunks[0]);
            // Ответ (прокручиваемый).
            let a_block = Block::default()
                .borders(Borders::ALL)
                .title(" Відповідь (↑↓/PgUp/PgDn — прокрутка) ")
                .border_style(Style::default().fg(Color::Green));
            let a_para = Paragraph::new(e.answer.as_str())
                .wrap(Wrap { trim: false })
                .scroll((ts.view_scroll, 0));
            f.render_widget(a_para, a_block.inner(chunks[1]));
            f.render_widget(a_block, chunks[1]);
            // Статус/подсказки.
            let hint = if ts.status.is_empty() {
                " y — копировать ответ · r — обновить · Esc — к ленте ".to_string()
            } else {
                format!(" {} · Esc — к ленте ", ts.status)
            };
            let hint_para = Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            )));
            f.render_widget(hint_para, chunks[2]);
            return;
        }
    }

    // ----- Лента пар -----
    let title = if ts.entries.is_empty() {
        " Transcript — лента чата (пусто: nlm ask <NB> \"вопрос\") ".to_string()
    } else {
        format!(
            " Transcript — лента чата: {} пар ({} всего) · F3 ",
            ts.entries.len(),
            ts.total
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));
    let inner = {
        let b = block.inner(area);
        f.render_widget(&block, area);
        b
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
        .split(inner);
    if ts.entries.is_empty() {
        let empty = Paragraph::new(
            "Лента пуста. Выполните nlm ask — каждая пара вопрос→ответ
             автоматически попадает сюда и переживает перезапуски.",
        )
        .style(Style::default().fg(Color::DarkGray));
        f.render_widget(empty, chunks[0]);
    } else {
        let items: Vec<ListItem> = ts
            .entries
            .iter()
            .map(|e| ListItem::new(transcript::feed_line(e)))
            .collect();
        let list = List::new(items)
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");
        f.render_stateful_widget(list, chunks[0], &mut ts.list.clone());
    }
    let hint = if ts.status.is_empty() {
        " ↑↓ — навигация · Enter — ответ · y — копировать · d — удалить · r — обновить · Esc — закрыть ".to_string()
    } else {
        format!(" {} ", ts.status)
    };
    let hint_para = Paragraph::new(Line::from(Span::styled(
        hint,
        Style::default().fg(Color::DarkGray),
    )));
    f.render_widget(hint_para, chunks[1]);
}

fn render_palette_overlay(f: &mut ratatui::Frame, palette_state: &ListState) {
    let area = centered_rect(80, 70, f.area());
    let scenarios = help::palette_scenarios();
    let items: Vec<ListItem> = scenarios
        .iter()
        .map(|sc| ListItem::new(Line::from(format!("{}. {}", sc.title, sc.cmd))))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title("? palette — 10 сценариев (↑↓=select, Enter=use, Esc=cancel)")
        .border_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
    let widget = List::new(items)
        .block(block)
        .highlight_style(Style::default().fg(Color::Black).bg(Color::Yellow));
    f.render_widget(Clear, area);
    f.render_stateful_widget(widget, area, &mut palette_state.clone());
}

fn render_note_editor_overlay(f: &mut ratatui::Frame, editor: &NoteEditorState) {
    let area = centered_rect(80, 70, f.area());
    f.render_widget(Clear, area);

    // Title input
    let title_area = Rect::new(area.x, area.y, area.width, 3);
    let title_block = Block::default()
        .borders(Borders::ALL)
        .title(if editor.title_active {
            "Title (TAB → body)"
        } else {
            "Title (TAB → body)"
        })
        .border_style(if editor.title_active {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        });
    let title_para = Paragraph::new(editor.title_input.as_str())
        .block(title_block)
        .style(Style::default().fg(Color::White));
    f.render_widget(title_para, title_area);

    // Body textarea
    let body_area = Rect::new(area.x, area.y + 3, area.width, area.height.saturating_sub(3));
    let body_block = Block::default()
        .borders(Borders::ALL)
        .title("Body (Ctrl+S=save, Esc=cancel)")
        .border_style(if !editor.title_active {
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        });
    f.render_widget(&editor.body, body_area);
    f.render_widget(body_block, body_area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let popup = popup_layout[1];
    let popup_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup);
    popup_layout[1]
}

#[allow(clippy::too_many_arguments)]
fn handle_key_event(
    k: KeyEvent,
    state: &mut ShellState,
    input_buf: &mut String,
    input_history: &mut Vec<String>,
    input_history_idx: &mut Option<usize>,
    output_lines: &mut Vec<String>,
    output_scroll: &mut usize,
    output_state: &mut ListState,
    notebooks: &mut Vec<String>,
    notebook_ids: &mut Vec<String>,
    nb_state: &mut ListState,
    notes_items: &mut Vec<String>,
    notes_state: &mut ListState,
    sources_items: &mut Vec<String>,
    sources_state: &mut ListState,
    focus: &mut Focus,
    should_quit: &mut bool,
    mode: &mut Mode,
    selection: &mut SelectionRect,
) {
    match (k.code, k.modifiers) {
        (KeyCode::F(3), _) => {
            // v0.17.4: Transcript — лента чата nlm ask (окно-оверлей).
            *mode = Mode::Transcript(TranscriptState::new(state));
        }
        (KeyCode::Esc, _) => {
            if selection.active {
                selection.clear();
            } else {
                *should_quit = true;
            }
        }
        (KeyCode::Tab, _) => {
            *focus = focus.next();
        }
        (KeyCode::BackTab, _) => {
            *focus = focus.prev();
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            *should_quit = true;
        }
        (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
            // Ctrl+Y: скопировать выделение (если активно) или весь последний вывод
            if selection.active {
                if let Some((min_col, min_row, max_col, max_row)) = selection.bbox() {
                    let text = mouse::extract_text(output_lines, &Rect::new(0, 0, 200, 1000), min_col, min_row, max_col, max_row);
                    match mouse::copy_to_clipboard(&text) {
                        Ok(()) => output_lines.push(format!("✓ Скопировано {} символов в буфер", text.chars().count())),
                        Err(e) => output_lines.push(format!("❌ clipboard: {e}")),
                    }
                    selection.clear();
                }
            } else if !state.last_output.is_empty() {
                match mouse::copy_to_clipboard(&state.last_output) {
                    Ok(()) => output_lines.push(format!("✓ Скопирован весь вывод ({} символов)", state.last_output.chars().count())),
                    Err(e) => output_lines.push(format!("❌ clipboard: {e}")),
                }
            } else {
                output_lines.push("ℹ Нет выделения и нет последнего вывода".into());
            }
            *output_scroll = output_lines.len().saturating_sub(1);
        }
        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
            // Ctrl+N: новая заметка (встроенный редактор)
            let mut body = TextArea::default();
            body.set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Note body (Ctrl+S=save, Esc=cancel)")
                    .border_style(Style::default().fg(Color::Yellow)),
            );
            *mode = Mode::NoteEditor(NoteEditorState {
                title_input: String::new(),
                title_active: true,
                body,
            });
        }
        (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
            // Ctrl+S: сохранить последний AI-ответ как заметку
            let title = format!(
                "AI reply @{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0)
            );
            match state.save_last_ai_reply_as_note(&title) {
                Ok(id) => {
                    output_lines.push(format!("✓ Сохранён AI-ответ как заметка #{id} «{title}»"));
                    refresh_notes_list(state, notes_items);
                }
                Err(e) => output_lines.push(format!("❌ {e}")),
            }
            *output_scroll = output_lines.len().saturating_sub(1);
        }
        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
            // Ctrl+E: редактировать выбранную заметку → открыть редактор
            if let Some(idx) = notes_state.selected() {
                if let Ok(conn) = state.ensure_notes_conn() {
                    let all = crate::notes::list_notes(conn, 500).unwrap_or_default();
                    if idx < all.len() {
                        let n = &all[idx];
                        let mut body = TextArea::default();
                        body.set_block(
                            Block::default()
                                .borders(Borders::ALL)
                                .title(format!("Edit #{} (Ctrl+S=save, Esc=cancel)", n.id))
                                .border_style(Style::default().fg(Color::Yellow)),
                        );
                        // Вставить существующий текст
                        for line in n.body.lines() {
                            body.insert_str(line);
                            body.insert_newline();
                        }
                        *mode = Mode::NoteEditor(NoteEditorState {
                            title_input: n.title.clone(),
                            title_active: true,
                            body,
                        });
                    }
                }
            }
        }
        (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
            // Ctrl+D: удалить выбранную заметку
            if matches!(focus, Focus::Notes) {
                if let Some(idx) = notes_state.selected() {
                    if let Ok(conn) = state.ensure_notes_conn() {
                        let all = crate::notes::list_notes(conn, 500).unwrap_or_default();
                        if idx < all.len() {
                            let id = all[idx].id;
                            match crate::notes::delete_note(conn, id) {
                                Ok(()) => {
                                    output_lines.push(format!("✓ Заметка #{id} удалена"));
                                    refresh_notes_list(state, notes_items);
                                }
                                Err(e) => output_lines.push(format!("❌ {e}")),
                            }
                            *output_scroll = output_lines.len().saturating_sub(1);
                        }
                    }
                }
            } else if matches!(focus, Focus::Sources) {
                if let Some(idx) = sources_state.selected() {
                    if let Ok(conn) = state.ensure_sources_conn() {
                        let all = crate::sources::list_sources(conn, 500).unwrap_or_default();
                        if idx < all.len() {
                            let id = all[idx].id;
                            match crate::sources::delete_source(conn, id) {
                                Ok(()) => {
                                    output_lines.push(format!("✓ Источник #{id} удалён"));
                                    refresh_sources_list(state, sources_items);
                                }
                                Err(e) => output_lines.push(format!("❌ {e}")),
                            }
                            *output_scroll = output_lines.len().saturating_sub(1);
                        }
                    }
                }
            }
        }
        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
            // Ctrl+T: тестировать источник
            if matches!(focus, Focus::Sources) {
                if let Some(idx) = sources_state.selected() {
                    if let Ok(conn) = state.ensure_sources_conn() {
                        let all = crate::sources::list_sources(conn, 500).unwrap_or_default();
                        if idx < all.len() {
                            let id = all[idx].id;
                            match crate::sources::test_source(conn, id) {
                                Ok(crate::sources::TestStatus::Ok) => {
                                    output_lines.push(format!("✓ #{id}: доступен"));
                                }
                                Ok(crate::sources::TestStatus::Fail) => {
                                    output_lines.push(format!("✗ #{id}: недоступен"));
                                }
                                Err(e) => output_lines.push(format!("❌ {e}")),
                            }
                            refresh_sources_list(state, sources_items);
                            *output_scroll = output_lines.len().saturating_sub(1);
                        }
                    }
                }
            }
        }
        (KeyCode::Enter, _) if matches!(focus, Focus::Sources) => {
            // M4: Enter-handler на источнике → companion::SourceKind::enter_action.
            // Источник из poler_sources маппится в SourceKind Companion Bridge:
            //   File  → FileUpload { local_path } → EditLocal(path)
            //   Url   → Web { url } → OpenUrl(url)
            //   Repo  → Web { "https://github.com/{value}" } → OpenUrl
            if let Some(idx) = sources_state.selected() {
                if let Ok(conn) = state.ensure_sources_conn() {
                    let all = crate::sources::list_sources(conn, 500).unwrap_or_default();
                    if idx < all.len() {
                        let src = &all[idx];
                        let kind = match src.kind {
                            crate::sources::SourceKind::File => {
                                crate::google::companion::SourceKind::FileUpload {
                                    local_path: src.value.clone(),
                                }
                            }
                            crate::sources::SourceKind::Url => {
                                crate::google::companion::SourceKind::Web {
                                    url: src.value.clone(),
                                }
                            }
                            crate::sources::SourceKind::Repo => {
                                crate::google::companion::SourceKind::Web {
                                    url: format!("https://github.com/{}", src.value),
                                }
                            }
                        };
                        let action = kind.enter_action(&src.id.to_string());
                        execute_enter_action(&action, output_lines);
                    }
                }
            }
            *output_scroll = output_lines.len().saturating_sub(1);
        }
        (KeyCode::Char('?'), _) => {
            // ? palette
            *mode = Mode::Palette;
        }
        (KeyCode::Char('r'), _) if matches!(focus, Focus::Notebooks) => {
            // Обновить список ноутбуков
            output_lines.push("poler> nlm list".into());
            let r = dispatch(state, "nlm list");
            if let CmdResult::Done(out) = r {
                let mut new_list = Vec::new();
                notebook_ids.clear();
                for l in out.lines() {
                    if l.contains("704f") || l.contains('-') && l.len() > 30 {
                        // Похоже на notebook UUID
                        let id = l.split_whitespace().next().unwrap_or("").to_string();
                        if id.len() >= 8 {
                            notebook_ids.push(id.clone());
                            new_list.push(l.to_string());
                            continue;
                        }
                    }
                    new_list.push(l.to_string());
                }
                if new_list.is_empty() {
                    new_list = vec!["(пусто)".into()];
                }
                *notebooks = new_list;
            }
            *output_scroll = output_lines.len().saturating_sub(1);
        }
        (KeyCode::PageUp, _) if matches!(focus, Focus::Output) || matches!(focus, Focus::Input) => {
            *output_scroll = output_scroll.saturating_sub(5);
        }
        (KeyCode::PageDown, _) if matches!(focus, Focus::Output) || matches!(focus, Focus::Input) => {
            let max = output_lines.len().saturating_sub(1);
            *output_scroll = (*output_scroll + 5).min(max);
        }
        (KeyCode::Up, _) if matches!(focus, Focus::Input) => {
            if !input_history.is_empty() {
                *input_history_idx = Some(match *input_history_idx {
                    None => input_history.len() - 1,
                    Some(i) if i > 0 => i - 1,
                    Some(i) => i,
                });
                if let Some(i) = *input_history_idx {
                    *input_buf = input_history[i].clone();
                }
            }
        }
        (KeyCode::Down, _) if matches!(focus, Focus::Input) => {
            if !input_history.is_empty() {
                *input_history_idx = match *input_history_idx {
                    None => None,
                    Some(i) if i + 1 < input_history.len() => Some(i + 1),
                    _ => None,
                };
                *input_buf = match *input_history_idx {
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
        (KeyCode::Up, _) if matches!(focus, Focus::Notes) => {
            let idx = notes_state.selected().unwrap_or(0);
            notes_state.select(Some(idx.saturating_sub(1)));
        }
        (KeyCode::Down, _) if matches!(focus, Focus::Notes) => {
            let idx = notes_state.selected().unwrap_or(0);
            let max = notes_items.len().saturating_sub(1);
            notes_state.select(Some((idx + 1).min(max)));
        }
        (KeyCode::Up, _) if matches!(focus, Focus::Sources) => {
            let idx = sources_state.selected().unwrap_or(0);
            sources_state.select(Some(idx.saturating_sub(1)));
        }
        (KeyCode::Down, _) if matches!(focus, Focus::Sources) => {
            let idx = sources_state.selected().unwrap_or(0);
            let max = sources_items.len().saturating_sub(1);
            sources_state.select(Some((idx + 1).min(max)));
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
                return;
            }
            input_history.push(trimmed.to_string());
            *input_history_idx = None;
            input_buf.clear();

            // Спец-выход TUI
            if matches!(trimmed, "quit" | "exit" | "q") {
                *should_quit = true;
                return;
            }
            let r = dispatch(state, trimmed);
            match r {
                CmdResult::Quit => {
                    *should_quit = true;
                }
                CmdResult::Empty => {}
                CmdResult::Done(out) => {
                    // После выполнения команды обновим notes/sources если это была CRUD
                    if trimmed.starts_with("notes") {
                        refresh_notes_list(state, notes_items);
                    }
                    if trimmed.starts_with("sources") {
                        refresh_sources_list(state, sources_items);
                    }
                    for l in out.lines() {
                        output_lines.push(l.to_string());
                    }
                    output_lines.push(String::new());
                    *output_scroll = output_lines.len().saturating_sub(1);
                }
            }
        }
        _ => {}
    }
}

/// M4: Выполнить `companion::EnterAction` для источника из poler_sources.
///
/// | EnterAction | Действие |
/// |---|---|
/// | `OpenUrl(url)` | `crate::google::open_in_user_browser(url)` (xdg-open) |
/// | `EditLocal(path)` | spawn `$EDITOR` с локальным файлом (fire-and-forget) |
/// | `EditTemp { content, filename }` | записать в `/tmp/{filename}` + spawn `$EDITOR` |
/// | `FallbackFetch { src_id }` | сообщение: HybridProvider.get_source_content ещё skeleton |
///
/// TUI raw-mode остаётся активным — spawned editor открывается в отдельном
/// процессе; для интерактивного редактирования пользователь переключается
/// на него (Ctrl+Z в большинстве терминалов), либо завершает TUI и
/// повторно открывает. Это сознательное упрощение M4: интегрировать
/// `tui-textarea` как viewer произвольных файлов — отдельная задача.
fn execute_enter_action(
    action: &crate::google::companion::EnterAction,
    output_lines: &mut Vec<String>,
) {
    use crate::google::companion::EnterAction;
    match action {
        EnterAction::OpenUrl(url) => {
            crate::google::open_in_user_browser(url);
            output_lines.push(format!("→ открыть в браузере: {}", url));
        }
        EnterAction::EditLocal(path) => {
            if !std::path::Path::new(path).exists() {
                output_lines.push(format!("❌ файл не найден: {}", path));
                return;
            }
            spawn_editor(path);
            output_lines.push(format!(
                "→ открыть в $EDITOR ({}): {}",
                std::env::var("EDITOR").unwrap_or_else(|_| "(nano)".into()),
                path
            ));
        }
        EnterAction::EditTemp {
            content,
            suggested_filename,
        } => {
            let tmp = std::env::temp_dir().join(suggested_filename);
            if let Err(e) = std::fs::write(&tmp, content) {
                output_lines.push(format!(
                    "❌ не удалось записать {}: {}",
                    tmp.display(),
                    e
                ));
                return;
            }
            let s = tmp.to_string_lossy().to_string();
            spawn_editor(&s);
            output_lines.push(format!("→ открыть в $EDITOR (temp): {}", s));
        }
        EnterAction::FallbackFetch { src_id } => {
            output_lines.push(format!(
                "↻ источник #{}: тип неизвестен. HybridProvider.get_source_content ещё skeleton — \
                 M5/M6 добавит CdpBatchexecuteProvider.get_source_content (batchexecute hizoJc).",
                src_id
            ));
        }
    }
}

/// Spawn `$EDITOR` (fallback `nano`) с путём. Fire-and-forget: не блокирует
/// TUI, но оставляет child-процесс запущенным. Пользователь переключается
/// вручную (терминальный Ctrl+Z или открытие нового окна терминала).
fn spawn_editor(path: &str) {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "nano".to_string());
    let _ = std::process::Command::new(&editor)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

#[allow(clippy::too_many_arguments)]
fn handle_mouse_event(
    me: MouseEvent,
    layout: &LayoutSnapshot,
    state: &mut ShellState,
    input_buf: &mut String,
    input_history: &mut Vec<String>,
    input_history_idx: &mut Option<usize>,
    output_lines: &mut Vec<String>,
    output_scroll: &mut usize,
    _output_state: &mut ListState,
    notebooks: &mut Vec<String>,
    notebook_ids: &mut Vec<String>,
    nb_state: &mut ListState,
    notes_items: &mut Vec<String>,
    notes_state: &mut ListState,
    sources_items: &mut Vec<String>,
    sources_state: &mut ListState,
    focus: &mut Focus,
    selection: &mut SelectionRect,
    last_click_time: &mut Option<Instant>,
    last_click_pos: &mut Option<(u16, u16)>,
) {
    let action = mouse::parse_event(me);
    match action {
        MouseAction::Ignore => {}
        MouseAction::DragStart { col, row } => {
            // Drag начинается только если клик в Chat panel
            if mouse::hit(&layout.chat_area, col, row) {
                selection.start(col, row);
                *focus = Focus::Output;
            }
        }
        MouseAction::DragMove { col, row } => {
            selection.extend(col, row);
        }
        MouseAction::DragEnd { col, row } => {
            // Если это был drag с активным выделением — финализируем и копируем
            if selection.active {
                if let Some((min_col, min_row, max_col, max_row)) = selection.finish() {
                    // Проверим что был реальный drag (не клик)
                    let was_drag = (min_col != max_col) || (min_row != max_row);
                    if was_drag {
                        // Копируем выделение в буфер автоматически
                        let text = mouse::extract_text(
                            output_lines,
                            &Rect::new(0, 0, 200, 1000),
                            min_col,
                            min_row,
                            max_col,
                            max_row,
                        );
                        match mouse::copy_to_clipboard(&text) {
                            Ok(()) => output_lines.push(format!(
                                "✓ Скопировано {} символов (drag-select)",
                                text.chars().count()
                            )),
                            Err(e) => output_lines.push(format!("❌ clipboard: {e}")),
                        }
                        *output_scroll = output_lines.len().saturating_sub(1);
                    } else {
                        // Это был одиночный клик — обрабатываем как клик
                        handle_single_click(
                            col, row, layout, state, input_buf, input_history,
                            input_history_idx, output_lines, output_scroll,
                            notebooks, notebook_ids, nb_state, notes_items, notes_state,
                            sources_items, sources_state, focus, last_click_time, last_click_pos,
                        );
                    }
                }
            } else {
                // Событие Up без активного drag — обрабатываем как одиночный клик
                handle_single_click(
                    col, row, layout, state, input_buf, input_history,
                    input_history_idx, output_lines, output_scroll,
                    notebooks, notebook_ids, nb_state, notes_items, notes_state,
                    sources_items, sources_state, focus, last_click_time, last_click_pos,
                );
            }
        }
        MouseAction::Click { .. } | MouseAction::DoubleClick { .. } => {
            // Используется только в handle_single_click через timing
        }
        MouseAction::ScrollUp => {
            if mouse::hit(&layout.chat_area, me.column, me.row) {
                *output_scroll = output_scroll.saturating_sub(3);
            } else if mouse::hit(&layout.nb_area, me.column, me.row) {
                let idx = nb_state.selected().unwrap_or(0);
                nb_state.select(Some(idx.saturating_sub(1)));
            } else if mouse::hit(&layout.notes_area, me.column, me.row) {
                let idx = notes_state.selected().unwrap_or(0);
                notes_state.select(Some(idx.saturating_sub(1)));
            } else if mouse::hit(&layout.sources_area, me.column, me.row) {
                let idx = sources_state.selected().unwrap_or(0);
                sources_state.select(Some(idx.saturating_sub(1)));
            }
        }
        MouseAction::ScrollDown => {
            if mouse::hit(&layout.chat_area, me.column, me.row) {
                let max = output_lines.len().saturating_sub(1);
                *output_scroll = (*output_scroll + 3).min(max);
            } else if mouse::hit(&layout.nb_area, me.column, me.row) {
                let idx = nb_state.selected().unwrap_or(0);
                let max = notebooks.len().saturating_sub(1);
                nb_state.select(Some((idx + 1).min(max)));
            } else if mouse::hit(&layout.notes_area, me.column, me.row) {
                let idx = notes_state.selected().unwrap_or(0);
                let max = notes_items.len().saturating_sub(1);
                notes_state.select(Some((idx + 1).min(max)));
            } else if mouse::hit(&layout.sources_area, me.column, me.row) {
                let idx = sources_state.selected().unwrap_or(0);
                let max = sources_items.len().saturating_sub(1);
                sources_state.select(Some((idx + 1).min(max)));
            }
        }
        MouseAction::RightClick { col, row } => {
            // Правый клик по источнику → открыть в xdg-open
            if mouse::hit(&layout.sources_area, col, row) {
                if let Some(idx) = sources_state.selected() {
                    if let Ok(conn) = state.ensure_sources_conn() {
                        let all = crate::sources::list_sources(conn, 500).unwrap_or_default();
                        if idx < all.len() {
                            let id = all[idx].id;
                            match crate::sources::open_source(conn, id) {
                                Ok(()) => {
                                    output_lines.push(format!("✓ #{id}: отправлено в xdg-open"));
                                    *output_scroll = output_lines.len().saturating_sub(1);
                                }
                                Err(e) => {
                                    output_lines.push(format!("❌ {e}"));
                                    *output_scroll = output_lines.len().saturating_sub(1);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_single_click(
    col: u16,
    row: u16,
    layout: &LayoutSnapshot,
    state: &mut ShellState,
    _input_buf: &mut String,
    _input_history: &mut Vec<String>,
    _input_history_idx: &mut Option<usize>,
    output_lines: &mut Vec<String>,
    output_scroll: &mut usize,
    notebooks: &[String],
    notebook_ids: &mut Vec<String>,
    nb_state: &mut ListState,
    _notes_items: &[String],
    notes_state: &mut ListState,
    _sources_items: &[String],
    sources_state: &mut ListState,
    focus: &mut Focus,
    last_click_time: &mut Option<Instant>,
    _last_click_pos: &mut Option<(u16, u16)>,
) {
    // Клик по левой панели → выбор ноутбука
    if mouse::hit(&layout.nb_area, col, row) {
        *focus = Focus::Notebooks;
        let local_row = row.saturating_sub(layout.nb_area.y) as usize;
        // Пропускаем рамку → строка 1 = индекс 0
        let local_row = local_row.saturating_sub(1);
        if local_row < notebooks.len() {
            nb_state.select(Some(local_row));
            // Если есть notebook_ids для этого индекса → активируем
            if local_row < notebook_ids.len() {
                let id = notebook_ids[local_row].clone();
                state.set_active_notebook(Some(id.clone()));
                output_lines.push(format!("✓ Активирован ноутбук {} ({})", local_row + 1, &id[..id.len().min(8)]));
                *output_scroll = output_lines.len().saturating_sub(1);
                // Двойной клик → nlm sync <id>
                let now = Instant::now();
                let is_double = last_click_time
                    .map(|t| now.duration_since(t).as_millis() < 400)
                    .unwrap_or(false);
                if is_double {
                    output_lines.push(format!("poler> nlm sync {}", id));
                    let r = dispatch(state, &format!("nlm sync {}", id));
                    if let CmdResult::Done(out) = r {
                        for l in out.lines() {
                            output_lines.push(l.to_string());
                        }
                        output_lines.push(String::new());
                        *output_scroll = output_lines.len().saturating_sub(1);
                    }
                }
                *last_click_time = Some(now);
            }
        }
        return;
    }

    // Клик по Notes → выбор + переход фокуса
    if mouse::hit(&layout.notes_area, col, row) {
        *focus = Focus::Notes;
        let local_row = row.saturating_sub(layout.notes_area.y).saturating_sub(1) as usize;
        notes_state.select(Some(local_row));
        return;
    }

    // Клик по Sources → выбор
    if mouse::hit(&layout.sources_area, col, row) {
        *focus = Focus::Sources;
        let local_row = row.saturating_sub(layout.sources_area.y).saturating_sub(1) as usize;
        sources_state.select(Some(local_row));
        return;
    }

    // Клик по Chat panel → фокус + клир выделения
    if mouse::hit(&layout.chat_area, col, row) {
        *focus = Focus::Output;
        return;
    }

    // Клик по Input panel → фокус
    if mouse::hit(&layout.input_area, col, row) {
        *focus = Focus::Input;
        return;
    }
}

fn handle_note_editor_event(ev: Event, state: &mut NoteEditorState) -> NoteEditorResult {
    if let Event::Key(k) = ev {
        match (k.code, k.modifiers) {
            (KeyCode::Esc, _) => return NoteEditorResult::Cancel,
            (KeyCode::Char('s'), KeyModifiers::CONTROL) => {
                let body = state.body.lines().join("\n");
                return NoteEditorResult::Save(state.title_input.clone(), body);
            }
            (KeyCode::Tab, _) => {
                state.title_active = !state.title_active;
                return NoteEditorResult::Continue;
            }
            (KeyCode::BackTab, _) => {
                state.title_active = !state.title_active;
                return NoteEditorResult::Continue;
            }
            (KeyCode::Char(c), _) if state.title_active => {
                state.title_input.push(c);
                return NoteEditorResult::Continue;
            }
            (KeyCode::Backspace, _) if state.title_active => {
                state.title_input.pop();
                return NoteEditorResult::Continue;
            }
            // Все остальные клавиши идут в body (если title не активен)
            _ if !state.title_active => {
                state.body.input(tui_textarea::Input::from(k));
                return NoteEditorResult::Continue;
            }
            _ => {}
        }
    }
    NoteEditorResult::Continue
}

fn refresh_notes_list(state: &mut ShellState, notes_items: &mut Vec<String>) {
    if let Ok(conn) = state.ensure_notes_conn() {
        let all = crate::notes::list_notes(conn, 500).unwrap_or_default();
        if all.is_empty() {
            *notes_items = vec!["(нет заметок — Ctrl+N)".into()];
        } else {
            *notes_items = all
                .iter()
                .map(|n| {
                    let preview = n.body.lines().next().unwrap_or("").chars().take(40).collect::<String>();
                    format!("#{} {} {}", n.id, n.title, if preview.is_empty() { String::new() } else { format!("— {}", preview) })
                })
                .collect();
        }
    }
}

fn refresh_sources_list(state: &mut ShellState, sources_items: &mut Vec<String>) {
    if let Ok(conn) = state.ensure_sources_conn() {
        let all = crate::sources::list_sources(conn, 500).unwrap_or_default();
        if all.is_empty() {
            *sources_items = vec!["(нет источников — sources add)".into()];
        } else {
            *sources_items = all
                .iter()
                .map(|s| {
                    let lbl = s.label.as_ref().map(|l| format!(" ({})", l)).unwrap_or_default();
                    let st = match s.last_status.as_str() {
                        "ok" => "✓",
                        "fail" => "✗",
                        _ => "?",
                    };
                    format!("#{} [{}] {} {}{}", s.id, st, s.kind.as_str(), s.value, lbl)
                })
                .collect();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_cycle_next() {
        assert_eq!(Focus::Notebooks.next(), Focus::Input);
        assert_eq!(Focus::Input.next(), Focus::Output);
        assert_eq!(Focus::Output.next(), Focus::Notes);
        assert_eq!(Focus::Notes.next(), Focus::Sources);
        assert_eq!(Focus::Sources.next(), Focus::Notebooks);
    }

    #[test]
    fn focus_cycle_prev() {
        assert_eq!(Focus::Notebooks.prev(), Focus::Sources);
        assert_eq!(Focus::Sources.prev(), Focus::Notes);
        assert_eq!(Focus::Notes.prev(), Focus::Output);
        assert_eq!(Focus::Output.prev(), Focus::Input);
        assert_eq!(Focus::Input.prev(), Focus::Notebooks);
    }

    #[test]
    fn focus_as_str_correct() {
        assert_eq!(Focus::Notebooks.as_str(), "notebooks");
        assert_eq!(Focus::Input.as_str(), "input");
        assert_eq!(Focus::Output.as_str(), "chat");
        assert_eq!(Focus::Notes.as_str(), "notes");
        assert_eq!(Focus::Sources.as_str(), "sources");
    }

    #[test]
    fn layout_snapshot_default_is_empty() {
        let ls = LayoutSnapshot::default();
        assert_eq!(ls.nb_area, Rect::default());
        assert_eq!(ls.palette_area, None);
    }

    #[test]
    fn transcript_state_navigation() {
        let mut ts = TranscriptState {
            entries: vec![
                ChatEntry { id: 1, notebook_id: None, question: "q1".into(), answer: "a1".into(), created_at: 100 },
                ChatEntry { id: 2, notebook_id: Some("nb".into()), question: "q2".into(), answer: "ответ 2".into(), created_at: 200 },
            ],
            list: ListState::default(),
            viewing: None,
            view_scroll: 0,
            total: 2,
            status: String::new(),
        };
        ts.list.select(Some(1)); // самая свежая
        assert_eq!(ts.selected().unwrap().id, 2);
        ts.move_up();
        assert_eq!(ts.selected().unwrap().id, 1);
        ts.move_up();
        assert_eq!(ts.selected().unwrap().id, 1, "не выше первой");
        ts.move_down();
        ts.move_down();
        assert_eq!(ts.selected().unwrap().id, 2, "не ниже последней");
        // Response View
        ts.open_selected();
        assert_eq!(ts.viewing, Some(1));
        assert_eq!(ts.viewing_entry().unwrap().question, "q2");
        ts.view_scroll_down();
        assert_eq!(ts.view_scroll, 1);
        ts.view_page_up();
        assert_eq!(ts.view_scroll, 0, "скролл не уходит в минус");
    }

    #[test]
    fn transcript_overlay_renders_feed_and_response() {
        use ratatui::backend::TestBackend;
        let entries = vec![
            ChatEntry {
                id: 7,
                notebook_id: Some("nb-12345678".into()),
                question: "Как добавить квитки?".into(),
                answer: "Короткий ответ на вопрос.".into(),
                created_at: 1_787_920_496,
            },
        ];
        // Лента
        let mut ts = TranscriptState {
            entries: entries.clone(),
            list: ListState::default(),
            viewing: None,
            view_scroll: 0,
            total: 1,
            status: String::new(),
        };
        ts.list.select(Some(0));
        let backend = TestBackend::new(100, 30);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            render_transcript_overlay(f, &ts);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..buf.area.area() as usize)
            .map(|i| buf.content()[i].symbol().to_string())
            .collect();
        assert!(text.contains("Transcript"), "заголовок ленты: {text}");
        assert!(text.contains("Как добавить"), "строка пары: {text}");
        // Response View
        ts.viewing = Some(0);
        term.draw(|f| {
            render_transcript_overlay(f, &ts);
        })
        .unwrap();
        let buf2 = term.backend().buffer().clone();
        let text2: String = (0..buf2.area.area() as usize)
            .map(|i| buf2.content()[i].symbol().to_string())
            .collect();
        assert!(text2.contains("Відповідь #7"), "шапка ответа: {text2}");
        assert!(text2.contains("Питання"), "шапка вопроса: {text2}");
        assert!(text2.contains("Короткий ответ"), "тело ответа: {text2}");
    }

    #[test]
    fn transcript_overlay_empty_feed() {
        use ratatui::backend::TestBackend;
        let ts = TranscriptState {
            entries: Vec::new(),
            list: ListState::default(),
            viewing: None,
            view_scroll: 0,
            total: 0,
            status: String::new(),
        };
        let backend = TestBackend::new(80, 24);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| {
            render_transcript_overlay(f, &ts);
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..buf.area.area() as usize)
            .map(|i| buf.content()[i].symbol().to_string())
            .collect();
        assert!(text.contains("пусто"), "пустая лента: {text}");
    }
}