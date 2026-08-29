//! # Auth Companion (v0.17.6) — интерактивное окно авторизации Google.
//!
//! Локальный Node.js-мост `scripts/auth-companion.js` (zero-dependency):
//!
//! ```text
//! poler-engine --auth-ui
//!   └─ spawn: node scripts/auth-companion.js
//!        ├─ Chromium с ИЗОЛИРОВАННЫМ профилем движка:
//!        │    userDataDir = ~/.cache/poler-engine/google-profile
//!        │    (владелец вводит логин/пароль и 2FA СВОИМИ руками)
//!        ├─ поллинг кук через CDP (Storage.getCookies) до полного ядра
//!        │    сессии: SID + HSID + SSID + APISID + SAPISID на .google.com
//!        ├─ снапшот → ~/.config/poler-engine/google_session.json (0600)
//!        └─ graceful-закрытие окна (CDP Browser.close): куки флэшатся
//!             на диск, висячих процессов не остаётся
//! ```
//!
//! ## Гарантии безопасности (ТЗ)
//! * **No Host Snooping** — профиль хоста (`~/.config/chromium`,
//!   `~/.config/google-chrome`) не читается и не пишется; companion
//!   отказывается стартовать, если `POLER_GOOGLE_PROFILE` указывает туда.
//! * **Localhost Only** — статус-сервер (`GET /status`, `POST /shutdown`)
//!   и DevTools-порт слушают строго `127.0.0.1`.
//! * **Auto-termination** — окно закрывается самим companion-ом после
//!   подтверждения входа; Ctrl+C тоже прибирает браузер.
//!
//! ## Почему НЕ google_tokens.json
//! `google_tokens.json` — строго типизированное OAuth-хранилище
//! ([`crate::google::oauth::StoredTokens`]: access/refresh-токены для
//! Gmail/Drive, выдаются consent-флоу `--google-auth`). Браузерная сессия —
//! другой класс креденшелов: вместо порчи OAuth-хранилища пишем снапшот в
//! отдельный `google_session.json`, а «синхронизация хранилища профиля»
//! происходит сама собой — куки уже лежат в изолированном профиле, который
//! читают `--google-fetch` / `--nlm-*`. OAuth-токены companion выдать не
//! может (нужен consent-экран) — они по-прежнему только через
//! `poler-engine --google-auth`.
//!
//! ## Контракт exit-кодов companion-скрипта
//! | код | смысл                                        |
//! |-----|----------------------------------------------|
//! | 0   | авторизация зафиксирована                    |
//! | 2   | окно закрыто до завершения входа            |
//! | 3   | таймаут ожидания входа                      |
//! | 4   | preflight (нет Node≥18/Chromium, snoop, …)  |
//! | 130 | прервано сигналом (Ctrl+C)                  |

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::{audit, config_dir, profile_dir};

/// Имя companion-скрипта в репозитории (scripts/…).
pub const COMPANION_SCRIPT: &str = "auth-companion.js";

/// Путь к снапшоту сессии: `$POLER_GOOGLE_SESSION` →
/// `~/.config/poler-engine/google_session.json`. Секретный файл (0600),
/// содержит значения кук — наружу печатаем только имена.
pub fn session_path() -> PathBuf {
    if let Ok(p) = std::env::var("POLER_GOOGLE_SESSION") {
        return PathBuf::from(p);
    }
    config_dir().join("google_session.json")
}

/// Transient-состояние companion-а (state/port/cookie_count; без секретов).
/// Движок его не парсит — файл для наблюдаемости и отладки.
pub fn companion_state_path() -> PathBuf {
    config_dir().join("auth-companion.state.json")
}

/// Ядро браузерной сессии Google: все 5 непустых кук на .google.com —
/// вход подтверждён (контракт синхронизирован с scripts/auth-companion.js).
pub const CORE_SESSION_COOKIES: [&str; 5] = ["SID", "HSID", "SSID", "APISID", "SAPISID"];

// ---------------------------------------------------------------------------
// снапшот google_session.json (serde-модель, camelCase как в JS)
// ---------------------------------------------------------------------------

/// Одна кука снапшота. `value` — секрет: печатать нельзя.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct SnapshotCookie {
    pub name: String,
    pub domain: String,
    pub path: String,
    pub value: String,
    pub expires: f64,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
}

/// Снапшот сессии, который пишет scripts/auth-companion.js.
/// Верхний уровень — snake_case (как в JS), поля кук — camelCase (CDP).
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
#[serde(default)]
pub struct SessionSnapshot {
    pub captured_at: String,
    pub core_ok: bool,
    pub missing_core: Vec<String>,
    pub cookie_names: Vec<String>,
    pub cookies: Vec<SnapshotCookie>,
    pub source: String,
}

/// Парсинг снапшота (чистая функция — тестируемая).
pub fn parse_session_snapshot(text: &str) -> Result<SessionSnapshot, String> {
    serde_json::from_str(text).map_err(|e| format!("google_session.json повреждён: {e}"))
}

/// Загрузка снапшота из стандартного пути.
pub fn load_session_snapshot() -> Result<(SessionSnapshot, PathBuf), String> {
    let p = session_path();
    let text = std::fs::read_to_string(&p).map_err(|_| {
        format!(
            "снапшот сессии не найден ({}). Сначала: poler-engine --auth-ui",
            p.display()
        )
    })?;
    Ok((parse_session_snapshot(&text)?, p))
}

/// Краткое описание снапшота БЕЗ значений кук (секреты не печатаем).
pub fn snapshot_summary(s: &SessionSnapshot) -> String {
    let have: HashSet<&str> = s.cookie_names.iter().map(String::as_str).collect();
    let core: Vec<&str> = CORE_SESSION_COOKIES
        .iter()
        .copied()
        .filter(|n| have.contains(n))
        .collect();
    let missing: Vec<&str> = CORE_SESSION_COOKIES
        .iter()
        .copied()
        .filter(|n| !have.contains(n))
        .collect();
    if missing.is_empty() {
        format!(
            "кук Google: {}, ядро сессии полное ({})",
            s.cookies.len(),
            core.join(" ")
        )
    } else {
        format!(
            "кук Google: {}, ядро НЕПОЛНОЕ (нет: {})",
            s.cookies.len(),
            missing.join(", ")
        )
    }
}

// ---------------------------------------------------------------------------
// поиск node + companion-скрипта
// ---------------------------------------------------------------------------

/// Кандидаты companion-скрипта в порядке приоритета (чистая функция).
/// 1. `$POLER_AUTH_COMPANION` (явный override)
/// 2. `<cwd>/scripts/auth-companion.js` (запуск из репо)
/// 3. `<exe_dir>/scripts/…` и `<exe_dir>/../scripts/…` (установленный бинарь)
/// 4. `~/.local/share/poler-engine/scripts/…`
pub fn companion_candidates(
    env_script: Option<&Path>,
    exe_dir: Option<&Path>,
    cwd: &Path,
    home: Option<&str>,
) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if let Some(p) = env_script {
        v.push(PathBuf::from(p));
    }
    v.push(cwd.join("scripts").join(COMPANION_SCRIPT));
    if let Some(d) = exe_dir {
        v.push(d.join("scripts").join(COMPANION_SCRIPT));
        v.push(d.join("..").join("scripts").join(COMPANION_SCRIPT));
    }
    if let Some(h) = home {
        if !h.is_empty() {
            v.push(
                PathBuf::from(h)
                    .join(".local/share/poler-engine/scripts")
                    .join(COMPANION_SCRIPT),
            );
        }
    }
    let mut seen: HashSet<PathBuf> = HashSet::new();
    v.into_iter().filter(|p| seen.insert(p.clone())).collect()
}

/// Первый существующий companion-скрипт.
pub fn find_companion_script() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let env_script = std::env::var("POLER_AUTH_COMPANION")
        .ok()
        .map(PathBuf::from);
    let home = std::env::var("HOME").ok();
    companion_candidates(
        env_script.as_deref(),
        exe_dir.as_deref(),
        &cwd,
        home.as_deref(),
    )
    .into_iter()
    .find(|p| p.is_file())
}

/// Node.js-бинарь: `$POLER_NODE_BIN` → поиск в PATH.
pub fn find_node_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("POLER_NODE_BIN") {
        if Path::new(&p).is_file() {
            return Some(PathBuf::from(p));
        }
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .map(|d| Path::new(d).join("node"))
        .find(|p| p.is_file())
}

/// Человекочитаемая интерпретация exit-кода companion-а (чистая функция).
pub fn describe_companion_exit(code: i32) -> String {
    match code {
        0 => "авторизация зафиксирована".to_string(),
        2 => "окно закрыто до завершения входа (логин не подтверждён)".to_string(),
        3 => "таймаут ожидания входа (см. POLER_AUTH_TIMEOUT_SECS)".to_string(),
        4 => "companion не смог стартовать: нет Node>=18/Chromium, профиль \
              занят другим окном или запрещённый путь (подробности выше)"
            .to_string(),
        130 => "прервано сигналом (Ctrl+C)".to_string(),
        c if c > 128 => format!("companion убит сигналом {}", c - 128),
        c => format!("companion завершился с кодом {c}"),
    }
}

// ---------------------------------------------------------------------------
// CLI: poler-engine --auth-ui
// ---------------------------------------------------------------------------

/// Поднять интерактивное окно авторизации (spawn node + ожидание).
/// stdout/stderr companion-а пробрасываются в терминал владельца.
pub fn run_auth_ui() -> Result<(), String> {
    let node = find_node_bin().ok_or_else(|| {
        "Node.js не найден (нужен для окна авторизации). Установи node >= 18 \
         или задай POLER_NODE_BIN=/путь/к/node"
            .to_string()
    })?;
    let script = find_companion_script().ok_or_else(|| {
        format!(
            "companion-скрипт не найден ({COMPANION_SCRIPT}). Запускай из \
             репо poler-engine, установи скрипт рядом с бинарем или задай \
             POLER_AUTH_COMPANION=/путь/к/auth-companion.js"
        )
    })?;

    // Профиль один: если google-браузер уже поднят (окно --google-browse),
    // второй Chromium с тем же user-data-dir не стартует — предупреждаем.
    let busy = crate::web::cdp_alive(super::google_cdp_port());
    if busy {
        println!(
            "⚠ Порт CDP {} занят — похоже, открыт google-браузер poler-engine.",
            super::google_cdp_port()
        );
        println!("  Закрой окно --google-browse (или зависший --google-fetch)");
        println!("  и повтори: профиль один, второй Chromium с ним не поднимется.");
    }

    let _ = std::fs::create_dir_all(config_dir());
    let _ = std::fs::create_dir_all(profile_dir());

    println!("poler-engine auth-ui: изолированное окно входа Google");
    println!("  node:     {}", node.display());
    println!("  скрипт:   {}", script.display());
    println!("  профиль:  {}", profile_dir().display());
    println!("  Правило: логин и 2FA — ТОЛЬКО в открывшемся окне, своими руками.");
    println!("  Ctrl+C — прервать. Хост-браузер не читается и не трогается.");
    println!();

    let status = Command::new(&node)
        .arg(&script)
        .status()
        .map_err(|e| format!("запуск node {}: {e}", script.display()))?;
    let code = status.code().unwrap_or(-1);
    // В audit — только код и счётчики, без секретов.
    audit::record("security.auth_companion", &format!("exit={code}"));

    match code {
        0 => {
            let (snap, p) = load_session_snapshot()?;
            println!();
            println!("✓ Авторизация зафиксирована.");
            println!("  Снапшот сессии:  {} (0600)", p.display());
            println!("  {}", snapshot_summary(&snap));
            println!("  Профиль движка:  {}", profile_dir().display());
            println!("  Проверка сессии: poler-engine --nlm-account");
            println!("  Gmail/Drive (OAuth-токены) — отдельно: poler-engine --google-auth");
            audit::record(
                "security.auth_companion",
                &format!(
                    "state=authorized cookies={} core_ok={}",
                    snap.cookies.len(),
                    snap.core_ok
                ),
            );
            Ok(())
        }
        c => Err(describe_companion_exit(c)),
    }
}

// ---------------------------------------------------------------------------
// тесты (чистые функции — без гонок окружения)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn companion_candidates_order_and_dedup() {
        let tmp = std::env::temp_dir();
        let cwd = tmp.join("repo");
        let exe = tmp.join("usr-local-bin");
        let env_script = tmp.join("custom").join(COMPANION_SCRIPT);
        let home = tmp.join("home");

        let v = companion_candidates(
            Some(&env_script),
            Some(&exe),
            &cwd,
            Some(home.to_str().unwrap()),
        );
        // порядок приоритета
        assert_eq!(v[0], env_script, "env override первый");
        assert_eq!(v[1], cwd.join("scripts").join(COMPANION_SCRIPT), "cwd/scripts");
        assert_eq!(v[2], exe.join("scripts").join(COMPANION_SCRIPT), "exe/scripts");
        assert_eq!(
            v[3],
            exe.join("..").join("scripts").join(COMPANION_SCRIPT),
            "exe/../scripts"
        );
        assert_eq!(
            v[4],
            home.join(".local/share/poler-engine/scripts").join(COMPANION_SCRIPT),
            "~/.local/share"
        );
        // дедупликация: env == cwd → одна запись
        let v2 = companion_candidates(
            Some(&cwd.join("scripts").join(COMPANION_SCRIPT)),
            None,
            &cwd,
            None,
        );
        assert_eq!(v2.len(), 1, "дубликат схлопнут");
        // без home — последняя запись не добавляется
        let v3 = companion_candidates(None, None, &cwd, None);
        assert_eq!(v3.len(), 1);
        // пустой home — тоже не добавляется
        let v4 = companion_candidates(None, None, &cwd, Some(""));
        assert_eq!(v4.len(), 1);
    }

    #[test]
    fn describe_companion_exit_known_codes() {
        assert_eq!(describe_companion_exit(0), "авторизация зафиксирована");
        assert!(describe_companion_exit(2).contains("окно закрыто"));
        assert!(describe_companion_exit(3).contains("таймаут"));
        assert!(describe_companion_exit(4).contains("companion не смог стартовать"));
        assert!(describe_companion_exit(130).contains("прервано сигналом"));
        assert!(describe_companion_exit(137).contains("сигналом 9"));
        assert!(describe_companion_exit(7).contains("кодом 7"));
    }

    #[test]
    fn parse_session_snapshot_ok() {
        let text = r#"{
          "captured_at": "2026-08-29T12:00:00.000Z",
          "core_ok": true,
          "missing_core": [],
          "cookie_names": ["SID", "HSID"],
          "cookies": [
            {"name": "SID", "domain": ".google.com", "path": "/",
             "value": "SECRET", "expires": 4102444800.0,
             "secure": true, "httpOnly": true, "sameSite": "Lax"}
          ],
          "source": "auth-companion 0.17.6"
        }"#;
        let snap = parse_session_snapshot(text).expect("парсинг");
        assert!(snap.core_ok);
        assert_eq!(snap.cookie_names, vec!["SID", "HSID"]);
        assert_eq!(snap.cookies.len(), 1);
        assert!(snap.cookies[0].http_only, "camelCase httpOnly");
        assert_eq!(snap.cookies[0].same_site.as_deref(), Some("Lax"));
        assert_eq!(snap.source, "auth-companion 0.17.6");
    }

    #[test]
    fn parse_session_snapshot_tolerates_missing_optional() {
        // JS может дописать новые поля; serde default их переживает.
        let text = r#"{"captured_at":"t","core_ok":false,"cookie_names":[],"cookies":[],"source":"x"}"#;
        let snap = parse_session_snapshot(text).expect("минимальный JSON парсится");
        assert!(!snap.core_ok);
        assert!(snap.missing_core.is_empty(), "missing_core имеет default");
    }

    #[test]
    fn parse_session_snapshot_bad_json() {
        let err = parse_session_snapshot("not json").expect_err("битый JSON");
        assert!(err.contains("повреждён"), "текст ошибки: {err}");
    }

    #[test]
    fn snapshot_summary_hides_values() {
        let snap = SessionSnapshot {
            captured_at: "t".into(),
            core_ok: true,
            missing_core: vec![],
            cookie_names: CORE_SESSION_COOKIES.iter().map(|s| s.to_string()).collect(),
            cookies: CORE_SESSION_COOKIES
                .iter()
                .map(|n| SnapshotCookie {
                    name: n.to_string(),
                    value: format!("SECRET-VALUE-{}", n),
                    domain: ".google.com".into(),
                    ..Default::default()
                })
                .collect(),
            source: "test".into(),
        };
        let summary = snapshot_summary(&snap);
        assert!(summary.contains("полное"), "summary: {summary}");
        assert!(!summary.contains("SECRET-VALUE"), "значения кук не печатаются");
    }

    #[test]
    fn snapshot_summary_reports_missing_core() {
        let snap = SessionSnapshot {
            captured_at: "t".into(),
            core_ok: false,
            missing_core: vec!["SAPISID".into()],
            cookie_names: vec!["SID".into(), "HSID".into()],
            cookies: vec![SnapshotCookie {
                name: "SID".into(),
                ..Default::default()
            }],
            source: "test".into(),
        };
        let summary = snapshot_summary(&snap);
        assert!(summary.contains("НЕПОЛНОЕ"), "summary: {summary}");
        assert!(summary.contains("SAPISID"), "нет имени отсутствующей куки");
    }

    #[test]
    fn core_cookies_contract_matches_js() {
        // Контракт синхронизирован с scripts/auth-companion.js::CORE_COOKIES
        // (там симметричный тест EXIT-контракта).
        assert_eq!(
            CORE_SESSION_COOKIES.to_vec(),
            vec!["SID", "HSID", "SSID", "APISID", "SAPISID"]
        );
    }
}
