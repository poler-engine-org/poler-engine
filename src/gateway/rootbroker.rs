//! # Root Broker — «sudo как услуга» через шлюз (v0.27.0)
//!
//! Живой кейс эксплуатации: агент в Container Jail честно жалуется «нет рута»
//! (uid 1000: apt-get и системные установки в образе недоступны). Вывод
//! владельца: **«рут — это тоже привилегия хоста»** — настоящим хостовым
//! рутом агенту владеть нельзя В ПРИНЦИПЕ, но возможность рут-исполнения
//! ему нужна. Модель — та же, что у облачных AI-рантаймов Google
//! («в песочнице нет Python — Python вызывают снаружи»):
//!
//! * агент в клетке **НЕ ДЕРЖИТ** рут ни секунды: у него есть `sudo`-шим,
//!   который умеет только ЗАПРОСИТЬ исполнение (файл-канал брокера);
//! * шлюз (контур 2) читает запрос, **судья** (`sandbox.rs` + корневая
//!   политика ниже) выносит вердикт: деструктив — Deny всегда, ручной
//!   allowlist владельца — Allow, менеджеры пакетов — Allow по умолчанию;
//! * при Allow исполнение происходит **со стороны хоста**:
//!   `docker exec -u 0:0 <box> <argv…>` — root ВНУТРИ контейнера,
//!   контейнер по-прежнему cap-drop ALL / no-new-privileges;
//! * агенту возвращаются ТОЛЬКО stdout/stderr + exit-код.
//!
//! Канал — файловый (без сети, работает при net=none, детерминированно
//! тестируется): хост-каталог `<box_home>/.poler-broker` (= контейнерный
//! `/home/poler/.poler-broker`, уже смонтирован как /home/poler):
//! * `requests.jsonl` — строки `<id>|<b64 cwd>|<b64 argv0>|<b64 argv1>…`
//!   (агент дописывает, шлюз читает по смещению);
//! * `responses/<id>.exit|.out|.err|.msg` — ответы шлюза (атомарная запись
//!   tmp+rename: симлинк-подмена агентом НЕ разыменовывается);
//! * `enabled` — маркер живости: брокер обновляет его каждые 5с, шим
//!   требует свежесть ≤2 мин (крах шлюза = маркер протух = шим честно
//!   отказывает, а не висит 120с).
//!
//! Политика владельца живёт ТОЛЬКО на хосте вне монтируемых каталогов
//! (`~/.local/share/poler-engine/policy/<box>.sudo` — агент-зец не может
//! дописать себя в allowlist). Аудит — тоже на хосте
//! (`~/.local/share/poler-engine/audit/root-<box>.jsonl`).
//!
//! Инварианты:
//! * **Block не ослабляется ничем** — allowlist не перекрывает вердикт судьи;
//! * **Zero Silent Escalation** — Confirm из клетки не подтверждается:
//!   рут-брокер отвечает отказом (подтверждение доступно только владельцу
//!   в шлюзе);
//! * лимит 30 запросов/час (анти-спам из клетки), таймаут 120с, кап вывода;
//! * инструменты контейнер-побега (docker/nsenter/unshare/mount/…) и пути
//!   ядра (/proc /sys /dev) в рут-запросах — Deny всегда.
//!
//! Нулей новых зависимостей: base64 — ручной кодек (стандартный алфавит).

use std::collections::VecDeque;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::containers::{BoxState, HOME_MOUNT};
use super::sandbox::{self, Policy};

/// Контейнерный каталог брокер-канала (= хост `<box_home>/.poler-broker`).
pub const BROKER_MOUNT: &str = "/home/poler/.poler-broker";
/// Контейнерный каталог шимов (= хост `<box_home>/.poler-bin`); попадает
/// первым в PATH контейнера (docker_run_argv v0.27.0).
pub const POLER_BIN_MOUNT: &str = "/home/poler/.poler-bin";
/// Хвостовое имя файла запросов.
const REQUESTS_FILE: &str = "requests.jsonl";
/// Хвостовое имя маркера живости брокера.
const MARKER_FILE: &str = "enabled";
/// Лимит рут-запросов в час (анти-спам: клетка не должна долбить судью).
const RATE_LIMIT_PER_HOUR: usize = 30;
/// Таймаут рут-исполнения (docker exec -u 0).
const ROOT_EXEC_TIMEOUT_SECS: u64 = 120;
/// Кап stdout/stderr рут-ответа агенту.
const ROOT_OUT_CAP: usize = 1_048_576;
const ROOT_ERR_CAP: usize = 131_072;
/// Период обновления маркера живости.
const MARKER_REFRESH: Duration = Duration::from_secs(5);
/// Тик чтения канала запросов.
const POLL_TICK: Duration = Duration::from_millis(150);
/// Максимум аргументов / длина строки запроса (защита от мусора).
const MAX_ARGS: usize = 64;
const MAX_LINE: usize = 8192;

// ---------------------------------------------------------------------------
// base64-кодек (стандартный алфавит; без новых зависимостей)
// ---------------------------------------------------------------------------

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Кодировать в base64 (с паддингом `=`).
pub fn b64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(B64_ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(B64_ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            B64_ALPHABET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            B64_ALPHABET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// Декодировать base64 (символы вне алфавита/`=` — ошибка).
pub fn b64_decode(s: &str) -> Result<Vec<u8>, String> {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    if bytes.len() % 4 != 0 {
        return Err("длина не кратна 4".into());
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for (ci, chunk) in bytes.chunks(4).enumerate() {
        let mut vals = [0u32; 4];
        let mut pad = 0usize;
        for (i, &c) in chunk.iter().enumerate() {
            if c == b'=' {
                if ci + 1 != bytes.len() / 4 {
                    return Err("= в середине".into());
                }
                pad += 1;
                vals[i] = 0;
            } else {
                let v = B64_ALPHABET
                    .iter()
                    .position(|&a| a == c)
                    .ok_or_else(|| format!("символ «{}» вне алфавита", c as char))?;
                vals[i] = v as u32;
            }
        }
        if pad > 2 || (pad > 0 && chunk[..4 - pad].contains(&b'=')) {
            return Err("некорректный паддинг".into());
        }
        let n = (vals[0] << 18) | (vals[1] << 12) | (vals[2] << 6) | vals[3];
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Протокол запроса
// ---------------------------------------------------------------------------

/// Разобранный рут-запрос из клетки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SudoRequest {
    /// Идентификатор (генерит шим: `r<pid>-<epoch>`; валидный charset).
    pub id: String,
    /// cwd ВНУТРИ контейнера (контейнерный абсолютный путь).
    pub cwd: String,
    /// argv полезной нагрузки (без «sudo» — шим и есть sudo).
    pub argv: Vec<String>,
}

/// Валидный id: `[A-Za-z0-9._-]{1,64}` — без слэшей/«..» (id попадает в
/// имена файлов ответов: path traversal исключается на парсинге).
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id != ".."
        && id != "."
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

/// Собрать строку запроса (канон протокола; шим собирает её же в sh).
pub fn encode_request(id: &str, cwd: &str, argv: &[String]) -> String {
    let mut parts: Vec<String> = vec![id.to_string(), b64_encode(cwd.as_bytes())];
    for a in argv {
        parts.push(b64_encode(a.as_bytes()));
    }
    parts.join("|")
}

/// Разобрать строку запроса. Fail-closed: любая дрянь — ошибка.
pub fn parse_request_line(line: &str) -> Result<SudoRequest, String> {
    let line = line.trim_end_matches('\n').trim_end_matches('\r');
    if line.is_empty() {
        return Err("пустая строка".into());
    }
    if line.len() > MAX_LINE {
        return Err(format!("строка длиннее {MAX_LINE} байт"));
    }
    let parts: Vec<&str> = line.split('|').collect();
    if parts.len() < 3 {
        return Err("меньше трёх полей (id|cwd|argv0)".into());
    }
    if parts.len() > MAX_ARGS + 2 {
        return Err(format!("больше {MAX_ARGS} аргументов"));
    }
    let id = parts[0].to_string();
    if !valid_id(&id) {
        return Err(format!("id «{id}» вне [A-Za-z0-9._-] (≤64)"));
    }
    let cwd_bytes = b64_decode(parts[1]).map_err(|e| format!("cwd: {e}"))?;
    let cwd = String::from_utf8(cwd_bytes).map_err(|_| "cwd: не UTF-8".to_string())?;
    let mut argv: Vec<String> = Vec::with_capacity(parts.len() - 2);
    for p in &parts[2..] {
        let b = b64_decode(p).map_err(|e| format!("argv: {e}"))?;
        let s = String::from_utf8(b).map_err(|_| "argv: не UTF-8".to_string())?;
        argv.push(s);
    }
    Ok(SudoRequest { id, cwd, argv })
}

// ---------------------------------------------------------------------------
// Судья рут-запросов (pure — юнит-тестируемый)
// ---------------------------------------------------------------------------

/// Вердикт рут-брокера.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootVerdict {
    /// Исполнить от рута (docker exec -u 0) со стороны хоста.
    Allow,
    /// Отказ (причина уходит агенту в .msg и в аудит).
    Deny(String),
}

/// Инструменты контейнер-побега/управления демоном: через рут-брокер
/// они не проходят НИКОГДА (брокер не должен стать примитивом побега).
const ROOT_ESCAPE_TOOLS: &[&str] = &[
    "docker",
    "podman",
    "nerdctl",
    "ctr",
    "crun",
    "runc",
    "dockerd",
    "docker-compose",
    "podman-compose",
    "nsenter",
    "unshare",
    "setns",
    "switch_root",
    "mount",
    "umount",
    "reboot",
    "shutdown",
    "poweroff",
    "halt",
    "telinit",
    "init",
    "modprobe",
    "insmod",
    "rmmod",
    "sysctl",
    "iptables",
    "nft",
    "ip6tables",
    "ebtables",
    "ptrace",
    "gdb",
    "proot",
    "nsjail",
    "bwrap",
    "chroot",
];

/// Пути ядра/устройств/демона, запрещённые в аргументах рут-запроса.
const ROOT_ARG_DENY_PREFIXES: &[&str] = &["/proc", "/sys", "/dev", "/run", "/var/run"];

/// Подкоманды менеджеров пакетов, разрешённые ПО УМОЛЧАНИЮ (жалоба агента
/// «нет рута» — почти всегда про apt-get install).
const PKG_TOOLS: &[&str] = &["apt-get", "apt", "dpkg", "dpkg-deb"];
const PKG_SAFE_SUBCMDS: &[&str] = &[
    "update",
    "install",
    "download",
    "show",
    "upgrade",
    "dist-upgrade",
    "clean",
    "autoremove",
    "remove",
    "purge",
    "list",
    "search",
    "-i",
    "-r",
    "-P",
    "-l",
    "-s",
    "-L",
    "--configure",
    "--unpack",
    "reconfigure",
];

/// Файловые операции, разрешённые в границах /workspace и /home/poler.
const FS_SCOPE_TOOLS: &[&str] = &[
    "mkdir", "mv", "cp", "touch", "chmod", "chown", "install", "tee", "ln",
];

/// Безобидные идентификационные команды.
const SAFE_TOOLS: &[&str] = &["id", "whoami", "true", "pwd", "uname"];

/// Проверить, что путь-подобный аргумент остаётся в разрешённой зоне.
fn path_in_scope(arg: &str) -> bool {
    if arg.starts_with('-') {
        return true; // флаг
    }
    let looks_path = arg.starts_with('/')
        || arg.starts_with("./")
        || arg.starts_with("../")
        || arg == "."
        || arg == "..";
    if !looks_path {
        return true; // не путь (имя пакета и т.п.)
    }
    let a = arg.trim_end_matches('/');
    a == "/workspace"
        || a == HOME_MOUNT
        || a.starts_with("/workspace/")
        || a.starts_with("/home/poler/")
}

/// Простой glob ( `*` — любая последовательность, `?` — один символ ).
pub fn glob_match(pat: &str, s: &str) -> bool {
    // итеративный two-pointer с бэктрекингом по «*»
    let (p, t) = (pat.as_bytes(), s.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == b'?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

/// Вердикт по рут-запросу. Порядок — инварианты вперёд:
/// 1) судья `sandbox.rs` по argv (Block → Deny ВСЕГДА; Confirm → Deny:
///    клетка не подтверждает — Zero Silent Escalation);
/// 2) инструменты побега/демона → Deny;
/// 3) пути ядра/устройств в аргументах → Deny;
/// 4) allowlist владельца (glob по «argv0 argv1 …»);
/// 5) дефолтный allow: пакетные менеджеры + fs-опы в границах + id/whoami;
/// 6) остальное — Deny с подсказкой.
pub fn judge_root_request(req: &SudoRequest, allow_globs: &[String]) -> RootVerdict {
    if req.argv.is_empty() {
        return RootVerdict::Deny("пустая полезная нагрузка".into());
    }
    // 1) первый эшелон — общий судья
    match sandbox::judge_segment(&req.argv) {
        Policy::Block(why) => {
            return RootVerdict::Deny(format!("судья: BLOCK — {why}"));
        }
        Policy::Confirm(why) => {
            return RootVerdict::Deny(format!(
                "судья: требует подтверждения ({why}) — рут-брокер молча не подтверждает; владелец: box allow sudo «<glob>»"
            ));
        }
        Policy::Allow => {}
    }
    // 2) инструменты побега
    let base = req.argv[0].rsplit('/').next().unwrap_or(&req.argv[0]);
    if ROOT_ESCAPE_TOOLS.contains(&base) {
        return RootVerdict::Deny(format!(
            "«{base}» — инструмент побега/управления контейнер-демоном: рут-брокер такие не исполняет"
        ));
    }
    // 3) пути ядра/демона в аргументах
    for a in &req.argv[1..] {
        if a.contains("docker.sock") || a.contains("podman.sock") {
            return RootVerdict::Deny("аргумент ссылается на docker-сокет".into());
        }
        if ROOT_ARG_DENY_PREFIXES.iter().any(|pfx| a.starts_with(pfx)) {
            return RootVerdict::Deny(format!(
                "путь «{a}» — ядро/устройства/раннтайм: вне рут-политики"
            ));
        }
    }
    // 4) allowlist владельца (хост-only файл; Block выше уже отработал)
    let joined = req.argv.join(" ");
    for g in allow_globs {
        if glob_match(g, &joined) {
            return RootVerdict::Allow;
        }
    }
    // 5) дефолтный allow
    if PKG_TOOLS.contains(&base) {
        let sub = req.argv.get(1).map(|s| s.as_str()).unwrap_or("");
        if PKG_SAFE_SUBCMDS.contains(&sub) {
            return RootVerdict::Allow;
        }
        return RootVerdict::Deny(format!(
            "«{base} {sub}» — подкоманда вне дефолтного набора (update/install/…); box allow sudo — расширить"
        ));
    }
    if FS_SCOPE_TOOLS.contains(&base) && req.argv[1..].iter().all(|a| path_in_scope(a)) {
        return RootVerdict::Allow;
    }
    if SAFE_TOOLS.contains(&base) {
        return RootVerdict::Allow;
    }
    // 6) отказ по умолчанию
    RootVerdict::Deny(format!(
        "«{joined}» не в рут-политике (менеджеры пакетов/fs в границах — дефолт; box allow sudo «<glob>» — точечно разрешить владельцу)"
    ))
}

// ---------------------------------------------------------------------------
// Политика владельца (host-only, вне монтируемых каталогов!)
// ---------------------------------------------------------------------------

/// База политик: `POLER_POLICY_HOME` (тесты) | `~/.local/share/poler-engine/policy`.
/// ВАЖНО: НЕ внутри box_home_base() — home бокса смонтирован в клетку rw,
/// агент не должен уметь дописывать себя в allowlist.
pub fn policy_base() -> PathBuf {
    if let Ok(d) = std::env::var("POLER_POLICY_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share/poler-engine/policy")
}

/// Файл allowlist-глобов конкретного бокса.
pub fn policy_path(box_name: &str) -> PathBuf {
    policy_base().join(format!("{box_name}.sudo"))
}

/// Прочитать глобы (файла нет → пусто; строки-комменты `#` игнорируются).
pub fn load_allow_globs(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect()
}

/// Добавить глоб (валидация: непустой, без перевода строки, ≤128).
pub fn add_allow_glob(path: &Path, glob: &str) -> Result<String, String> {
    let g = glob.trim();
    if g.is_empty() {
        return Err("box allow sudo: пустой glob".into());
    }
    if g.contains('\n') || g.contains('\r') {
        return Err("box allow sudo: glob без переводов строк".into());
    }
    if g.len() > 128 {
        return Err("box allow sudo: glob длиннее 128 символов".into());
    }
    if g.contains(|c: char| c.is_control()) {
        return Err("box allow sudo: control-символы запрещены".into());
    }
    let mut list = load_allow_globs(path);
    if list.iter().any(|x| x == g) {
        return Ok(format!("box allow sudo: «{g}» уже в списке\n"));
    }
    list.push(g.to_string());
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("policy {}: {e}", parent.display()))?;
    }
    let body = format!(
        "# POLER root-broker allowlist (владелец; host-only)\n{}\n",
        list.join("\n")
    );
    std::fs::write(path, body).map_err(|e| format!("policy {}: {e}", path.display()))?;
    Ok(format!(
        "box allow sudo: «{g}» добавлен (список: {})\n⛔ инвариант: Block-вердикты судьи allowlist НЕ ослабляет\n",
        list.len()
    ))
}

/// Очистить allowlist.
pub fn reset_allow_globs(path: &Path) -> Result<String, String> {
    let _ = std::fs::remove_file(path);
    Ok("box allow sudo: allowlist очищен\n".into())
}

// ---------------------------------------------------------------------------
// Аудит (host-only JSONL)
// ---------------------------------------------------------------------------

/// База аудита: `POLER_AUDIT_HOME` (тесты) | `~/.local/share/poler-engine/audit`.
pub fn audit_base() -> PathBuf {
    if let Ok(d) = std::env::var("POLER_AUDIT_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share/poler-engine/audit")
}

/// Файл аудита рут-запросов бокса.
pub fn audit_path(box_name: &str) -> PathBuf {
    audit_base().join(format!("root-{box_name}.jsonl"))
}

/// Дописать JSON-запись в аудит (создаёт каталог; ошибки не роняют брокер).
fn audit_append(box_name: &str, value: serde_json::Value) {
    let path = audit_path(box_name);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut line = value.to_string();
    line.push('\n');
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// Хвост аудита для `box sudo log [N]`.
pub fn tail_audit(box_name: &str, n: usize) -> String {
    let path = audit_path(box_name);
    let Ok(text) = std::fs::read_to_string(&path) else {
        return format!("руут-аудит пуст ({}) — запросов не было\n", path.display());
    };
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return format!("руут-аудит пуст ({})\n", path.display());
    }
    let take = n.clamp(1, 200);
    let start = lines.len().saturating_sub(take);
    let mut s = format!(
        "руут-аудит {box_name} — последние {} из {} записей:\n",
        lines.len() - start,
        lines.len()
    );
    for l in &lines[start..] {
        // компактный вид: {ts, argv, verdict, reason}
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(l) {
            let argv = v
                .get("argv")
                .and_then(|a| a.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            s.push_str(&format!(
                "  · {} · [{}] {}{}{}\n",
                v.get("ts").and_then(|t| t.as_i64()).unwrap_or(0),
                v.get("verdict").and_then(|t| t.as_str()).unwrap_or("?"),
                argv,
                v.get("reason")
                    .and_then(|t| t.as_str())
                    .filter(|r| !r.is_empty())
                    .map(|r| format!(" · {r}"))
                    .unwrap_or_default(),
                v.get("exit")
                    .and_then(|t| t.as_i64())
                    .filter(|e| *e != 0)
                    .map(|e| format!(" · exit {e}"))
                    .unwrap_or_default(),
            ));
        } else {
            s.push_str(&format!("  · {l}\n"));
        }
    }
    s
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Шим sudo внутри клетки (deploy: box on / box sudo on)
// ---------------------------------------------------------------------------

/// Текст `sudo`-шима (POSIX sh; coreutils: date/base64/tr/find).
/// Рут остаётся у хоста: шим только оформляет ЗАПРОС и ждёт ответа брокера.
pub const SUDO_SHIM: &str = r#"#!/bin/sh
# POLER Engine v0.27.0 — root broker shim (sudo как услуга через шлюз).
# Рут — привилегия ХОСТА: агент может только ЗАПРОСИТЬ исполнение; судья
# шлюза решает, исполнение идёт docker exec -u 0 СО СТОРОНЫ ХОСТА.
# Агенту возвращаются только stdout/stderr/exit-код. Аудит — на хосте.
B=/home/poler/.poler-broker
if [ "$#" -eq 0 ]; then
  echo "poler-sudo: usage: sudo <команда…> — запрос уйдёт брокеру шлюза (судья решает)" >&2
  exit 2
fi
if [ ! -f "$B/enabled" ] || [ -z "$(find "$B/enabled" -mmin -2 2>/dev/null)" ]; then
  echo "poler-sudo: рут-брокер не активен (шлюз не слушает) — владелец: box sudo on в шлюзе" >&2
  exit 4
fi
ID="r$$-$(date +%s)"
REQ="$ID|$(pwd | base64 | tr -d '\n')"
for a in "$@"; do
  REQ="$REQ|$(printf %s "$a" | base64 | tr -d '\n')"
done
printf '%s\n' "$REQ" >> "$B/requests.jsonl" 2>/dev/null || {
  echo "poler-sudo: брокер-канал недоступен" >&2
  exit 3
}
i=0
while [ "$i" -lt 120 ]; do
  if [ -f "$B/responses/$ID.exit" ]; then
    [ -f "$B/responses/$ID.msg" ] && cat "$B/responses/$ID.msg" >&2
    [ -f "$B/responses/$ID.out" ] && cat "$B/responses/$ID.out"
    [ -f "$B/responses/$ID.err" ] && cat "$B/responses/$ID.err" >&2
    ex="$(cat "$B/responses/$ID.exit" 2>/dev/null)"
    rm -f "$B/responses/$ID.exit" "$B/responses/$ID.out" \
          "$B/responses/$ID.err" "$B/responses/$ID.msg" 2>/dev/null
    exit "${ex:-125}"
  fi
  i=$((i+1))
  sleep 1
done
echo "poler-sudo: таймаут ожидания решения шлюза (120с)" >&2
exit 124
"#;

/// Хост-каталог брокер-канала бокса.
pub fn broker_dir(box_home: &Path) -> PathBuf {
    box_home.join(".poler-broker")
}

/// Развернуть шимы в home бокса (идемпотентно: перезапись лечит подмену —
/// подмена шима привилегии не даёт: решение и исполнение на хосте).
pub fn deploy_shims(box_home: &Path) -> Result<(), String> {
    let bin = box_home.join(".poler-bin");
    let broker = broker_dir(box_home);
    std::fs::create_dir_all(&bin).map_err(|e| format!("shim {}: {e}", bin.display()))?;
    std::fs::create_dir_all(broker.join("responses"))
        .map_err(|e| format!("broker {}: {e}", broker.display()))?;
    for name in ["sudo", "poler-sudo"] {
        let p = bin.join(name);
        std::fs::write(&p, SUDO_SHIM).map_err(|e| format!("shim {}: {e}", p.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Ответы (атомарно: tmp + rename — симлинк-подмена не разыменовывается)
// ---------------------------------------------------------------------------

/// Безопасная запись файла ответа: tmp (create_new) → rename поверх цели.
/// rename заменяет сам inode записи каталога — симлинк агента будет
/// перезаписан КАК ЗАПИСЬ, а не через переход.
fn write_response_file(resp_dir: &Path, name: &str, bytes: &[u8]) -> Result<(), String> {
    let final_p = resp_dir.join(name);
    let tmp_p = resp_dir.join(format!(".tmp.{}.{}", name, std::process::id()));
    let _ = std::fs::remove_file(&tmp_p);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp_p)
        .map_err(|e| format!("tmp {}: {e}", tmp_p.display()))?;
    f.write_all(bytes).map_err(|e| format!("tmp write: {e}"))?;
    drop(f);
    std::fs::rename(&tmp_p, &final_p)
        .map_err(|e| format!("rename → {}: {e}", final_p.display()))?;
    Ok(())
}

/// Выслать агенту ответ по запросу.
fn write_response(resp_dir: &Path, req_id: &str, exit: i32, out: &str, err: &str, msg: &str) {
    let _ = write_response_file(
        resp_dir,
        &format!("{req_id}.exit"),
        format!("{exit}\n").as_bytes(),
    );
    let _ = write_response_file(resp_dir, &format!("{req_id}.out"), out.as_bytes());
    let _ = write_response_file(resp_dir, &format!("{req_id}.err"), err.as_bytes());
    if !msg.is_empty() {
        let _ = write_response_file(
            resp_dir,
            &format!("{req_id}.msg"),
            format!("{msg}\n").as_bytes(),
        );
    }
}

/// Кап строки с маркером обреза.
fn cap_str(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        s.to_string()
    } else {
        let mut cut = s
            .char_indices()
            .take_while(|(i, _)| *i <= cap)
            .last()
            .map(|(i, _)| i)
            .unwrap_or(cap);
        cut = cut.min(cap);
        format!("{}\n…(обрезано на капе {} байт)\n", &s[..cut], cap)
    }
}

// ---------------------------------------------------------------------------
// Исполнение от рута (host-сторона, docker exec -u 0:0)
// ---------------------------------------------------------------------------

fn docker_bin() -> String {
    std::env::var("POLER_BOX_DOCKER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "docker".into())
}

/// Безопасный cwd запроса: контейнерный абсолютный путь без «..».
fn safe_cwd(cwd: &str) -> String {
    if cwd.starts_with('/') && !cwd.contains("..") && cwd.len() <= 256 {
        cwd.to_string()
    } else {
        "/workspace".to_string()
    }
}

/// argv `docker exec -u 0:0 -w CWD BOX argv…` (pure — тестируем форму).
pub fn root_exec_argv(jail_name: &str, req: &SudoRequest) -> Vec<String> {
    let mut v: Vec<String> = vec![
        docker_bin(),
        "exec".into(),
        "-u".into(),
        "0:0".into(),
        "-w".into(),
        safe_cwd(&req.cwd),
        jail_name.into(),
    ];
    v.extend(req.argv.iter().cloned());
    v
}

/// Исполнить с таймаутом и раздельными пайпами. Возвращает
/// (exit, stdout, stderr, timed_out, spawn_err).
#[allow(clippy::type_complexity)]
fn exec_root(argv: &[String]) -> (i32, String, String, bool, Option<String>) {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child: Child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return (
                125,
                String::new(),
                String::new(),
                false,
                Some(format!("{e}")),
            )
        }
    };
    let t_out = child.stdout.take().map(super::containers::drain_pipe);
    let t_err = child.stderr.take().map(super::containers::drain_pipe);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if started.elapsed() >= Duration::from_secs(ROOT_EXEC_TIMEOUT_SECS) {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break None,
        }
    };
    let out = t_out
        .and_then(|t| t.join().ok())
        .map(|v| String::from_utf8_lossy(&v).to_string())
        .unwrap_or_default();
    let err = t_err
        .and_then(|t| t.join().ok())
        .map(|v| String::from_utf8_lossy(&v).to_string())
        .unwrap_or_default();
    match status {
        Some(st) => (st.code().unwrap_or(-1), out, err, false, None),
        None => (124, out, err, true, None),
    }
}

// ---------------------------------------------------------------------------
// Брокер (поток шлюза)
// ---------------------------------------------------------------------------

/// Счётчики/статус брокера (виден владельцу через `box sudo status`).
#[derive(Debug, Clone)]
pub struct BrokerStatus {
    pub box_name: String,
    pub processed: u64,
    pub allowed: u64,
    pub denied: u64,
    pub rate_limited: u64,
    pub malformed: u64,
    pub duplicates: u64,
    pub last_reason: Option<String>,
}

/// Ручка брокера: остановка + чтение статуса.
pub struct BrokerHandle {
    stop_flag: Arc<AtomicBool>,
    status: Arc<Mutex<BrokerStatus>>,
    broker_dir: PathBuf,
    /// поток не джойнимся: он может сидеть в docker exec до 120с;
    /// маркер живости снимаем сразу, поток завершится сам.
    _thread: Option<std::thread::JoinHandle<()>>,
}

impl BrokerHandle {
    /// Остановить брокер (снять маркер немедленно; поток доберётся сам).
    pub fn stop(&mut self) -> String {
        self.stop_flag.store(true, Ordering::SeqCst);
        let _ = std::fs::remove_file(self.broker_dir.join(MARKER_FILE));
        let st = self.status();
        format!(
            "🔐 рут-брокер ОСТАНОВЛЕН: обработано {} · allow {} · deny {} · rate {} · мусор {}\n",
            st.processed, st.allowed, st.denied, st.rate_limited, st.malformed
        )
    }

    /// Снимок статуса.
    pub fn status(&self) -> BrokerStatus {
        self.status
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Текст `box sudo status`.
    pub fn status_text(&self) -> String {
        let st = self.status();
        let globs = load_allow_globs(&policy_path(&st.box_name));
        let mut s = format!(
            "🔐 рут-брокер ВКЛ — контейнер {}\nобработано {} · allow {} · deny {} · rate-limited {}\nлимит {} запросов/час · таймаут {}с · кап вывода {}КБ\n",
            st.box_name,
            st.processed,
            st.allowed,
            st.denied,
            st.rate_limited,
            RATE_LIMIT_PER_HOUR,
            ROOT_EXEC_TIMEOUT_SECS,
            ROOT_OUT_CAP / 1024
        );
        if globs.is_empty() {
            s.push_str("allowlist: пуст (дефолт: apt/apt-get/dpkg + fs в границах /workspace|/home/poler; остальное — отказ)\n");
        } else {
            s.push_str(&format!(
                "allowlist ({}): {}\n",
                globs.len(),
                globs.join(", ")
            ));
        }
        if let Some(r) = st.last_reason {
            s.push_str(&format!("последний вердикт: {r}\n"));
        }
        s.push_str("модель: агент ПРОСИТ — судья решает — рут исполняется СО СТОРОНЫ ХОСТА (docker exec -u 0); агент рут не держит\n");
        s
    }
}

/// Поднять брокер для активного бокса: каналы + шимы + поток-читатель.
/// Маркер живости обновляется каждые 5с (протухает через 2 мин после краха).
pub fn spawn_broker(jail: &BoxState) -> Result<BrokerHandle, String> {
    let broker = broker_dir(&jail.home_dir);
    std::fs::create_dir_all(broker.join("responses"))
        .map_err(|e| format!("брокер-канал {}: {e}", broker.display()))?;
    // шимы: развернуть/вылечить (подмена шима привилегии не даёт)
    deploy_shims(&jail.home_dir)?;
    // маркер живости (дороже протухания: write = mtime)
    std::fs::write(
        broker.join(MARKER_FILE),
        format!("alive {}", now_epoch_ms()),
    )
    .map_err(|e| format!("маркер {}: {e}", broker.display()))?;
    // стартуем с текущего конца файла: старые неотвеченные запросы
    // (от прошлой сессии шлюза) честно игнорируются
    let start_offset = std::fs::metadata(broker.join(REQUESTS_FILE))
        .map(|m| m.len())
        .unwrap_or(0);
    if !broker.join(REQUESTS_FILE).exists() {
        std::fs::write(broker.join(REQUESTS_FILE), "").map_err(|e| format!("канал: {e}"))?;
    }

    let stop_flag = Arc::new(AtomicBool::new(false));
    let status = Arc::new(Mutex::new(BrokerStatus {
        box_name: jail.name.clone(),
        processed: 0,
        allowed: 0,
        denied: 0,
        rate_limited: 0,
        malformed: 0,
        duplicates: 0,
        last_reason: None,
    }));
    let (stop2, status2) = (stop_flag.clone(), status.clone());
    let box_name = jail.name.clone();
    let broker2 = broker.clone();
    let thread = std::thread::Builder::new()
        .name("poler-root-broker".into())
        .spawn(move || broker_loop(box_name, broker2, start_offset, stop2, status2))
        .map_err(|e| format!("поток брокера: {e}"))?;

    Ok(BrokerHandle {
        stop_flag,
        status,
        broker_dir: broker,
        _thread: Some(thread),
    })
}

/// Цикл брокера: поллинг requests.jsonl по смещению, судья, исполнение,
/// ответы, аудит, маркер живости.
fn broker_loop(
    box_name: String,
    broker: PathBuf,
    mut offset: u64,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<BrokerStatus>>,
) {
    let req_path = broker.join(REQUESTS_FILE);
    let resp_dir = broker.join("responses");
    let mut rate: VecDeque<Instant> = VecDeque::new();
    let mut seen: VecDeque<String> = VecDeque::with_capacity(512);
    let mut pending: Vec<u8> = Vec::new();
    let mut last_marker = Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(broker.join(MARKER_FILE));
            break;
        }
        // маркер живости
        if last_marker.elapsed() >= MARKER_REFRESH {
            let _ = std::fs::write(
                broker.join(MARKER_FILE),
                format!("alive {}", now_epoch_ms()),
            );
            last_marker = Instant::now();
        }
        // дочитать новые байты; нет новых — тик (частичная строка не гоняет цикл вхолостую)
        let meta_len = std::fs::metadata(&req_path).map(|m| m.len()).unwrap_or(0);
        if meta_len < offset {
            // файл усечён/ротирован агентом — перечитываем с нуля
            offset = 0;
            pending.clear();
        }
        if meta_len <= offset {
            std::thread::sleep(POLL_TICK);
            continue;
        }
        if meta_len > offset {
            if let Ok(mut f) = std::fs::File::open(&req_path) {
                use std::io::Seek as _;
                if f.seek(std::io::SeekFrom::Start(offset)).is_ok() {
                    let mut buf = Vec::new();
                    if f.read_to_end(&mut buf).is_ok() {
                        offset += buf.len() as u64;
                        pending.extend_from_slice(&buf);
                    }
                }
            }
        }
        // разобрать полные строки
        while let Some(nl) = pending.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = pending.drain(..=nl).collect();
            let line = String::from_utf8_lossy(&line[..nl]).to_string();
            if line.trim().is_empty() {
                continue;
            }
            // id известен? (для отказа мусору тоже нужен адресат)
            let maybe_id = line.split('|').next().unwrap_or("").to_string();
            let id_ok = valid_id(&maybe_id);
            // дедуп: повтор запроса с тем же id — только аудит
            if id_ok && seen.contains(&maybe_id) {
                audit_append(
                    &box_name,
                    serde_json::json!({
                        "ts": now_epoch_ms(), "id": maybe_id, "verdict": "duplicate",
                        "argv": [], "reason": "повтор id (replay)",
                    }),
                );
                if let Ok(mut st) = status.lock() {
                    st.duplicates += 1;
                }
                continue;
            }
            if id_ok {
                seen.push_back(maybe_id.clone());
                if seen.len() > 512 {
                    seen.pop_front();
                }
            }
            // rate limit
            while let Some(front) = rate.front() {
                if front.elapsed() > Duration::from_secs(3600) {
                    rate.pop_front();
                } else {
                    break;
                }
            }
            if rate.len() >= RATE_LIMIT_PER_HOUR {
                if id_ok {
                    write_response(
                        &resp_dir,
                        &maybe_id,
                        1,
                        "",
                        "",
                        "poler-sudo: лимит рут-запросов (30/час) — подождите час",
                    );
                }
                audit_append(
                    &box_name,
                    serde_json::json!({
                        "ts": now_epoch_ms(), "id": maybe_id, "verdict": "rate",
                        "argv": [], "reason": "лимит 30/час",
                    }),
                );
                if let Ok(mut st) = status.lock() {
                    st.rate_limited += 1;
                }
                continue;
            }
            rate.push_back(Instant::now());
            // разбор + судья
            let req = match parse_request_line(&line) {
                Ok(r) => r,
                Err(why) => {
                    if id_ok {
                        write_response(
                            &resp_dir,
                            &maybe_id,
                            1,
                            "",
                            "",
                            &format!("poler-sudo: мусорный запрос: {why}"),
                        );
                    }
                    audit_append(
                        &box_name,
                        serde_json::json!({
                            "ts": now_epoch_ms(), "id": maybe_id, "verdict": "malformed",
                            "argv": [], "reason": why,
                        }),
                    );
                    if let Ok(mut st) = status.lock() {
                        st.malformed += 1;
                    }
                    continue;
                }
            };
            let globs = load_allow_globs(&policy_path(&box_name));
            let t0 = Instant::now();
            let (verdict, exit, out, err, note) = match judge_root_request(&req, &globs) {
                RootVerdict::Deny(why) => (Some(why), 1, String::new(), String::new(), "deny"),
                RootVerdict::Allow => {
                    let argv = root_exec_argv(&box_name, &req);
                    let (exit, out, err, timed_out, spawn_err) = exec_root(&argv);
                    let note = if timed_out {
                        "allow/timeout"
                    } else if spawn_err.is_some() {
                        "allow/spawn-error"
                    } else {
                        "allow"
                    };
                    let se = spawn_err.unwrap_or_default();
                    (Some(se), exit, out, err, note)
                }
            };
            let ms = t0.elapsed().as_millis() as u64;
            let reason = verdict.clone().unwrap_or_default();
            // ответ агенту
            match verdict.as_deref() {
                Some(why) if note == "deny" => {
                    write_response(
                        &resp_dir,
                        &req.id,
                        1,
                        "",
                        "",
                        &format!("poler-sudo: судья: {why}"),
                    );
                }
                _ => {
                    let out_c = cap_str(&out, ROOT_OUT_CAP);
                    let err_c = cap_str(&err, ROOT_ERR_CAP);
                    let mut msg = String::new();
                    if timed_note(note) {
                        msg.push_str(
                            "poler-sudo: исполнено от рута (docker exec -u 0, со стороны хоста)\n",
                        );
                    }
                    if note == "allow/timeout" {
                        msg.push_str("poler-sudo: таймаут 120с — процесс убит\n");
                    }
                    if note == "allow/spawn-error" {
                        msg.push_str(&format!("poler-sudo: docker exec не поднялся: {reason}\n"));
                    }
                    write_response(&resp_dir, &req.id, exit, &out_c, &err_c, &msg);
                }
            }
            // аудит + счётчики
            audit_append(
                &box_name,
                serde_json::json!({
                    "ts": now_epoch_ms(), "id": req.id, "argv": req.argv,
                    "cwd": req.cwd, "verdict": note, "reason": reason,
                    "exit": exit, "ms": ms,
                }),
            );
            if let Ok(mut st) = status.lock() {
                st.processed += 1;
                match note {
                    "deny" => {
                        st.denied += 1;
                        st.last_reason = Some(format!("deny: {}", req.argv.join(" ")));
                    }
                    "rate" => st.rate_limited += 1,
                    "malformed" => st.malformed += 1,
                    _ => {
                        st.allowed += 1;
                        st.last_reason =
                            Some(format!("allow: {} (exit {})", req.argv.join(" "), exit));
                    }
                }
            }
        }
        if pending.is_empty() {
            std::thread::sleep(POLL_TICK);
        }
    }
}

fn timed_note(note: &str) -> bool {
    note == "allow" || note == "allow/timeout" || note == "allow/spawn-error"
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn req(argv: &[&str]) -> SudoRequest {
        SudoRequest {
            id: "r123-1700000000".into(),
            cwd: "/workspace".into(),
            argv: argv.iter().map(|s| s.to_string()).collect(),
        }
    }

    // --- base64 ---
    #[test]
    fn b64_roundtrip() {
        for s in ["", "a", "ab", "abc", "hello мир!", "sudo\trm -rf /\nx"] {
            let enc = b64_encode(s.as_bytes());
            assert_eq!(b64_decode(&enc).unwrap(), s.as_bytes(), "roundtrip: {s:?}");
        }
        assert_eq!(b64_encode(b"abc"), "YWJj");
        assert_eq!(b64_encode(b"ab"), "YWI=");
        assert_eq!(b64_encode(b"a"), "YQ==");
    }

    #[test]
    fn b64_decode_rejects_garbage() {
        assert!(b64_decode("YQ=").is_err(), "длина не кратна 4");
        assert!(b64_decode("Y*==").is_err(), "символ вне алфавита");
        assert!(b64_decode("Y=Q=").is_err(), "= в середине");
        assert!(b64_decode("Y===").is_err(), "паддинг > 2");
    }

    // --- протокол ---
    #[test]
    fn request_line_roundtrip() {
        let line = encode_request(
            "r42-1700000001",
            "/workspace",
            &["apt-get".into(), "install".into(), "-y".into(), "sl".into()],
        );
        let r = parse_request_line(&line).unwrap();
        assert_eq!(r.id, "r42-1700000001");
        assert_eq!(r.cwd, "/workspace");
        assert_eq!(r.argv, vec!["apt-get", "install", "-y", "sl"]);
    }

    #[test]
    fn request_line_rejects_traversal_and_garbage() {
        // id с «/» — path traversal в имени файла ответа
        let bad_id = encode_request("../evil", "/x", &["id".into()]);
        assert!(
            parse_request_line(&bad_id).is_err(),
            "id-траверсал обязан отказывать"
        );
        assert!(parse_request_line("").is_err());
        assert!(parse_request_line("onlyfield").is_err());
        assert!(
            parse_request_line("id|notb64!|YQ==").is_err(),
            "битый base64 cwd"
        );
        // перегруз аргументов
        let many: Vec<String> = (0..80).map(|i| format!("a{i}")).collect();
        let line = encode_request("ok-id", "/w", &many);
        assert!(parse_request_line(&line).is_err(), ">64 аргументов — отказ");
        // пустой id
        assert!(parse_request_line(&encode_request("", "/w", &["id".into()])).is_err());
    }

    // --- судья ---
    #[test]
    fn judge_allows_package_managers() {
        for argv in [
            vec!["apt-get", "install", "-y", "sl"],
            vec!["apt", "update"],
            vec!["dpkg", "-i", "/workspace/pkg.deb"],
            vec!["apt-get", "download", "htop"],
        ] {
            let v = judge_root_request(&req(&argv), &[]);
            assert_eq!(v, RootVerdict::Allow, "дефолт-allow: {argv:?}");
        }
    }

    #[test]
    fn judge_blocks_destructive_invariant() {
        // Block судьи НЕ ослабляется даже allowlist'ом
        for globs in [vec![], vec!["rm *".into()], vec!["*".into()]] {
            let v = judge_root_request(&req(&["rm", "-rf", "/usr"]), &globs);
            assert!(
                matches!(v, RootVerdict::Deny(_)),
                "Block-инвариант: {globs:?}"
            );
            let v = judge_root_request(&req(&["sh", "-c", "mkfs.ext4 /dev/sda"]), &globs);
            assert!(
                matches!(v, RootVerdict::Deny(_)),
                "интерп-деструктив: {globs:?}"
            );
        }
    }

    #[test]
    fn judge_confirm_is_deny_zse() {
        // sudo в payload → судья Confirm → брокер не подтверждает молча
        let v = judge_root_request(&req(&["sudo", "id"]), &[]);
        match v {
            RootVerdict::Deny(why) => assert!(why.contains("подтвержд"), "ZSE-причина: {why}"),
            other => panic!("sudo-payload должен быть Deny: {other:?}"),
        }
    }

    #[test]
    fn judge_denies_escape_tools() {
        for argv in [
            vec!["docker", "ps"],
            vec!["nsenter", "--target", "1", "--mount", "--uts"],
            vec!["unshare", "-Ur", "id"],
            vec!["mount", "/dev/sda", "/mnt"],
            vec!["reboot"],
            vec!["modprobe", "evil"],
            vec!["ptrace", "-p", "1"],
        ] {
            let v = judge_root_request(&req(&argv), &[]);
            assert!(
                matches!(v, RootVerdict::Deny(_)),
                "инструмент побега: {argv:?}"
            );
        }
    }

    #[test]
    fn judge_denies_kernel_paths() {
        for argv in [
            vec!["chmod", "777", "/proc/self/mem"],
            vec!["sh", "-c", "echo x > /proc/sys/kernel/core_pattern"], // судья BLOCKED_WRITE → Block
            vec!["cp", "x", "/sys/fs/cgroup/thing"],
            vec!["tee", "/dev/kmem"],
            vec!["cat", "/var/run/docker.sock"],
        ] {
            let v = judge_root_request(&req(&argv), &[]);
            assert!(
                matches!(v, RootVerdict::Deny(_)),
                "пути ядра/демона: {argv:?}"
            );
        }
    }

    #[test]
    fn judge_allowlist_glob() {
        let globs = vec!["cargo install*".to_string()];
        assert_eq!(
            judge_root_request(&req(&["cargo", "install", "ripgrep"]), &globs),
            RootVerdict::Allow
        );
        // без глоба — отказ по умолчанию
        assert!(matches!(
            judge_root_request(&req(&["cargo", "install", "ripgrep"]), &[]),
            RootVerdict::Deny(_)
        ));
        // глоб не совпал
        assert!(matches!(
            judge_root_request(&req(&["cargo", "build"]), &globs),
            RootVerdict::Deny(_)
        ));
    }

    #[test]
    fn judge_fs_ops_scope() {
        assert_eq!(
            judge_root_request(&req(&["mkdir", "/workspace/build"]), &[]),
            RootVerdict::Allow
        );
        assert_eq!(
            judge_root_request(&req(&["chown", "root:root", "/home/poler/x"]), &[]),
            RootVerdict::Allow
        );
        assert!(
            matches!(
                judge_root_request(&req(&["mkdir", "/etc/pwn"]), &[]),
                RootVerdict::Deny(_)
            ),
            "fs-опа вне границ — отказ"
        );
        // rm не входит в fs-дефолт (только allowlist)
        assert!(matches!(
            judge_root_request(&req(&["rm", "/workspace/x"]), &[]),
            RootVerdict::Deny(_)
        ));
    }

    #[test]
    fn judge_safe_and_empty() {
        assert_eq!(judge_root_request(&req(&["id"]), &[]), RootVerdict::Allow);
        assert_eq!(
            judge_root_request(&req(&["whoami"]), &[]),
            RootVerdict::Allow
        );
        assert!(matches!(
            judge_root_request(&req(&[]), &[]),
            RootVerdict::Deny(_)
        ));
        // пакый саб
        assert!(matches!(
            judge_root_request(&req(&["apt-get", "moo"]), &[]),
            RootVerdict::Deny(_)
        ));
    }

    #[test]
    fn glob_match_cases() {
        assert!(glob_match("*", "всё что угодно"));
        assert!(glob_match("apt*", "apt-get install x"));
        assert!(glob_match("cargo install*", "cargo install ripgrep"));
        assert!(!glob_match("cargo install*", "cargo build"));
        assert!(glob_match("te?t", "test"));
        assert!(!glob_match("te?t", "tempt"));
        assert!(glob_match("a*b*c", "a-x-b-y-c"));
        assert!(!glob_match("a*b*c", "a-b"));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    // --- политика ---
    #[test]
    fn policy_add_list_reset() {
        let dir = std::env::temp_dir().join(format!("poler-pol-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("box.sudo");
        assert!(load_allow_globs(&path).is_empty());
        add_allow_glob(&path, "cargo *").unwrap();
        add_allow_glob(&path, "make *").unwrap();
        assert!(add_allow_glob(&path, "cargo *")
            .unwrap()
            .contains("уже в списке"));
        assert_eq!(load_allow_globs(&path), vec!["cargo *", "make *"]);
        assert!(add_allow_glob(&path, "").is_err());
        assert!(add_allow_glob(&path, "a\nb").is_err());
        assert!(add_allow_glob(&path, &"x".repeat(200)).is_err());
        reset_allow_globs(&path).unwrap();
        assert!(load_allow_globs(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- шимы ---
    #[test]
    fn deploy_shims_creates_executable_scripts() {
        let dir = std::env::temp_dir().join(format!("poler-shim-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        deploy_shims(&dir).unwrap();
        let sudo = dir.join(".poler-bin/sudo");
        assert!(sudo.is_file());
        let text = std::fs::read_to_string(&sudo).unwrap();
        assert!(text.contains(".poler-broker"), "шим пишет в брокер-канал");
        assert!(text.contains("docker exec"), "документация модели в шиме");
        assert!(text.contains("base64"), "протокол base64");
        assert!(
            text.contains("-mmin -2"),
            "маркер живости с freshness-чеком"
        );
        assert!(dir.join(".poler-broker/responses").is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&sudo).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "исполняемый: {:o}", mode);
        }
        assert!(dir.join(".poler-bin/poler-sudo").is_file());
        // идемпотентность
        deploy_shims(&dir).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn root_exec_argv_shape() {
        let _g = super::super::containers::docker_env_test_lock();
        let argv = root_exec_argv("poler-box-abcd", &req(&["apt-get", "update"]));
        assert_eq!(
            &argv[1..5],
            &["exec".to_string(), "-u".into(), "0:0".into(), "-w".into()]
        );
        assert_eq!(argv[5], "/workspace");
        assert_eq!(argv[6], "poler-box-abcd");
        assert_eq!(&argv[7..], &["apt-get".to_string(), "update".into()]);
        // cwd с «..» → якорь /workspace
        let bad = SudoRequest {
            id: "x".into(),
            cwd: "/workspace/../../etc".into(),
            argv: vec!["id".into()],
        };
        assert_eq!(root_exec_argv("b", &bad)[5], "/workspace");
        std::env::set_var("POLER_BOX_DOCKER", "/bin/false");
        assert_eq!(root_exec_argv("b", &req(&["id"]))[0], "/bin/false");
        std::env::remove_var("POLER_BOX_DOCKER");
    }

    #[test]
    fn write_response_is_symlink_safe() {
        let dir = std::env::temp_dir().join(format!("poler-resp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let resp = dir.join("responses");
        std::fs::create_dir_all(&resp).unwrap();
        // агент подготовил симлинк-ловушку на месте ответа
        #[cfg(unix)]
        std::os::unix::fs::symlink("/tmp/poler-would-be-victim", resp.join("r1.exit")).unwrap();
        write_response_file(&resp, "r1.exit", b"7\n").unwrap();
        let meta = std::fs::symlink_metadata(resp.join("r1.exit")).unwrap();
        assert!(meta.is_file(), "после записи — обычный файл, не симлинк");
        assert_eq!(
            std::fs::read_to_string(resp.join("r1.exit")).unwrap(),
            "7\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- живой брокер с фейковым docker (герметичный интеграционный) ---
    #[test]
    fn broker_end_to_end_allow_and_deny() {
        let _g = super::super::containers::docker_env_test_lock();
        let base = std::env::temp_dir().join(format!("poler-broker-e2e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let home = base.join("home");
        let audit_dir = base.join("audit");
        let policy_dir = base.join("policy");
        std::fs::create_dir_all(&home).unwrap();
        // фейковый docker: логирует argv, пишет stdout/stderr, exit 7
        let fake = base.join("fake-docker.sh");
        let log = base.join("docker-calls.log");
        std::fs::write(
            &fake,
            format!(
                "#!/bin/sh\necho \"$@\" >> {0}\necho FAKE-ROOT-OUT\necho FAKE-ROOT-ERR >&2\nexit 7\n",
                log.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755));
        }
        let jail = BoxState {
            name: "poler-box-e2e".into(),
            cfg: super::super::containers::BoxConfig::default(),
            ws_root: base.join("ws"),
            home_dir: home.clone(),
        };
        std::env::set_var("POLER_BOX_DOCKER", fake.to_str().unwrap());
        std::env::set_var("POLER_AUDIT_HOME", audit_dir.to_str().unwrap());
        std::env::set_var("POLER_POLICY_HOME", policy_dir.to_str().unwrap());
        let mut handle = spawn_broker(&jail).unwrap();
        // маркер живости появился
        assert!(broker_dir(&home).join(MARKER_FILE).is_file());

        // 1) разрешённый запрос: apt-get install
        let ok_line = encode_request(
            "r1-1",
            "/workspace",
            &["apt-get".into(), "install".into(), "-y".into(), "sl".into()],
        );
        std::fs::write(
            broker_dir(&home).join(REQUESTS_FILE),
            format!("{ok_line}\n"),
        )
        .unwrap();
        // 2) деструктив: rm -rf /
        let bad_line = encode_request(
            "r2-2",
            "/workspace",
            &["rm".into(), "-rf".into(), "/".into()],
        );
        // файл перезапишется — ждём обработки первого, потом второй
        let resp = broker_dir(&home).join("responses");
        let mut waited = 0;
        while !resp.join("r1-1.exit").exists() && waited < 100 {
            std::thread::sleep(Duration::from_millis(100));
            waited += 1;
        }
        assert!(resp.join("r1-1.exit").exists(), "allow-ответ пришёл");
        assert_eq!(
            std::fs::read_to_string(resp.join("r1-1.exit"))
                .unwrap()
                .trim(),
            "7"
        );
        assert!(std::fs::read_to_string(resp.join("r1-1.out"))
            .unwrap()
            .contains("FAKE-ROOT-OUT"));
        assert!(std::fs::read_to_string(resp.join("r1-1.err"))
            .unwrap()
            .contains("FAKE-ROOT-ERR"));
        // фейковый docker получил -u 0:0 и argv насквозь
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("-u 0:0"), "рут-флаг: {calls}");
        assert!(calls.contains("poler-box-e2e"));
        assert!(
            calls.contains("apt-get install -y sl"),
            "argv насквозь: {calls}"
        );

        // второй запрос (файл перезаписан заново — offset растёт)
        std::fs::write(
            broker_dir(&home).join(REQUESTS_FILE),
            format!("{ok_line}\n{bad_line}\n"),
        )
        .unwrap();
        let mut waited = 0;
        while !resp.join("r2-2.exit").exists() && waited < 100 {
            std::thread::sleep(Duration::from_millis(100));
            waited += 1;
        }
        let deny_exit = std::fs::read_to_string(resp.join("r2-2.exit"))
            .unwrap()
            .trim()
            .to_string();
        assert_eq!(deny_exit, "1", "deny → exit 1");
        let msg = std::fs::read_to_string(resp.join("r2-2.msg")).unwrap();
        assert!(msg.contains("судья"), "причина судьи: {msg}");

        // статус
        let st = handle.status();
        assert_eq!(st.allowed, 1);
        assert_eq!(st.denied, 1);
        // аудит
        let audit = std::fs::read_to_string(audit_path("poler-box-e2e")).unwrap();
        assert!(audit.contains("\"verdict\":\"allow\""));
        assert!(audit.contains("\"verdict\":\"deny\""));
        // остановка: маркер снят
        let rep = handle.stop();
        assert!(rep.contains("ОСТАНОВЛЕН"));
        assert!(!broker_dir(&home).join(MARKER_FILE).is_file());
        // replay того же id после остановки не обрабатывается
        std::env::remove_var("POLER_BOX_DOCKER");
        std::env::remove_var("POLER_AUDIT_HOME");
        std::env::remove_var("POLER_POLICY_HOME");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn tail_audit_empty_and_filled() {
        let dir = std::env::temp_dir().join(format!("poler-audit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("POLER_AUDIT_HOME", dir.to_str().unwrap());
        let empty = tail_audit("poler-box-none", 10);
        assert!(empty.contains("пуст"));
        audit_append(
            "poler-box-x",
            serde_json::json!({"ts": 1, "argv": ["apt-get", "update"], "verdict": "allow", "reason": "", "exit": 0}),
        );
        audit_append(
            "poler-box-x",
            serde_json::json!({"ts": 2, "argv": ["rm", "-rf", "/"], "verdict": "deny", "reason": "судья: BLOCK", "exit": 1}),
        );
        let text = tail_audit("poler-box-x", 10);
        assert!(text.contains("apt-get update"));
        assert!(text.contains("[deny]"));
        std::env::remove_var("POLER_AUDIT_HOME");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn shim_text_is_posix_clean() {
        // без башизмов (клетка = debian slim: /bin/sh → dash)
        assert!(!SUDO_SHIM.contains("function "));
        assert!(!SUDO_SHIM.contains("[[ "));
        assert!(SUDO_SHIM.contains("#!/bin/sh"));
        // id формируется без $RANDOM (dash)
        assert!(SUDO_SHIM.contains("$(date +%s)"));
    }
}
