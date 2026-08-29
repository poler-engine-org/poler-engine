//! JSONL audit-trail внешних API-действий аккаунта (v0.17.5).
//!
//! Каждое обращение движка к облачным сервисам Google (Gmail, Drive,
//! NotebookLM, GCP) и каждое действие с куками профиля фиксируется
//! одной строкой JSON в `~/.config/poler-engine/audit.log`:
//!
//! ```json
//! {"ts":"2026-08-29T12:00:00Z","action":"nlm.create_note","details":"nb=abc-123 title=\"Мысль\""}
//! ```
//!
//! Свойства:
//! * **Best-effort** — ошибка записи лога НИКОГДА не ломает основную
//!   операцию (только eprintln в verbose-режиме).
//! * **0600** — файл доступен только владельцу (в нём метаданные
//!   активности аккаунта: какие ноутбуки/письма читались).
//! * **Без контента** — в details только идентификаторы и счётчики;
//! * полный текст заметок/писем в лог не пишется.
//! * Отключение: `POLER_AUDIT_LOG=off`. Своё место: `POLER_AUDIT_LOG=/path`.
//!
//! Отвязка тестов от домашней директории: функции принимают явный путь,
//! а обёртка [`record`] ходит в дефолтный.

use std::io::Write;
use std::path::PathBuf;

use super::config_dir;

/// Действие, не подлежащее логированию (env `POLER_AUDIT_LOG=off`).
pub const AUDIT_OFF: &str = "off";

/// Путь к audit-логу: `$POLER_AUDIT_LOG` → конфиг/audit.log.
pub fn audit_log_path() -> PathBuf {
    if let Ok(p) = std::env::var("POLER_AUDIT_LOG") {
        return PathBuf::from(p);
    }
    config_dir().join("audit.log")
}

/// Логирование выключено env-ом?
pub fn audit_disabled() -> bool {
    std::env::var("POLER_AUDIT_LOG").map(|v| v.eq_ignore_ascii_case(AUDIT_OFF)).unwrap_or(false)
}

/// ISO-8601 UTC-метка времени без внешних зависимостей
/// (civil-from-days по Howard Hinnant, точность — секунды).
pub fn iso_utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil-from-days: 1970-01-01 → эпоха Юлианского календаря
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mth = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if mth <= 2 { y + 1 } else { y };
    format!("{y:04}-{mth:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Записать одну строку JSONL по ЯВНОМУ пути (для тестов).
pub fn record_at(path: &std::path::Path, action: &str, details: &str) -> Result<(), String> {
    let line = serde_json::json!({
        "ts": iso_utc_now(),
        "action": action,
        "details": details,
    });
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("audit dir {}: {e}", dir.display()))?;
    }
    let is_new = !path.exists();
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("audit open {}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("audit write: {e}"))?;
    if is_new {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(
                path,
                std::fs::Permissions::from_mode(0o600),
            );
        }
    }
    Ok(())
}

/// Записать событие в дефолтный audit-лог (best-effort, без паники).
///
/// Вызывается из всех точек работы с аккаунтом: чтения Gmail/Drive,
/// операции NotebookLM, OAuth-обмены, перенос куков профиля.
pub fn record(action: &str, details: &str) {
    if audit_disabled() {
        return;
    }
    if let Err(e) = record_at(&audit_log_path(), action, details) {
        // аудит не должен ломать основную операцию — только тихо ругнуться
        eprintln!("poler-audit: {e}");
    }
}

/// Усечь строку до N символов для details (защита от раздувания лога).
pub fn clip(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_utc_now_format() {
        let ts = iso_utc_now();
        // 2026-08-29T12:34:56Z — строгий формат, 20 символов
        assert_eq!(ts.len(), 20, "длина ISO-метки: {ts}");
        assert!(ts.ends_with('Z'));
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[7..8], "-");
        assert_eq!(&ts[10..11], "T");
        assert_eq!(&ts[13..14], ":");
        // год в разумных пределах (эпоха → 2xxx)
        let year: i64 = ts[0..4].parse().unwrap();
        assert!(1970 <= year && year <= 2100);
    }

    #[test]
    fn record_at_writes_valid_jsonl_with_0600() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("audit.log");
        record_at(&p, "test.read", "nb=1 count=3").unwrap();
        record_at(&p, "test.write", "nb=1 note=\"x\"").unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "две строки JSONL");
        for l in &lines {
            let v: serde_json::Value = serde_json::from_str(l).unwrap();
            assert!(v["ts"].is_string());
            assert!(v["action"].is_string());
            assert!(v["details"].is_string());
        }
        assert!(lines[0].contains("test.read"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "audit.log только для владельца");
        }
    }

    #[test]
    fn record_at_creates_nested_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a/b/c/audit.log");
        record_at(&p, "x.y", "z").unwrap();
        assert!(p.exists());
    }

    #[test]
    fn clip_truncates_long_details() {
        let long = "а".repeat(100);
        assert_eq!(clip(&long, 10).chars().count(), 10);
        assert_eq!(clip("коротко", 10), "коротко");
    }

    #[test]
    fn audit_disabled_respects_env() {
        // Тест мутации env в однопоточном режиме: сохраняем и восстанавливаем.
        // (tests идут параллельно — используем уникальное значение, чтобы
        //  не пересечься с audit_log_path-тестом)
        let saved = std::env::var("POLER_AUDIT_LOG");
        std::env::set_var("POLER_AUDIT_LOG", "off");
        assert!(audit_disabled());
        std::env::remove_var("POLER_AUDIT_LOG");
        assert!(!audit_disabled());
        if let Ok(v) = saved {
            std::env::set_var("POLER_AUDIT_LOG", v);
        }
    }
}
