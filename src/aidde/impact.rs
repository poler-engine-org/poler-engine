//! Двунаправленный impact-анализ и Impact Passport.
//!
//! **Upstream** (кто зависит от меня): BFS по обратным рёбрам call graph —
//! все функции и файлы, которые сломаются при изменении символа.
//! **Downstream** (от кого завишу я): BFS по прямым рёбрам — все скрытые
//! зависимости самого символа.
//!
//! Выход — самодостаточный паспорт для AI-агента:

//! ```json
//! {
//!   "target_function": "engine::alloc_buffer",
//!   "file": "src/engine/allocator.rs",
//!   "lines": "120-145",
//!   "upstream_dependents": [{"caller": "pipeline::decode", "file": "...", "line": 45}],
//!   "downstream_dependencies": [{"callee": "memmap2::MmapMut", "file": "..."}],
//!   "side_effects": ["Блокировка мьютекса ALLOC_MUTEX"],
//!   "danger_level_if_modified": "CRITICAL (затронет 14 файлов)"
//! }
//! ```

use serde::Serialize;
use std::collections::HashSet;
use std::path::Path;

use crate::aidde::symbols::{last_segment_is, SymbolTable};
use crate::parser::{detect_lang, extract_enclosing_scope};

/// Зависимый (upstream): кто вызывает цель.
#[derive(Debug, Clone, Serialize)]
pub struct Dependent {
    pub caller: String,
    pub file: String,
    pub line: usize,
}

/// Зависимость (downstream): кого вызывает цель.
#[derive(Debug, Clone, Serialize)]
pub struct Dependency {
    pub callee: String,
    pub file: String,
}

/// Impact Passport символа.
#[derive(Debug, Clone, Serialize)]
pub struct ImpactReport {
    pub target_function: String,
    pub file: String,
    pub lines: String,
    pub upstream_dependents: Vec<Dependent>,
    pub downstream_dependencies: Vec<Dependency>,
    pub side_effects: Vec<String>,
    pub danger_level_if_modified: String,
}

/// Маркеры сайд-эффектов: регулярные источники скрытых зависимостей,
/// которые не видны ни grep, ни векторному RAG.
const SIDE_EFFECT_MARKERS: &[(&str, &str)] = &[
    ("unsafe", "Блок unsafe — снятые гарантии безопасности памяти"),
    ("Mutex", "Блокировка мьютекса"),
    (".lock()", "Блокировка мьютекса"),
    ("RwLock", "Блокировка чтения-записи"),
    ("static mut", "Мутация глобального состояния"),
    ("static ", "Доступ к глобальной статике"),
    ("lazy_static", "Инициализация глобального состояния"),
    ("GLOBAL", "Доступ к глобальному счётчику/состоянию"),
    ("RefCell", "Внутренняя мутабельность (RefCell)"),
    ("std::fs::", "Файловый I/O"),
    ("File::create", "Создание файла"),
    ("OpenOptions", "Открытие файла"),
    (".write(", "Запись в разделяемый ресурс"),
    ("spawn", "Порождение потока/задачи"),
    ("socket", "Сетевой сокет"),
    ("TcpStream", "Сетевое соединение"),
    ("UdpSocket", "Сетевой сокет"),
    ("connect", "Сетевое соединение"),
    ("println!", "Вывод в stdout"),
    ("eprintln!", "Вывод в stderr"),
    ("panic!", "Паника"),
    ("process::exit", "Завершение процесса"),
];

fn scan_side_effects(body: &str) -> Vec<String> {
    let mut out: Vec<String> = SIDE_EFFECT_MARKERS
        .iter()
        .filter(|(m, _)| body.contains(m))
        .map(|(_, d)| d.to_string())
        .collect();
    out.dedup();
    out
}

fn stem_of(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// Строит Impact Passport для символа.
///
/// * `depth` — глубина BFS в обе стороны (2–3 уровня абстракции);
/// * `max_items` — потолок списков зависимостей.
pub fn impact_analysis(
    table: &SymbolTable,
    target: &str,
    depth: usize,
    max_items: usize,
) -> Option<ImpactReport> {
    let def = table.resolve(target).first().cloned()?;

    // Тело определения (enclosing scope) для строк и сайд-эффектов.
    let text = std::fs::read_to_string(&def.file).ok()?;
    let lang = detect_lang(Path::new(&def.file));
    let scope = extract_enclosing_scope(&text, def.byte, lang);
    let lines = format!("{}-{}", scope.start_line, scope.end_line);

    // ---------- Upstream: кто зависит от меня (обратные рёбра) ----------
    let mut upstream: Vec<Dependent> = Vec::new();
    let mut seen_calls: HashSet<(String, String, usize)> = HashSet::new();
    let mut level: HashSet<String> = HashSet::from([last_segment_is(target).to_string()]);
    let mut visited: HashSet<String> = level.clone();

    for _ in 0..depth.max(1) {
        let mut next: HashSet<String> = HashSet::new();
        for cs in &table.calls {
            if !level.iter().any(|l| l == last_segment_is(&cs.callee)) {
                continue;
            }
            let key = (cs.caller.clone(), cs.file.clone(), cs.line);
            if seen_calls.insert(key) {
                upstream.push(Dependent {
                    caller: cs.caller.clone(),
                    file: cs.file.clone(),
                    line: cs.line,
                });
            }
            next.insert(last_segment_is(&cs.caller).to_string());
        }
        next.retain(|n| !visited.contains(n));
        if next.is_empty() || upstream.len() >= max_items {
            break;
        }
        visited.extend(next.iter().cloned());
        level = next;
    }
    upstream.truncate(max_items);

    // ---------- Downstream: от кого завишу я (прямые рёбра) ----------
    let mut downstream: Vec<Dependency> = Vec::new();
    let mut seen_dep: HashSet<String> = HashSet::new();
    let mut level: HashSet<String> = HashSet::from([last_segment_is(target).to_string()]);
    let mut visited: HashSet<String> = level.clone();

    for _ in 0..depth.max(1) {
        let mut next: HashSet<String> = HashSet::new();
        for cs in &table.calls {
            if !level.iter().any(|l| l == last_segment_is(&cs.caller)) {
                continue;
            }
            if seen_dep.insert(cs.callee.clone()) {
                let file = table
                    .resolve(&cs.callee)
                    .first()
                    .map(|d| d.file.clone())
                    .unwrap_or_else(|| cs.file.clone());
                downstream.push(Dependency {
                    callee: cs.callee.clone(),
                    file,
                });
            }
            next.insert(last_segment_is(&cs.callee).to_string());
        }
        next.retain(|n| !visited.contains(n));
        if next.is_empty() || downstream.len() >= max_items {
            break;
        }
        visited.extend(next.iter().cloned());
        level = next;
    }
    downstream.truncate(max_items);

    // ---------- Сайд-эффекты и danger level ----------
    let side_effects = scan_side_effects(&scope.text);
    let files: HashSet<&String> = upstream.iter().map(|d| &d.file).collect();
    let n = files.len();
    let danger = match n {
        0 => "LOW (прямых зависимых не найдено)".to_string(),
        1..=3 => format!("MEDIUM (затронет {n} файл)"),
        4..=10 => format!("HIGH (затронет {n} файлов)"),
        _ => format!("CRITICAL (затронет {n} файлов)"),
    };

    Some(ImpactReport {
        target_function: format!("{}::{}", stem_of(&def.file), def.symbol),
        file: def.file.clone(),
        lines,
        upstream_dependents: upstream,
        downstream_dependencies: downstream,
        side_effects,
        danger_level_if_modified: danger,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_effects_detected() {
        let body = "unsafe { ALLOC_MUTEX.lock() }\nGLOBAL_GAUGE += 1;\nstd::fs::write(p, b)?;\n";
        let fx = scan_side_effects(body);
        assert!(fx.iter().any(|s| s.contains("unsafe")));
        assert!(fx.iter().any(|s| s.contains("мьютекса")));
        assert!(fx.iter().any(|s| s.contains("глобальному")));
        assert!(fx.iter().any(|s| s.contains("файлового") || s.contains("Файловый")));
    }

    #[test]
    fn clean_body_no_effects() {
        assert!(scan_side_effects("let x = a + b;").is_empty());
    }
}
