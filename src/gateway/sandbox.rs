//! # Terminal Gateway: Sandbox OS Subshell — политика безопасности (v0.22.0)
//!
//! Классификация хостовых команд ДО исполнения. Три вердикта:
//!
//! - **Block** — деструктивное (rm -rf /, форк-бомбы, dd of=/dev/…,
//!   shutdown-семейство, запись в блочные устройства и системные каталоги,
//!   `curl | sh`) — отказ сразу, без вопросов;
//! - **Confirm** — опасное, но легитимное (sudo/su — эскалация прав;
//!   рекурсивный rm по несистемным путям; dd без /dev-цели) — явный
//!   yes/no запрос;
//! - **Allow** — всё остальное.
//!
//! Threat model честная (docs/terminal-gateway-architecture.md §4.2):
//! это политический userspace-фильтр от ОШИБОК ПОЛЬЗОВАТЕЛЯ, а не ядровая
//! песочница от малвари. Разбор строки делает gateway (не `/bin/sh`),
//! поэтому классификация покрывает всё, что реально будет исполнено.

use super::pipeline::Pipeline;

// ---------------------------------------------------------------------------
// Вердикты
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Policy {
    /// Исполнять.
    Allow,
    /// Спросить пользователя (reason — что именно опасно).
    Confirm(String),
    /// Отказ (reason — что и почему заблокировано).
    Block(String),
}

impl Policy {
    pub fn is_allow(&self) -> bool {
        matches!(self, Policy::Allow)
    }
}

// ---------------------------------------------------------------------------
// Системные корни (блок-лист для рекурсивных операций и записи)
// ---------------------------------------------------------------------------

/// Каталоги, рекурсивное удаление/перезапись которых — катастрофа.
const SYSTEM_ROOTS: &[&str] = &[
    "/", "/usr", "/etc", "/bin", "/sbin", "/lib", "/lib64", "/libx32", "/boot", "/sys", "/proc",
    "/dev", "/run", "/var", "/opt", "/home", "/Users", "/private",
];

/// Редирект/запись сюда — Block (системная целостность).
const BLOCKED_WRITE_ROOTS: &[&str] = &[
    "/etc", "/boot", "/sys", "/proc", "/usr", "/bin", "/sbin", "/lib", "/lib64",
];

/// Команды выключения/уровня исполнения — Block всегда.
const SHUTDOWN_CMDS: &[&str] = &["shutdown", "reboot", "halt", "poweroff", "telinit", "killall5"];

/// Утилиты разметки диска — Block всегда (mkfs.* — по префиксу).
const DISK_CMDS: &[&str] = &["wipefs", "fdisk", "sfdisk", "parted", "blockdev"];

/// Эскалация прав — Confirm (внутри может быть Block — см. judge_privilege).
const PRIVILEGE_CMDS: &[&str] = &["sudo", "su", "doas", "pkexec", "sudoedit", "visudo"];

// ---------------------------------------------------------------------------
// Анализ
// ---------------------------------------------------------------------------

/// Полный вердикт по конвейеру: все хостовые сегменты + цель редиректа.
/// Сегменты движка безопасны by construction (это наши функции).
pub fn judge_pipeline(pipeline: &Pipeline, host_segments: &[usize]) -> Policy {
    // 1) сырой скан всей строки: форк-бомбы и замаскированные паттерны
    let raw_all = pipeline
        .segments
        .iter()
        .map(|s| s.raw.as_str())
        .collect::<Vec<_>>()
        .join(" | ");
    if let Some(b) = raw_danger_scan(&raw_all) {
        return b;
    }
    // 2) remote-exec: curl/wget в конвейере с sh/bash в хвосте
    if let Some(b) = remote_exec_check(pipeline, host_segments) {
        return b;
    }
    // 3) по сегментам
    for &idx in host_segments {
        let seg = &pipeline.segments[idx];
        match judge_segment(&seg.tokens) {
            Policy::Allow => continue,
            other => return other,
        }
    }
    // 4) цель редиректа
    if let Some(r) = &pipeline.redirect {
        match judge_redirect_target(&r.path) {
            Policy::Allow => {}
            other => return other,
        }
    }
    Policy::Allow
}

/// Вердикт по одному хостовому сегменту.
pub fn judge_segment(tokens: &[String]) -> Policy {
    let Some(first) = tokens.first() else {
        return Policy::Allow;
    };
    // basename: /usr/bin/rm → rm (запуск по абсолютному пути — тот же анализ)
    let cmd = first.rsplit('/').next().unwrap_or(first);

    // «:» — нет легитимных применений, классика форк-бомб
    if cmd == ":" {
        return Policy::Block("форк-бомба (команда «:» не имеет легитимных применений)".into());
    }

    if SHUTDOWN_CMDS.contains(&cmd) {
        return Policy::Block(format!(
            "команда {cmd} выключает/перезагружает машину — вне полномочий gateway"
        ));
    }
    if cmd.starts_with("mkfs") || DISK_CMDS.contains(&cmd) {
        return Policy::Block(format!("команда {cmd} работает с дисковой разметкой"));
    }
    if cmd == "dd" {
        // dd of=/dev/… → Block; прочий dd → Confirm
        for a in &tokens[1..] {
            if let Some(v) = a.strip_prefix("of=") {
                if v.starts_with("/dev/") {
                    return Policy::Block(format!("dd пишет напрямую в устройство {v}"));
                }
            }
        }
        return Policy::Confirm(
            "dd — низкоуровневое копирование; ошибка в аргументах необратима".into(),
        );
    }
    if PRIVILEGE_CMDS.contains(&cmd) {
        return judge_privilege(cmd, tokens);
    }
    if cmd == "rm" {
        return judge_rm(tokens);
    }
    if cmd == "chmod" || cmd == "chown" || cmd == "chgrp" {
        return judge_recursive_fs(tokens, cmd);
    }
    if cmd == "mv" || cmd == "cp" {
        // цель в /dev, /sys, /proc — порча ядра-интерфейсов
        for a in &tokens[1..] {
            if !a.starts_with('-')
                && (a == "/dev" || a == "/sys" || a == "/proc"
                    || a.starts_with("/dev/") || a.starts_with("/sys/") || a.starts_with("/proc/"))
            {
                return Policy::Block(format!("{cmd} с целью в системный виртуальный каталог {a}"));
            }
        }
        return Policy::Allow;
    }
    if cmd == "tee" {
        // tee пишет файлы БЕЗ оператора `>` — применяем те же правила
        for a in &tokens[1..] {
            if !a.starts_with('-') {
                match judge_redirect_target(a) {
                    Policy::Allow => continue,
                    other => return other,
                }
            }
        }
        return Policy::Allow;
    }
    // sh -c '…' / bash -c '…' — полезаем внутрь (один уровень рекурсии):
    // payload токенизируется и разбирается как команда — ловит
    // «rm -rf /usr» и «tee /etc/x»; сырой скан — бомбы/дд/редиректы
    if (cmd == "sh" || cmd == "bash" || cmd == "zsh" || cmd == "dash") && tokens.len() >= 3 {
        let is_c = tokens[1] == "-c" || tokens[1] == "--command";
        if is_c {
            let payload = tokens[2..].join(" ");
            if let Some(b) = raw_danger_scan(&payload) {
                return b;
            }
            let inner_tokens: Vec<String> = payload
                .split_whitespace()
                .map(|s| s.to_string())
                .collect();
            if !inner_tokens.is_empty() {
                match judge_segment(&inner_tokens) {
                    Policy::Allow => {}
                    other => return other,
                }
            }
        }
    }
    Policy::Allow
}

/// sudo/su/doas/pkexec: эскалация всегда требует подтверждения, НО если
/// внутри сидит Block-паттерн (`sudo rm -r /usr`, `sudo tee /etc/passwd`)
/// — блокируем без вопросов.
fn judge_privilege(cmd: &str, tokens: &[String]) -> Policy {
    let inner = &tokens[1..];
    // пропускаем ФЛАГИ САМОГО sudo (-u user, -p prompt, -g group, -C)
    // до первого слова-команды; дальше аргументы идут нетронутыми —
    // иначе мы бы срезали флаги внутренней команды (sudo rm -r …)
    let mut skip_next = false;
    let mut cmd_start = inner.len();
    for (idx, a) in inner.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "-u" || a == "--user" || a == "-p" || a == "-g" || a == "-C" {
            skip_next = true;
            continue;
        }
        if a.starts_with('-') {
            continue;
        }
        cmd_start = idx;
        break;
    }
    if cmd_start < inner.len() {
        let inner_tokens: Vec<String> = inner[cmd_start..].to_vec();
        if let Policy::Block(why) = judge_segment(&inner_tokens) {
            return Policy::Block(format!("под {cmd}: {why}"));
        }
    }
    Policy::Confirm(format!("эскалация прав: {cmd} … — подтвердите осознанно"))
}

/// `rm` с рекурсией: системные корни и cwd — Block; прочее рекурсивное — Confirm.
fn judge_rm(tokens: &[String]) -> Policy {
    let recursive = tokens[1..]
        .iter()
        .any(|a| a == "-r" || a == "-R" || a == "--recursive" || a.starts_with("-r") || a.starts_with("-R"));
    let targets: Vec<&String> = tokens[1..].iter().filter(|a| !a.starts_with('-')).collect();
    if !recursive {
        return Policy::Allow; // rm файла — обычный workflow (без -r катастрофы не будет)
    }
    if targets.is_empty() {
        return Policy::Confirm("rm -r без цели (возможно, ждёт stdin) — подтверждаете?".into());
    }
    for t in &targets {
        let t = t.as_str();
        let expanded = expand_home(t);
        if is_system_root(&expanded) || is_whole_home_dir(&expanded) {
            return Policy::Block(format!(
                "rm -r по системному/корневому пути «{t}» — катастрофическая потеря данных"
            ));
        }
        if t == "~" || t == "$HOME" {
            return Policy::Block("rm -r по всему домашнему каталогу".into());
        }
        if t == "." || t == ".." {
            return Policy::Block(
                "rm -r по текущему каталогу (.) — сами под собой рубите сук".into(),
            );
        }
    }
    let list = targets
        .iter()
        .map(|s| s.as_str())
        .take(3)
        .collect::<Vec<_>>()
        .join(", ");
    Policy::Confirm(format!("рекурсивное удаление: {list}"))
}

/// chmod/chown/chgrp -R: системные корни — Block; абсолютный путь вне HOME — Confirm.
fn judge_recursive_fs(tokens: &[String], cmd: &str) -> Policy {
    let recursive = tokens[1..].iter().any(|a| a == "-R" || a == "-r" || a == "--recursive");
    if !recursive {
        return Policy::Allow;
    }
    for a in &tokens[1..] {
        if a.starts_with('-') {
            continue;
        }
        let expanded = expand_home(a);
        if is_system_root(&expanded) || is_whole_home_dir(&expanded) {
            return Policy::Block(format!("{cmd} -R по системному пути «{a}»"));
        }
        if expanded == home_prefix() {
            return Policy::Confirm(format!("{cmd} -R по всему домашнему каталогу"));
        }
        if expanded.starts_with('/') && !expanded.starts_with(&home_prefix()) {
            return Policy::Confirm(format!("{cmd} -R по абсолютному пути вне HOME: {a}"));
        }
    }
    Policy::Allow
}

/// Цель редиректа: устройства и системные каталоги — Block.
pub fn judge_redirect_target(path: &str) -> Policy {
    let p = expand_home(path);
    if p.starts_with("/dev/") {
        let safe = ["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty", "/dev/zero"];
        if safe.contains(&p.as_str()) {
            return Policy::Allow;
        }
        return Policy::Block(format!(
            "редирект в устройство {p} (блочное/символьное — данные на носителе)"
        ));
    }
    if BLOCKED_WRITE_ROOTS.iter().any(|r| p == *r || p.starts_with(&format!("{r}/"))) {
        return Policy::Block(format!("редирект в системный каталог: {p}"));
    }
    Policy::Allow
}

/// `curl … | sh` / `wget … | bash` — исполнение удалённого кода.
fn remote_exec_check(pipeline: &Pipeline, host_segments: &[usize]) -> Option<Policy> {
    let has_fetcher = host_segments.iter().any(|&i| {
        let c = pipeline.segments[i].cmd().rsplit('/').next().unwrap_or("");
        c == "curl" || c == "wget"
    });
    if !has_fetcher {
        return None;
    }
    let has_shell = host_segments.iter().any(|&i| {
        let seg = &pipeline.segments[i];
        let c = seg.cmd().rsplit('/').next().unwrap_or("");
        (c == "sh" || c == "bash" || c == "zsh" || c == "dash") && i > 0
    });
    if has_shell {
        return Some(Policy::Block(
            "конвейер «скачать → исполнить» (curl/wget | sh): исполнение удалённого кода".into(),
        ));
    }
    None
}

/// Сырой скан на замаскированные паттерны (в любом месте строки, включая
/// кавычки и аргументы sh -c). Пробелы выбрасываются — «rm -r -f /» и
/// «rm -rf /» дают одинаковую норм-форму. RM-паттерны ловим ТОЛЬКО на
/// точный корень (хвост норм-формы) — глубокие пути разбирает токен-анализ
/// (judge_rm различает системные и пользовательские цели).
fn raw_danger_scan(text: &str) -> Option<Policy> {
    let norm: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    // форк-бомба: «:(){»
    if norm.contains(":(){") {
        return Some(Policy::Block(
            "форк-бомба (классический «:(){ :|:& };:» или вариант)".into(),
        ));
    }
    // rm ровно по корню (возможно с sudo/ш-обёрткой): норм-форма
    // заканчивается на «rm-rf/» (или вариант); «rm-rf/*» — wildcard-корень
    let rm_tail = ["rm-rf/", "rm-fr/", "rm-r-f/", "rm-f-r/"];
    if rm_tail.iter().any(|p| norm.ends_with(p)) || norm.contains("rm-rf/*") {
        return Some(Policy::Block("rm по корню файловой системы (rm -rf /)".into()));
    }
    if norm.contains("ddof=/dev/") {
        return Some(Policy::Block("dd пишет напрямую в /dev-устройство".into()));
    }
    // редирект в системные каталоги/устройства внутри sh -c строк
    let bad_redirect = [">/etc/", ">/boot/", ">/sys/", ">/proc/", ">/dev/sd", ">/dev/nvme", ">/dev/disk"];
    if bad_redirect.iter().any(|p| norm.contains(p)) {
        return Some(Policy::Block(
            "перенаправление вывода в системный каталог/устройство".into(),
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// Утилиты
// ---------------------------------------------------------------------------

fn home_prefix() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/home".into())
}

/// `~/x` → `/home/user/x`; прочее без изменений. Хвостовые `/` срезаются
/// (кроме корня "/") — «~/» и «~» означают одно и то же.
pub fn expand_home(p: &str) -> String {
    let out = if p == "~" {
        home_prefix()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("{}/{}", home_prefix(), rest)
    } else {
        p.to_string()
    };
    if out.len() > 1 {
        out.trim_end_matches('/').to_string()
    } else {
        out
    }
}

/// Путь — системный корень (точное совпадение или вложенный в него).
/// Мульти-юзерные корни (/home, /Users) исключены из префикс-матчинга —
/// их содержимое разбирает is_whole_home_dir (пользовательские данные).
fn is_system_root(p: &str) -> bool {
    if p == "/" {
        return true;
    }
    let multi_user = ["/home", "/Users"];
    SYSTEM_ROOTS
        .iter()
        .filter(|r| !multi_user.contains(r))
        .any(|r| *r == p || p.starts_with(&format!("{r}/")))
}

/// Целый пользовательский каталог (/home целиком или /home/alice без
/// вложенности) — Block при рекурсивных операциях; глубже — данные
/// пользователя, Confirm.
fn is_whole_home_dir(p: &str) -> bool {
    for root in ["/home", "/Users"] {
        if p == root {
            return true;
        }
        if let Some(rest) = p.strip_prefix(&format!("{root}/")) {
            if !rest.trim_end_matches('/').contains('/') {
                return true;
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Тесты — security-critical код
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::pipeline::parse_line;

    fn policy_of(line: &str) -> Policy {
        let p = parse_line(line).unwrap();
        // все сегменты считаем хостовыми (сегменты движка безопасны)
        let host: Vec<usize> = (0..p.segments.len()).collect();
        judge_pipeline(&p, &host)
    }

    // ---------- Block: rm ----------

    #[test]
    fn block_rm_rf_root() {
        assert!(matches!(policy_of("rm -rf /"), Policy::Block(_)));
        assert!(matches!(policy_of("rm -fr /"), Policy::Block(_)));
        assert!(matches!(policy_of("rm -r -f /"), Policy::Block(_)));
        assert!(matches!(policy_of("sudo rm -rf /"), Policy::Block(_)));
        assert!(matches!(policy_of("sudo rm -r /usr"), Policy::Block(_)));
    }

    #[test]
    fn block_rm_rf_system_dirs() {
        assert!(matches!(policy_of("rm -rf /usr"), Policy::Block(_)));
        assert!(matches!(policy_of("rm -rf /etc/nginx"), Policy::Block(_)));
        assert!(matches!(policy_of("rm -r /home"), Policy::Block(_)));
        assert!(matches!(policy_of("rm -rf ~"), Policy::Block(_)));
        assert!(matches!(policy_of("rm -rf ~/"), Policy::Block(_)));
        // целый пользовательский каталог (даже свой) — Block
        assert!(matches!(policy_of("rm -rf /home/alice"), Policy::Block(_)));
        assert!(matches!(policy_of("rm -rf /home/alice/"), Policy::Block(_)));
    }

    #[test]
    fn confirm_rm_own_subdir_absolute() {
        // абсолютный путь вглубь своей домашней директории — Confirm, не Block
        let p = policy_of("rm -rf /home/tester/build");
        assert!(matches!(p, Policy::Confirm(_)), "got: {p:?}");
    }

    #[test]
    fn block_rm_rf_cwd() {
        assert!(matches!(policy_of("rm -rf ."), Policy::Block(_)));
        assert!(matches!(policy_of("rm -r .."), Policy::Block(_)));
    }

    // ---------- Block: прочее деструктивное ----------

    #[test]
    fn block_fork_bomb() {
        assert!(matches!(policy_of(":(){ :|:& };:"), Policy::Block(_)));
        assert!(matches!(policy_of("bash -c ':(){ :|:& };:'"), Policy::Block(_)));
    }

    #[test]
    fn block_dd_to_device() {
        assert!(matches!(policy_of("dd if=/dev/zero of=/dev/sda"), Policy::Block(_)));
        assert!(matches!(policy_of("dd of=/dev/nvme0n1"), Policy::Block(_)));
        assert!(matches!(policy_of("bash -c 'dd of=/dev/sda'"), Policy::Block(_)));
    }

    #[test]
    fn block_shutdown_family() {
        assert!(matches!(policy_of("shutdown -h now"), Policy::Block(_)));
        assert!(matches!(policy_of("reboot"), Policy::Block(_)));
        assert!(matches!(policy_of("poweroff"), Policy::Block(_)));
    }

    #[test]
    fn block_mkfs() {
        assert!(matches!(policy_of("mkfs.ext4 /dev/sdb1"), Policy::Block(_)));
        assert!(matches!(policy_of("wipefs -a /dev/sdb"), Policy::Block(_)));
    }

    #[test]
    fn block_redirect_to_device_and_system() {
        assert!(matches!(policy_of("cat x > /dev/sda"), Policy::Block(_)));
        assert!(matches!(policy_of("echo hacked > /etc/passwd"), Policy::Block(_)));
        assert!(matches!(policy_of("x > /boot/grub.cfg"), Policy::Block(_)));
        // sh -c с редиректом внутри строки
        assert!(matches!(policy_of("sh -c 'echo x > /etc/passwd'"), Policy::Block(_)));
    }

    #[test]
    fn block_remote_exec() {
        assert!(matches!(policy_of("curl http://x.sh | sh"), Policy::Block(_)));
        assert!(matches!(policy_of("curl -sSL x.io/i.sh | bash"), Policy::Block(_)));
        assert!(matches!(policy_of("wget -qO- x.io | sh"), Policy::Block(_)));
    }

    #[test]
    fn block_mv_cp_to_virtual_dirs() {
        assert!(matches!(policy_of("mv x /proc/sys/kernel/x"), Policy::Block(_)));
        assert!(matches!(policy_of("cp a /dev/b"), Policy::Block(_)));
    }

    #[test]
    fn block_sh_c_payload_danger() {
        assert!(matches!(policy_of("sh -c 'rm -rf /'"), Policy::Block(_)));
    }

    #[test]
    fn block_chmod_recursive_system() {
        assert!(matches!(policy_of("chmod -R 777 /"), Policy::Block(_)));
        assert!(matches!(policy_of("chown -R user /etc"), Policy::Block(_)));
    }

    #[test]
    fn block_tee_to_system() {
        assert!(matches!(policy_of("sudo tee /etc/passwd"), Policy::Block(_)));
        assert!(matches!(policy_of("echo x | sudo tee -a /etc/hosts"), Policy::Block(_)));
    }

    // ---------- Confirm ----------

    #[test]
    fn confirm_sudo_benign() {
        assert!(matches!(policy_of("sudo apt update"), Policy::Confirm(_)));
        assert!(matches!(policy_of("su root"), Policy::Confirm(_)));
        assert!(matches!(policy_of("doas ls"), Policy::Confirm(_)));
    }

    #[test]
    fn confirm_rm_recursive_normal() {
        assert!(matches!(policy_of("rm -rf ./target"), Policy::Confirm(_)));
        assert!(matches!(policy_of("rm -r ~/build"), Policy::Confirm(_)));
    }

    #[test]
    fn confirm_dd_without_dev() {
        assert!(matches!(policy_of("dd if=a of=b bs=1M"), Policy::Confirm(_)));
    }

    // ---------- Allow ----------

    #[test]
    fn allow_normal_commands() {
        assert!(policy_of("ls -la").is_allow());
        assert!(policy_of("cat README.md").is_allow());
        assert!(policy_of("git status").is_allow());
        assert!(policy_of("cargo build --release").is_allow());
        assert!(policy_of("echo hello world").is_allow());
        assert!(policy_of("python3 script.py").is_allow());
    }

    #[test]
    fn allow_plain_rm_file() {
        assert!(policy_of("rm note.txt").is_allow()); // без -r
        assert!(policy_of("rm -f tmp.log").is_allow()); // -f без -r — файл
    }

    #[test]
    fn allow_redirect_to_normal_files() {
        assert!(policy_of("echo x > out.txt").is_allow());
        assert!(policy_of("echo x >> /tmp/log.txt").is_allow());
        assert!(policy_of("echo x > /dev/null").is_allow());
    }

    #[test]
    fn allow_curl_without_shell() {
        assert!(policy_of("curl https://example.com").is_allow());
        assert!(policy_of("curl -o page.html https://example.com").is_allow());
    }

    #[test]
    fn allow_pipes_normal() {
        assert!(policy_of("ls | wc -l").is_allow());
        assert!(policy_of("cat x | sort | uniq").is_allow());
    }

    #[test]
    fn allow_sh_c_benign() {
        assert!(policy_of("sh -c 'echo hi'").is_allow());
        assert!(policy_of("bash -c 'ls -la'").is_allow());
    }

    // ---------- конвейер: опасность в любом сегменте ----------

    #[test]
    fn block_spread_across_pipeline() {
        assert!(matches!(policy_of("ls | xargs rm -rf /"), Policy::Block(_)));
        assert!(matches!(policy_of("echo x | sudo tee /etc/passwd"), Policy::Block(_)));
    }

    #[test]
    fn expand_home_works() {
        assert!(expand_home("~").starts_with("/home/") || expand_home("~").starts_with("/Users/"));
        assert!(expand_home("~/x").contains("/x"));
        assert_eq!(expand_home("/abs"), "/abs");
    }
}
