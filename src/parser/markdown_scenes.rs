//! Выделение семантически целостных сцен из markdown/текста.
//!
//! Решение проблемы Chunk Fragmentation: границы enclosing scope
//! привязаны к структуре документа (заголовки сцен/глав), а не к
//! нарезке по N токенов. Метаданные сцены (временная метрика,
//! локация, субъекты) парсятся из строк-карточек над совпадением
//! и попадают в ContextAnchor без потери квантификаторов.

use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::parser::ast_code::CodeScope;

/// Контекст сцены, в которой найдено совпадение.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SceneContext {
    /// Глава / раздел (из `#`-заголовка либо имени файла).
    pub chapter: String,
    /// Временная метрика в формате контракта: `Метрика: Т-23`.
    pub temporal_metric: Option<String>,
    /// Локация в формате контракта: `Локация: Разлом Каньона`.
    pub location: Option<String>,
    /// Субъекты сцены (метки контракта).
    pub subjects: Vec<String>,
    /// Полный текст законченной сцены (enclosing scope).
    pub enclosing_scope: String,
    /// Распознанный тег метрики (`Т-23`) — для temporal-фильтрации. В JSON не попадает.
    #[serde(skip)]
    pub metric_tag: Option<String>,
    /// Распарсенные имена субъектов и их алиасы — для графа сущностей.
    #[serde(skip)]
    pub subject_names: Vec<String>,
    /// Пары (алиас, базовое имя) из одной скобочной группы.
    #[serde(skip)]
    pub subject_pairs: Vec<(String, String)>,
}

/// Обрезает строку до `max_bytes` без разрыва UTF-8 символа.
pub fn truncate_char_safe(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

fn strip_inline_md(s: &str) -> String {
    s.trim()
        .trim_matches(|c: char| c == '*' || c == '`' || c == '#' || c == '-' || c == '>')
        .trim()
        .to_string()
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Границы сцены (лёгкая структура — вычисляется на каждое совпадение).
#[derive(Debug, Clone)]
pub struct SceneBounds {
    /// Глава / раздел.
    pub chapter: String,
    /// Байтовое начало сцены.
    pub start: usize,
    /// Байтовый конец сцены (не включительно).
    pub end: usize,
    /// true — сцена ограничена заголовками markdown; false — txt-абзац.
    pub structured: bool,
}

/// Заголовок markdown: уровень (#..######) и чистый текст.
fn parse_heading(line: &str) -> Option<(usize, String)> {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return None;
    }
    let hashes = t.chars().take_while(|c| *c == '#').count();
    if hashes > 6 {
        return None;
    }
    let rest = t[hashes..].trim();
    if rest.is_empty() {
        return None;
    }
    Some((hashes, strip_inline_md(rest)))
}

impl SceneContext {
    /// Лёгкая локализация сцены для байтового смещения (без клонирования
    /// текста) — вызывается на каждое совпадение.
    pub fn locate(text: &str, byte_offset: usize, file_path: &Path) -> SceneBounds {
        let off = byte_offset.min(text.len());

        let mut headings: Vec<(usize, usize, usize, String)> = Vec::new();
        let mut pos = 0usize;
        for line in text.lines() {
            if let Some((lvl, h)) = parse_heading(line) {
                headings.push((pos, pos + line.len(), lvl, h));
            }
            pos += line.len() + 1;
        }

        // txt без заголовков: абзац как enclosing scope
        if headings.is_empty() {
            let start = text[..off].rfind("\n\n").map(|i| i + 2).unwrap_or(0);
            let end = text[off..].find("\n\n").map(|i| off + i).unwrap_or(text.len());
            return SceneBounds {
                chapter: file_stem(file_path),
                start,
                end,
                structured: false,
            };
        }

        let above: Vec<&(usize, usize, usize, String)> =
            headings.iter().filter(|h| h.0 <= off).collect();
        let (scene_start, scene_level) = match above.last() {
            Some(h) => (h.0, h.2),
            None => (0, 7), // выше любого уровня
        };
        // Сцена — семантически целостный блок: заканчивается на первом
        // заголовке ПОСЛЕ совпадения (любого уровня); если такового нет —
        // на границе родительского раздела (уровень <= scene_level) или EOF.
        let scene_end = headings
            .iter()
            .filter(|h| h.0 > off)
            .map(|h| h.0)
            .min()
            .or_else(|| {
                headings
                    .iter()
                    .filter(|h| h.0 > scene_start && h.2 <= scene_level)
                    .map(|h| h.0)
                    .min()
            })
            .unwrap_or(text.len());

        let chapter = headings
            .iter()
            .rfind(|h| h.0 <= off && h.2 == 1)
            .or_else(|| headings.first())
            .map(|h| h.3.clone())
            .unwrap_or_else(|| file_stem(file_path));

        SceneBounds {
            chapter,
            start: scene_start,
            end: scene_end,
            structured: true,
        }
    }

    /// Полное построение SceneContext по границам — один раз на уникальную
    /// сцену (кэшируется вызывающим кодом).
    pub fn build(text: &str, bounds: &SceneBounds, _file_path: &Path) -> SceneContext {
        let start = bounds.start.min(text.len());
        let end = bounds.end.min(text.len()).max(start);
        let scope_raw = &text[start..end];

        let mut temporal_metric = None;
        let mut metric_tag = None;
        let mut location = None;
        let mut subjects: Vec<String> = Vec::new();
        let mut subject_names: Vec<String> = Vec::new();
        let mut subject_pairs: Vec<(String, String)> = Vec::new();

        if bounds.structured {
            // метаданные — карточка сцены: первые 60 строк от начала сцены
            for line in scope_raw.lines().take(60) {
                let lower = line.to_lowercase();
                let cleaned = strip_inline_md(line);
                if temporal_metric.is_none() && lower.contains("метрика:") {
                    let value = after_colon(&cleaned);
                    if !value.is_empty() {
                        metric_tag = extract_metric_tag(&value);
                        temporal_metric = Some(format!("Метрика: {value}"));
                    }
                }
                if location.is_none()
                    && (lower.contains("локация:") || lower.contains("место:"))
                {
                    let value = after_colon(&cleaned);
                    if !value.is_empty() {
                        location = Some(format!("Локация: {value}"));
                    }
                }
                if subjects.is_empty()
                    && (lower.contains("субъекты:")
                        || lower.contains("персонажи:")
                        || lower.contains("герои:")
                        || lower.contains("действующие лица:"))
                {
                    let value = after_colon(&cleaned);
                    if !value.is_empty() {
                        subjects.push(format!("Субъекты: {value}"));
                        (subject_names, subject_pairs) = parse_subjects(&value);
                    }
                }
                if temporal_metric.is_some() && location.is_some() && !subjects.is_empty() {
                    break; // карточка прочитана целиком
                }
            }
        } else {
            // txt-фолбэк: метаданные ищутся строками выше абзаца (макс. 50)
            let mut lines_above: Vec<&str> = Vec::new();
            for line in text[..start].lines().rev().take(50) {
                lines_above.push(line);
            }
            for line in lines_above {
                let lower = line.to_lowercase();
                let cleaned = strip_inline_md(line);
                if temporal_metric.is_none() && lower.contains("метрика:") {
                    let value = after_colon(&cleaned);
                    if !value.is_empty() {
                        metric_tag = extract_metric_tag(&value);
                        temporal_metric = Some(format!("Метрика: {value}"));
                    }
                }
                if location.is_none()
                    && (lower.contains("локация:") || lower.contains("место:"))
                {
                    let value = after_colon(&cleaned);
                    if !value.is_empty() {
                        location = Some(format!("Локация: {value}"));
                    }
                }
                if subjects.is_empty()
                    && (lower.contains("субъекты:")
                        || lower.contains("персонажи:")
                        || lower.contains("герои:"))
                {
                    let value = after_colon(&cleaned);
                    if !value.is_empty() {
                        subjects.push(format!("Субъекты: {value}"));
                        (subject_names, subject_pairs) = parse_subjects(&value);
                    }
                }
            }
        }

        SceneContext {
            chapter: bounds.chapter.clone(),
            temporal_metric,
            location,
            subjects,
            enclosing_scope: scope_raw.trim().to_string(),
            metric_tag,
            subject_names,
            subject_pairs,
        }
    }

    /// Совместимый API: локализация + построение за один вызов.
    pub fn extract_from_text(text: &str, byte_offset: usize, file_path: &Path) -> Self {
        let bounds = Self::locate(text, byte_offset, file_path);
        Self::build(text, &bounds, file_path)
    }

    /// Строит сцену из кодового скоупа (файлы исходников).
    pub fn from_code_scope(scope: &CodeScope, file_path: &Path, max_scope_bytes: usize) -> Self {
        let sig = match (&scope.name, scope.scope_type.as_str()) {
            (Some(n), ty) => format!("{} :: {} (L{}–L{})", file_stem(file_path), n, scope.start_line, scope.end_line).replace(" :: ", &format!(" [{ty}] :: ")),
            (None, ty) => format!("{} [{}] (L{}–L{})", file_stem(file_path), ty, scope.start_line, scope.end_line),
        };
        SceneContext {
            chapter: sig,
            temporal_metric: None,
            location: None,
            subjects: Vec::new(),
            enclosing_scope: truncate_char_safe(&scope.text, max_scope_bytes),
            metric_tag: None,
            subject_names: Vec::new(),
            subject_pairs: Vec::new(),
        }
    }
}

fn after_colon(s: &str) -> String {
    match s.find(':') {
        Some(i) => strip_inline_md(&s[i + 1..]),
        None => String::new(),
    }
}

/// `Т-23` / `T-23` -> нормализованный `Т-23`.
fn extract_metric_tag(value: &str) -> Option<String> {
    let mut i = 0usize;
    while i < value.len() {
        let c = value[i..].chars().next()?;
        let clen = c.len_utf8();
        if matches!(c, 'Т' | 'т' | 'T' | 't') && value[i + clen..].starts_with('-') {
            let digits: String = value[i + clen + 1..]
                .chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            if !digits.is_empty() {
                return Some(format!("Т-{digits}"));
            }
        }
        i += clen;
    }
    None
}

/// `Мальчик (гибрид), Соболь (Нокс)` -> (плоский список имён,
/// пары алиас -> базовое имя из той же части).
fn parse_subjects(value: &str) -> (Vec<String>, Vec<(String, String)>) {
    let mut names = Vec::new();
    let mut pairs = Vec::new();
    for part in value.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        if p.ends_with(')') {
            if let Some(open) = p.find('(') {
                let base = p[..open].trim();
                let alias = p[open + 1..p.len() - 1].trim();
                if !base.is_empty() {
                    names.push(base.to_string());
                }
                if !alias.is_empty() {
                    names.push(alias.to_string());
                    if !base.is_empty() {
                        pairs.push((alias.to_string(), base.to_string()));
                    }
                }
                continue;
            }
        }
        names.push(p.to_string());
    }
    (names, pairs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CH: &str = "# Глава 36. Инертный\n\n**Метрика: Т-23**\n**Локация: Разлом Каньона**\n**Субъекты: Мальчик (гибрид), Соболь (Нокс)**\n\nНокс вонзила когти в солнечное сплетение противника. Шунт сбрасывает тепло.\n";

    #[test]
    fn chapter_and_metadata_extracted() {
        let off = CH.find("вонзила").unwrap();
        let sc = SceneContext::extract_from_text(CH, off, Path::new("/book/chapter_36.md"));
        assert_eq!(sc.chapter, "Глава 36. Инертный");
        assert_eq!(sc.temporal_metric.as_deref(), Some("Метрика: Т-23"));
        assert_eq!(sc.location.as_deref(), Some("Локация: Разлом Каньона"));
        assert_eq!(sc.subjects.len(), 1);
        assert!(sc.subjects[0].contains("Соболь (Нокс)"));
        assert_eq!(sc.metric_tag.as_deref(), Some("Т-23"));
        assert!(sc.enclosing_scope.contains("Шунт"));
    }

    #[test]
    fn subject_names_parsed_with_aliases() {
        let off = CH.find("вонзила").unwrap();
        let sc = SceneContext::extract_from_text(CH, off, Path::new("/book/chapter_36.md"));
        assert!(sc.subject_names.contains(&"Мальчик".to_string()));
        assert!(sc.subject_names.contains(&"Нокс".to_string()));
        assert!(sc.subject_names.contains(&"Соболь".to_string()));
        assert!(sc.subject_names.contains(&"гибрид".to_string()));
    }

    #[test]
    fn scene_stops_at_next_heading() {
        let text = "# Глава 1\n\nТекст сцены один с keyword.\n\n## Подсцена\n\nДругой текст.\n\n# Глава 2\n\nЕщё текст.\n";
        let off = text.find("keyword").unwrap();
        let sc = SceneContext::extract_from_text(text, off, Path::new("/b/a.md"));
        assert!(sc.enclosing_scope.contains("сцены один"));
        assert!(!sc.enclosing_scope.contains("Другой текст"));
    }

    #[test]
    fn subsection_hit_uses_subsection_scope_but_chapter_heading() {
        let text = "# Глава 1\n\nВступление.\n\n## Подсцена\n\nНокс действует.\n";
        let off = text.find("Нокс").unwrap();
        let sc = SceneContext::extract_from_text(text, off, Path::new("/b/a.md"));
        assert_eq!(sc.chapter, "Глава 1");
        assert!(sc.enclosing_scope.contains("Подсцена"));
        assert!(!sc.enclosing_scope.contains("Вступление"));
    }

    #[test]
    fn txt_falls_back_to_paragraph() {
        let text = "Первый абзац.\n\nВторой абзац с Нокс внутри.\n\nТретий абзац.\n";
        let off = text.find("Нокс").unwrap();
        let sc = SceneContext::extract_from_text(text, off, Path::new("/b/plain.txt"));
        assert_eq!(sc.enclosing_scope, "Второй абзац с Нокс внутри.");
        assert_eq!(sc.chapter, "plain");
    }

    #[test]
    fn markdown_bold_is_stripped() {
        let text = "# Глава\n\n**Метрика: Т-7**\n\nКлючевое слово здесь.\n";
        let off = text.find("Ключевое").unwrap();
        let sc = SceneContext::extract_from_text(text, off, Path::new("/b/x.md"));
        assert_eq!(sc.temporal_metric.as_deref(), Some("Метрика: Т-7"));
        assert_eq!(sc.metric_tag.as_deref(), Some("Т-7"));
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "кириллица".repeat(100);
        let t = truncate_char_safe(&s, 25); // 12.5 символов кириллицы
        assert!(t.len() <= 25 + 3);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn metric_tag_variants() {
        assert_eq!(extract_metric_tag("Т-23"), Some("Т-23".to_string()));
        assert_eq!(extract_metric_tag("t-5 хвост"), Some("Т-5".to_string()));
        assert_eq!(extract_metric_tag("нет метрики"), None);
    }
}
