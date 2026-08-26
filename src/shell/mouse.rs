//! # Mouse capture + drag-select (v0.17.0)
//!
//! Полная поддержка мыши в терминале (как в mimocode от Xiaomi):
//!
//! - **Клик** на ноутбуке/источнике слева/справа → активирует его в центре
//! - **Двойной клик** на ноутбуке → мгновенный `nlm sync <NB_ID>`
//! - **Drag-select** на Chat panel → выделяет текст → Ctrl+Y копирует в буфер
//! - **Shift+Drag** → нативное выделение Linux (mouse capture отключается)
//!
//! ## Реализация drag-select
//!
//! Crossterm в raw-mode перехватывает все мышиные события. Когда пользователь
//! зажал ЛКМ на Chat panel, мы запоминаем стартовую (col,row). Каждый
//! `MouseEvent(MouseEventKind::Drag(MouseButton::Left, ...))` обновляет
//! прямоугольник выделения. `MouseEvent(MouseEventKind::Up(MouseButton::Left, ...))`
//! финализирует выделение и кладёт текст в буфер через `arboard`.
//!
//! Для Shift+Drag терминал сам передаёт нативное выделение (мышь в raw-mode
//! ловит только события БЕЗ модификаторов), поэтому гики получают привычное
//! поведение.

use crossterm::event::{MouseEvent, MouseEventKind, MouseButton, KeyModifiers};
use ratatui::layout::Rect;

/// Прямоугольник выделения мыши (в абсолютных координатах терминала).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelectionRect {
    pub start_col: u16,
    pub start_row: u16,
    pub end_col: u16,
    pub end_row: u16,
    pub active: bool,
}

impl SelectionRect {
    pub fn new() -> Self {
        Self::default()
    }

    /// Начать drag в точке (col, row).
    pub fn start(&mut self, col: u16, row: u16) {
        self.start_col = col;
        self.start_row = row;
        self.end_col = col;
        self.end_row = row;
        self.active = true;
    }

    /// Обновить текущее положение drag.
    pub fn extend(&mut self, col: u16, row: u16) {
        if self.active {
            self.end_col = col;
            self.end_row = row;
        }
    }

    /// Завершить drag. Возвращает `Some(rect)`, если выделение было
    /// непустым (хотя бы 1 ячейка).
    pub fn finish(&mut self) -> Option<(u16, u16, u16, u16)> {
        if !self.active {
            return None;
        }
        self.active = false;
        let (min_col, max_col) = if self.start_col <= self.end_col {
            (self.start_col, self.end_col)
        } else {
            (self.end_col, self.start_col)
        };
        let (min_row, max_row) = if self.start_row <= self.end_row {
            (self.start_row, self.end_row)
        } else {
            (self.end_row, self.start_row)
        };
        if min_col == max_col && min_row == max_row {
            None
        } else {
            Some((min_col, min_row, max_col, max_row))
        }
    }

    /// Отменить выделение (Esc, клик в другом месте).
    pub fn clear(&mut self) {
        self.active = false;
    }

    /// Нормализованный прямоугольник для рендеринга.
    pub fn bbox(&self) -> Option<(u16, u16, u16, u16)> {
        if !self.active {
            return None;
        }
        let (min_col, max_col) = if self.start_col <= self.end_col {
            (self.start_col, self.end_col)
        } else {
            (self.end_col, self.start_col)
        };
        let (min_row, max_row) = if self.start_row <= self.end_row {
            (self.start_row, self.end_row)
        } else {
            (self.end_row, self.start_row)
        };
        Some((min_col, min_row, max_col, max_row))
    }
}

/// Точка попадания клика (col, row) в прямоугольник панели.
pub fn hit(area: &Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// Перевести абсолютные (col,row) в координаты внутри прямоугольника.
pub fn to_local(area: &Rect, col: u16, row: u16) -> (u16, u16) {
    (col.saturating_sub(area.x), row.saturating_sub(area.y))
}

/// Извлечь текст из вектора строк по прямоугольнику выделения (абсолютные
/// координаты, чтобы работало между строками). `lines` — строки текста
/// (по одной ячейке на символ).
///
/// Координаты автоматически нормализуются (start<=end не обязателен).
pub fn extract_text(
    lines: &[String],
    area: &Rect,
    a_col: u16,
    a_row: u16,
    b_col: u16,
    b_row: u16,
) -> String {
    // Нормализуем (start может быть больше end если пользователь тянул вверх/влево)
    let (min_col, max_col) = if a_col <= b_col {
        (a_col, b_col)
    } else {
        (b_col, a_col)
    };
    let (min_row, max_row) = if a_row <= b_row {
        (a_row, b_row)
    } else {
        (b_row, a_row)
    };
    // Конвертируем абсолютные в локальные
    let (lcol0, lrow0) = to_local(area, min_col, min_row);
    let (lcol1, lrow1) = to_local(area, max_col, max_row);
    let mut out = String::new();
    for r in lrow0..=lrow1 {
        if (r as usize) >= lines.len() {
            break;
        }
        let line = &lines[r as usize];
        let row_chars: Vec<char> = line.chars().collect();
        let start = lcol0 as usize;
        // Последняя колонка — это позиция курсора, обычно на один символ меньше
        let end = (lcol1 as usize).min(row_chars.len());
        if start < end {
            let chunk: String = row_chars[start..end].iter().collect();
            out.push_str(&chunk);
        }
        // Перенос строки между строками (не после последней)
        if r < lrow1 {
            out.push('\n');
        }
    }
    out
}

/// Классифицировать мышиное событие.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    /// Клик ЛКМ (Up(MouseButton::Left) без перемещения).
    Click { col: u16, row: u16 },
    /// Двойной клик (детектируется вызовом по времени, здесь только сигнал).
    DoubleClick { col: u16, row: u16 },
    /// Начало drag ЛКМ.
    DragStart { col: u16, row: u16 },
    /// Продолжение drag ЛКМ.
    DragMove { col: u16, row: u16 },
    /// Конец drag ЛКМ — возвращает прямоугольник.
    DragEnd { col: u16, row: u16 },
    /// Скролл вверх.
    ScrollUp,
    /// Скролл вниз.
    ScrollDown,
    /// Правый клик (контекстное меню).
    RightClick { col: u16, row: u16 },
    /// Игнорировать (не наша мышь).
    Ignore,
}

/// Распарсить MouseEvent в MouseAction.
pub fn parse_event(ev: MouseEvent) -> MouseAction {
    match ev.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if ev.modifiers.contains(KeyModifiers::SHIFT) {
                // Shift+Click в raw-mode — игнорируем, чтобы терминал сделал нативное выделение
                MouseAction::Ignore
            } else {
                MouseAction::DragStart { col: ev.column, row: ev.row }
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if ev.modifiers.contains(KeyModifiers::SHIFT) {
                MouseAction::Ignore
            } else {
                MouseAction::DragMove { col: ev.column, row: ev.row }
            }
        }
        MouseEventKind::Up(MouseButton::Left) => {
            MouseAction::DragEnd { col: ev.column, row: ev.row }
        }
        MouseEventKind::Down(MouseButton::Right) => MouseAction::RightClick {
            col: ev.column,
            row: ev.row,
        },
        MouseEventKind::ScrollUp => MouseAction::ScrollUp,
        MouseEventKind::ScrollDown => MouseAction::ScrollDown,
        MouseEventKind::Down(MouseButton::Middle) => MouseAction::Ignore,
        _ => MouseAction::Ignore,
    }
}

/// Скопировать текст в системный буфер обмена через `arboard`.
pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    arboard::Clipboard::new()
        .and_then(|mut cb| cb.set_text(text.to_string()))
        .map_err(|e| format!("clipboard: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_lifecycle_start_extend_finish() {
        let mut s = SelectionRect::new();
        assert!(!s.active);
        s.start(10, 5);
        assert!(s.active);
        assert_eq!(s.bbox(), Some((10, 5, 10, 5)));
        s.extend(20, 15);
        // drag идёт от (10,5) до (20,15) — bbox нормализован
        assert_eq!(s.bbox(), Some((10, 5, 20, 15)));
        s.extend(5, 1);
        // drag продолжается — теперь end=5,1; bbox = (5,1,10,5)
        // (между (10,5) и (5,1) нормализованный bbox)
        assert_eq!(s.bbox(), Some((5, 1, 10, 5)));
        let r = s.finish().unwrap();
        assert_eq!(r, (5, 1, 10, 5));
        assert!(!s.active);
    }

    #[test]
    fn finish_no_active_returns_none() {
        let mut s = SelectionRect::new();
        assert_eq!(s.finish(), None);
    }

    #[test]
    fn finish_single_cell_returns_none() {
        let mut s = SelectionRect::new();
        s.start(5, 5);
        assert_eq!(s.finish(), None, "single cell is not a selection");
    }

    #[test]
    fn hit_inside_rect() {
        let r = Rect::new(10, 5, 30, 20);
        assert!(hit(&r, 10, 5));
        assert!(hit(&r, 39, 24));
        assert!(!hit(&r, 9, 5));
        assert!(!hit(&r, 40, 5));
        assert!(!hit(&r, 10, 25));
    }

    #[test]
    fn extract_single_line() {
        let lines = vec!["Hello, world!".to_string()];
        let area = Rect::new(0, 0, 80, 24);
        // Выделить "world"
        let text = extract_text(&lines, &area, 7, 0, 12, 0);
        assert_eq!(text, "world");
    }

    #[test]
    fn extract_multi_line() {
        let lines = vec![
            "Line one".to_string(),
            "Line two".to_string(),
            "Line three".to_string(),
        ];
        let area = Rect::new(0, 0, 80, 24);
        // с позиции (5,0) до (10,2) — нормализованный прямоугольник
        let text = extract_text(&lines, &area, 5, 0, 10, 2);
        // row0: "Line one"  cols[5..8] → "one"
        // row1: "Line two"  cols[5..8] → "two"
        // row2: "Line three" cols[5..10] → "three" (но в строке 10 символов, end=min(10,10)=10)
        assert_eq!(text, "one\ntwo\nthree");
    }

    #[test]
    fn extract_multi_line_reversed_drag() {
        // drag снизу-вверх: end выше start — extract_text должен нормализовать
        let lines = vec![
            "Line one".to_string(),
            "Line two".to_string(),
        ];
        let area = Rect::new(0, 0, 80, 24);
        // с позиции (5,1) до (8,0) — drag вверх
        let text = extract_text(&lines, &area, 5, 1, 8, 0);
        // должно дать то же что и с (5,0) до (8,1): row0[5..8]="one", row1[5..8]="two"
        assert_eq!(text, "one\ntwo");
    }

    #[test]
    fn extract_clipped_to_area() {
        let lines = vec!["abc".to_string()];
        // area начинается в (10,5), но выделение в (5,5) — за пределами
        let area = Rect::new(10, 5, 80, 24);
        let text = extract_text(&lines, &area, 5, 5, 9, 5);
        // После to_local получатся отрицательные (saturating_sub) → 0,0..0,0 → пусто
        assert_eq!(text, "");
    }

    #[test]
    fn extract_when_line_shorter_than_end() {
        let lines = vec!["hi".to_string()];
        let area = Rect::new(0, 0, 80, 24);
        // Выделить с col 0 до col 10 — но в строке только 2 символа
        let text = extract_text(&lines, &area, 0, 0, 10, 0);
        assert_eq!(text, "hi");
    }

    #[test]
    fn parse_left_click() {
        let ev = crossterm::event::MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 5,
            row: 10,
            modifiers: KeyModifiers::empty(),
        };
        match parse_event(ev) {
            MouseAction::DragEnd { col, row } => {
                assert_eq!(col, 5);
                assert_eq!(row, 10);
            }
            _ => panic!("expected DragEnd"),
        }
    }

    #[test]
    fn parse_shift_click_ignored() {
        let ev = crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 10,
            modifiers: KeyModifiers::SHIFT,
        };
        assert_eq!(parse_event(ev), MouseAction::Ignore);
    }

    #[test]
    fn parse_scroll() {
        let up = crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(parse_event(up), MouseAction::ScrollUp);
        let dn = crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        };
        assert_eq!(parse_event(dn), MouseAction::ScrollDown);
    }

    #[test]
    fn selection_clear_resets() {
        let mut s = SelectionRect::new();
        s.start(1, 1);
        s.extend(5, 5);
        s.clear();
        assert!(!s.active);
        assert_eq!(s.bbox(), None);
    }
}
