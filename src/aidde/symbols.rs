//! Глобальная таблица символов и call graph проекта.
//!
//! Лексический уровень (без полного парсера): определения извлекаются
//! построчной сигнатурной эвристикой, вызовы — паттерном `ident(`,
//! скоуп вызова определяется лексическим сканером [`crate::parser::ast_code`]
//! (строки/raw-строки/комментарии корректно пропускаются).

use rayon::prelude::*;
use regex::Regex;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;

use crate::parser::ast_code;
use crate::parser::triples::CALL_KEYWORDS;
use crate::parser::{detect_lang, CodeLang};

/// Определение символа (функция/структура/класс/…).
#[derive(Debug, Clone, Serialize)]
pub struct Definition {
    /// Имя символа (`validate`).
    pub symbol: String,
    /// Вид определения (`fn`, `struct`, `class`, `def`, …).
    pub kind: String,
    /// Файл определения.
    pub file: String,
    /// Строка определения (1-based).
    pub line: usize,
    /// Байтовое смещение начала сигнатуры.
    pub byte: usize,
}

/// Вызов: `caller` вызывает `callee` в файле `file` на строке `line`.
#[derive(Debug, Clone, Serialize)]
pub struct CallSite {
    pub caller: String,
    pub callee: String,
    pub file: String,
    pub line: usize,
}

/// Импорт модуля в файле.
#[derive(Debug, Clone, Serialize)]
pub struct ImportStmt {
    pub file: String,
    pub module: String,
}

static CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap());

static IMPORT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^\s*(?:pub(?:\s+\([^)]*\))?\s+)?use\s+([A-Za-z0-9_:]+)|^\s*from\s+([\w.]+)\s+import|^\s*import\s+([\w.]+)|^\s*#\s*include\s+[<"]([^>"]+)[>"]|^\s*import\s+[^;\n]*?from\s+['"]([^'"]+)['"]|=\s*require\(\s*['"]([^'"]+)['"]"#,
    )
    .unwrap()
});

fn line_of_byte(text: &str, byte: usize) -> usize {
    text[..byte.min(text.len())].matches('\n').count() + 1
}

fn import_of(line: &str) -> Option<String> {
    let caps = IMPORT_RE.captures(line)?;
    for g in 1..=6 {
        if let Some(m) = caps.get(g) {
            let s = m.as_str().trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

/// Последний сегмент qualified-имени (`a::b::c` / `a.b.c` -> `c`).
pub fn last_segment_is(s: &str) -> &str {
    last_segment(s)
}

fn last_segment(s: &str) -> &str {
    s.rsplit("::")
        .next()
        .unwrap_or(s)
        .rsplit('.')
        .next()
        .unwrap_or(s)
}

/// Глобальная таблица символов проекта.
pub struct SymbolTable {
    /// Все определения.
    pub defs: Vec<Definition>,
    /// Все вызовы (call graph).
    pub calls: Vec<CallSite>,
    /// Все импорты.
    pub imports: Vec<ImportStmt>,
    by_name: HashMap<String, Vec<usize>>,
}

impl SymbolTable {
    /// Сканирует кодовые файлы (Rust/Python/C/JS/…) и строит таблицу.
    ///
    /// v0.6: параллельная сборка (rayon par_iter) — файлы независимы,
    /// результаты сливаются в порядке исходного списка (детерминизм).
    pub fn build(files: &[PathBuf], max_file_bytes: u64) -> Self {
        let per_file: Vec<(Vec<Definition>, Vec<CallSite>, Vec<ImportStmt>)> = files
            .par_iter()
            .map(|path| scan_one_file(path, max_file_bytes))
            .collect();

        let mut defs: Vec<Definition> = Vec::new();
        let mut calls: Vec<CallSite> = Vec::new();
        let mut imports: Vec<ImportStmt> = Vec::new();
        for (d, c, i) in per_file {
            defs.extend(d);
            calls.extend(c);
            imports.extend(i);
        }
        Self::from_parts(defs, calls, imports)
    }

    /// Сборка таблицы из готовых частей (используется SQLite-бэкендом).
    pub fn from_parts(
        defs: Vec<Definition>,
        calls: Vec<CallSite>,
        imports: Vec<ImportStmt>,
    ) -> Self {
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, d) in defs.iter().enumerate() {
            by_name.entry(d.symbol.clone()).or_default().push(i);
        }
        Self {
            defs,
            calls,
            imports,
            by_name,
        }
    }
}

/// Разбор одного файла в (defs, calls, imports).
pub(crate) fn scan_one_file(
    path: &PathBuf,
    max_file_bytes: u64,
) -> (Vec<Definition>, Vec<CallSite>, Vec<ImportStmt>) {
    let mut defs: Vec<Definition> = Vec::new();
    let mut calls: Vec<CallSite> = Vec::new();
    let mut imports: Vec<ImportStmt> = Vec::new();

    if detect_lang(path) == CodeLang::Plain {
        return (defs, calls, imports);
    }
    let Ok(meta) = std::fs::metadata(path) else {
        return (defs, calls, imports);
    };
    if meta.len() > max_file_bytes {
        return (defs, calls, imports);
    }
    let Ok(text) = std::fs::read_to_string(path) else {
        return (defs, calls, imports);
    };
    let lang = detect_lang(path);
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    #[allow(unused_variables)]
    let file_s = path.to_string_lossy().to_string();

let file_s = path.to_string_lossy().to_string();

    // --- определения + импорты (построчно) ---
    let mut off = 0usize;
    for line in text.lines() {
        if let Some((kind, name)) = ast_code::signature_of(line) {
            defs.push(Definition {
                symbol: name,
                kind,
                file: file_s.clone(),
                line: line_of_byte(&text, off),
                byte: off,
            });
        }
        if let Some(module) = import_of(line) {
            imports.push(ImportStmt {
                file: file_s.clone(),
                module,
            });
        }
        off += line.len() + 1;
    }

    // --- вызовы (call graph) ---
    // Спаны генерируются последовательно => уже отсортированы по
    // началу. Бинарный поиск: O(log K) на вызов вместо O(K).
    // (v0.6: на Wireshark-файлах с 5000+ спанов убирает O(M×N).)
    let noise_spans = ast_code::string_comment_spans(&text);
    let in_noise = |off: usize| -> bool {
        match noise_spans.binary_search_by(|(s, e)| {
            if off < *s {
                std::cmp::Ordering::Greater
            } else if off >= *e {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        }) {
            Ok(_) => true,
            // Err(insert_pos): спан слева от insert_pos может
            // накрывать off (перекрывающихся спанов нет — они
            // дизъюнктны по построению)
            Err(0) => false,
            Err(ip) => {
                let (s, e) = noise_spans[ip - 1];
                off >= s && off < e
            }
        }
    };

    for caps in CALL_RE.captures_iter(&text) {
        let callee = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        if callee.is_empty() || CALL_KEYWORDS.contains(&callee.as_str()) {
            continue;
        }
        let call_off = caps.get(0).map(|m| m.start()).unwrap_or(0);

        // вызовы внутри строковых литералов и комментариев — шум
        if in_noise(call_off) {
            continue;
        }

        let scope = ast_code::locate_scope(&text, call_off, lang);

        // Определение вне любого блока (например, `fn inner() {}`
        // на верхнем уровне): сигнатура строки совпадает с callee.
        if scope.is_none() {
            let line_sig = ast_code::light_signature(&text, call_off);
            if line_sig.as_deref() == Some(callee.as_str()) {
                continue;
            }
        }

        let (scope_b, scope_name) = match scope {
            Some((b, _)) => (b, ast_code::light_signature(&text, b)),
            None => (0, None),
        };
        let caller = match &scope_name {
            Some(n) => format!("{stem}::{n}"),
            None => stem.clone(),
        };

        // Совпадение в строке сигнатуры — это определение, не вызов:
        // пропуск, если имя совпадает со скоупом и смещение до '{'
        // (brace-языки) / в пределах строки заголовка (Python).
        if let Some(name) = &scope_name {
            if *name == callee {
                let header_end = match lang {
                    CodeLang::Brace => text[scope_b..]
                        .find('{')
                        .map(|i| scope_b + i)
                        .unwrap_or(scope_b),
                    _ => text[scope_b..]
                        .find('\n')
                        .map(|i| scope_b + i)
                        .unwrap_or(text.len()),
                };
                if call_off <= header_end {
                    continue;
                }
            }
        }

        calls.push(CallSite {
            caller,
            callee,
            file: file_s.clone(),
            line: line_of_byte(&text, call_off),
        });
    }

    (defs, calls, imports)
}

impl SymbolTable {
    /// Разрешает имя символа (с поддержкой qualified `a::b::c`).
    pub fn resolve(&self, name: &str) -> Vec<&Definition> {
        let key = last_segment(name);
        self.by_name
            .get(key)
            .map(|idxs| idxs.iter().map(|&i| &self.defs[i]).collect())
            .unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn write(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn definitions_and_calls_extracted() {
        let dir = tempfile::TempDir::new().unwrap();
        write(
            dir.path(),
            "core.rs",
            "pub fn core_fn(x: i32) -> i32 {\n    x + 1\n}\n",
        );
        write(dir.path(), "mid.rs", "pub fn mid() {\n    core_fn(1);\n}\n");
        let files = vec![dir.path().join("core.rs"), dir.path().join("mid.rs")];
        let t = SymbolTable::build(&files, 1024 * 1024);
        assert_eq!(t.defs.len(), 2);
        assert_eq!(t.resolve("core_fn").len(), 1);
        assert_eq!(t.resolve("mid").len(), 1);
        // определение не считается вызовом самого себя
        let self_calls: Vec<&CallSite> = t
            .calls
            .iter()
            .filter(|c| c.caller.ends_with("core_fn"))
            .collect();
        assert!(self_calls.is_empty(), "{:?}", t.calls);
        // реальный вызов зарегистрирован
        assert!(
            t.calls
                .iter()
                .any(|c| c.callee == "core_fn" && c.caller == "mid::mid"),
            "{:?}",
            t.calls
        );
    }

    #[test]
    fn definition_in_signature_not_a_call() {
        let dir = tempfile::TempDir::new().unwrap();
        write(
            dir.path(),
            "a.rs",
            "fn outer() {\n    inner();\n}\nfn inner() {}\n",
        );
        let t = SymbolTable::build(&[dir.path().join("a.rs")], 1024 * 1024);
        // inner() на строке 1 после '{' — реальный вызов
        assert!(t
            .calls
            .iter()
            .any(|c| c.callee == "inner" && c.caller == "a::outer"));
        // определение inner (строка 3) не порождает вызова
        assert_eq!(t.calls.iter().filter(|c| c.callee == "inner").count(), 1);
    }

    #[test]
    fn python_definitions_and_imports() {
        let dir = tempfile::TempDir::new().unwrap();
        write(
            dir.path(),
            "svc.py",
            "import json\n\n\nclass Worker:\n    def run(self):\n        return self.load()\n\n    def load(self):\n        return 1\n",
        );
        let t = SymbolTable::build(&[dir.path().join("svc.py")], 1024 * 1024);
        assert_eq!(t.resolve("Worker").len(), 1);
        assert_eq!(t.resolve("run").len(), 1);
        assert!(t.imports.iter().any(|i| i.module == "json"));
        assert!(t
            .calls
            .iter()
            .any(|c| c.callee == "load" && c.caller == "svc::run"));
    }

    #[test]
    fn calls_inside_strings_ignored() {
        let dir = tempfile::TempDir::new().unwrap();
        write(
            dir.path(),
            "s.rs",
            "fn f() {\n    let s = \"not_a_call(1)\";\n    real();\n}\n",
        );
        let t = SymbolTable::build(&[dir.path().join("s.rs")], 1024 * 1024);
        assert!(t.calls.iter().any(|c| c.callee == "real"));
        assert!(!t.calls.iter().any(|c| c.callee == "not_a_call"));
    }
}
