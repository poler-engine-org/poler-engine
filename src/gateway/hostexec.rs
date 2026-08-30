//! # Terminal Gateway: контролируемый запуск хостовых команд (v0.22.0)
//!
//! Sandboxed OS Subshell на уровне исполнения: прямой спавн бинарника
//! (БЕЗ `/bin/sh` — токенизацию сделали мы), отдельная группа процессов
//! (Ctrl+C уходит группе, а не только лидеру), кап вывода (как CDP-кап из
//! аудита v0.21.1), таймаут стены, фильтрация окружения (секреты не
//! наследуются), SIGINT/SIGKILL по группе с grace-периодом.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Кап вывода на поток: 16 МБ (канон CDP HTTP_BODY_CAP из аудита v0.21.1).
pub const OUTPUT_CAP: usize = 16 * 1024 * 1024;
/// Таймаут стены по умолчанию (сек). Меняется через `set hosttimeout N`.
pub const DEFAULT_HOST_TIMEOUT_SECS: u64 = 120;
/// Grace-период между SIGINT и SIGKILL при прерывании пользователем.
const KILL_GRACE: Duration = Duration::from_secs(2);
/// Интервал поллинга try_wait (мс).
const POLL_MS: u64 = 30;

/// Режим окружения дочерних процессов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvMode {
    /// Минимальное окружение: PATH/HOME/LANG/TERM/TMPDIR + POLER_*,
    /// секретные переменные вырезаны (по умолчанию).
    Filtered,
    /// Полное наследование (явное решение пользователя: `set hostenv full`).
    Full,
}

impl std::fmt::Display for EnvMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EnvMode::Filtered => write!(f, "filtered"),
            EnvMode::Full => write!(f, "full"),
        }
    }
}

/// Лимиты исполнения хостовой команды.
#[derive(Debug, Clone)]
pub struct HostLimits {
    pub timeout_secs: u64,
    pub output_cap: usize,
    pub env_mode: EnvMode,
}

impl Default for HostLimits {
    fn default() -> Self {
        Self {
            timeout_secs: DEFAULT_HOST_TIMEOUT_SECS,
            output_cap: OUTPUT_CAP,
            env_mode: EnvMode::Filtered,
        }
    }
}

/// Результат исполнения хостовой команды.
#[derive(Debug, Clone, Default)]
pub struct HostOutcome {
    pub stdout: String,
    pub stderr: String,
    /// Код возврата (None — процесс убит сигналом/не запущен).
    pub code: Option<i32>,
    /// Прерван пользователем (Ctrl+C).
    pub interrupted: bool,
    /// Убит по таймауту.
    pub timed_out: bool,
    /// Вывод достиг капа и был обрезан.
    pub truncated: bool,
    /// Не удалось запустить (нет такого бинарника/permission denied).
    pub spawn_error: Option<String>,
}

impl HostOutcome {
    /// Печать в терминал gateway: stdout в общий поток, stderr — с меткой.
    pub fn render(&self) -> String {
        if let Some(e) = &self.spawn_error {
            return format!("⚡ {e}");
        }
        let mut out = String::new();
        out.push_str(&self.stdout);
        if !self.stderr.is_empty() {
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            for l in self.stderr.lines() {
                out.push_str(&format!("⚠ stderr: {l}\n"));
            }
        }
        if self.truncated {
            out.push_str(&format!(
                "⚠ вывод обрезан на капе {} байт\n",
                OUTPUT_CAP
            ));
        }
        if self.timed_out {
            out.push_str("⚠ таймаут: процесс убит по лимиту стены\n");
        }
        if self.interrupted {
            out.push_str("⚠ прервано (Ctrl+C)\n");
        }
        if self.code.is_some_and(|c| c != 0) && !self.timed_out && !self.interrupted {
            out.push_str(&format!("⚠ exit-код: {}\n", self.code.unwrap()));
        }
        out
    }
}

/// Прерывание: флаг поднимает ctrlc-обработчик gateway (mod.rs).
pub static INTERRUPT: AtomicBool = AtomicBool::new(false);

/// Сброс флага прерывания перед новой командой.
pub fn clear_interrupt() {
    INTERRUPT.store(false, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// Фильтрация окружения
// ---------------------------------------------------------------------------

/// Имена переменных, которые наследуются всегда.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH", "HOME", "LANG", "TERM", "TMPDIR", "TMP", "USER", "LOGNAME", "SHELL",
    "LC_ALL", "LC_CTYPE", "LC_NUMERIC", "LC_TIME", "LC_COLLATE", "LC_MONETARY",
    "LC_MESSAGES", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_STATE_HOME",
    "COLORTERM", "NO_COLOR",
];

/// Секретоподобные подстроки в имени переменной → вырезать.
fn looks_secret(name: &str) -> bool {
    let up = name.to_uppercase();
    up.contains("TOKEN")
        || up.contains("SECRET")
        || up.contains("PASSWORD")
        || up.contains("PASSWD")
        || up.ends_with("_KEY")
        || up.ends_with("_KEYS")
        || up.contains("PRIVATE")
}

/// Собрать фильтрованное окружение: allowlist-переменные + POLER_* без
/// секретов + маркер POLER_GATEWAY=1.
pub fn filtered_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    for (k, v) in std::env::vars() {
        let keep = (ENV_ALLOWLIST.contains(&k.as_str())
            || (k.starts_with("POLER_") && !looks_secret(&k) && !k.starts_with("POLER_MCP_TOKEN"))
            || (k.starts_with("LC_") && !k.starts_with("LC_ALL_")))
            // POLER_GATEWAY задаём сами ниже — не берём из родителя
            && k != "POLER_GATEWAY";
        if keep {
            env.push((k, v));
        }
    }
    env.push(("POLER_GATEWAY".into(), "1".into()));
    env
}

// ---------------------------------------------------------------------------
// Запуск
// ---------------------------------------------------------------------------

/// Исполнить хостовую команду с контролем. `tokens[0]` — бинарник (ищется
/// в PATH), остальное — аргументы. `stdin` — данные предыдущего сегмента
/// конвейера (None — наследовать / закрыть).
#[allow(unused_variables)]
pub fn run(
    tokens: &[String],
    stdin: Option<&str>,
    limits: &HostLimits,
    cwd: &Path,
    extra_env: &[(String, String)],
) -> HostOutcome {
    clear_interrupt();
    if tokens.is_empty() {
        return HostOutcome {
            spawn_error: Some("пустая команда".into()),
            ..Default::default()
        };
    }
    let mut cmd = Command::new(&tokens[0]);
    cmd.args(&tokens[1..]).current_dir(cwd);
    cmd.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());

    // Окружение: filtered (по умолчанию) или full
    match limits.env_mode {
        EnvMode::Filtered => {
            cmd.env_clear();
            for (k, v) in filtered_env() {
                cmd.env(k, v);
            }
        }
        EnvMode::Full => {}
    }
    for (k, v) in extra_env {
        cmd.env(k, v);
    }

    // Отдельная группа процессов: сигналы уходят всей группе
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return HostOutcome {
                spawn_error: Some(format!(
                    "не удалось запустить «{}»: {e} (проверьте PATH; команды движка — смотрите `help`)",
                    tokens[0]
                )),
                ..Default::default()
            }
        }
    };

    // stdin-писатель в отдельном потоке (большой ввод не блокирует пайп)
    if let Some(input) = stdin {
        let mut handle = match child.stdin.take() {
            Some(h) => h,
            None => {
                let _ = child.kill();
                return HostOutcome {
                    spawn_error: Some("stdin-пайп недоступен".into()),
                    ..Default::default()
                };
            }
        };
        let data = input.to_string();
        std::thread::spawn(move || {
            let _ = handle.write_all(data.as_bytes());
            // drop(handle) закрывает пайп → дочерний видит EOF
        });
    } else {
        // stdin не нужен: закрываем сразу (drop)
        drop(child.stdin.take());
    }

    // Читатели stdout/stderr в отдельных потоках с капом
    let out_cap = limits.output_cap;
    let stdout_handle = child.stdout.take();
    let stderr_handle = child.stderr.take();
    let t_out = spawn_reader(stdout_handle, out_cap);
    let t_err = spawn_reader(stderr_handle, out_cap);

    // Ожидание с поллингом: таймаут + прерывание пользователя
    let started = Instant::now();
    let mut interrupted = false;
    let mut timed_out = false;
    let status = 'wait: loop {
        if INTERRUPT.load(Ordering::SeqCst) {
            interrupted = true;
            let _ = kill_group(&mut child, Signal::Int);
            // grace: дать процессу шанс на корректный выход
            let grace_start = Instant::now();
            loop {
                if let Some(st) = child.try_wait().unwrap_or(None) {
                    break 'wait Some(st);
                }
                if grace_start.elapsed() >= KILL_GRACE {
                    let _ = kill_group(&mut child, Signal::Kill);
                    break 'wait child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(POLL_MS));
            }
        }
        if let Some(st) = child.try_wait().unwrap_or(None) {
            break 'wait Some(st);
        }
        if started.elapsed() >= Duration::from_secs(limits.timeout_secs) {
            timed_out = true;
            let _ = kill_group(&mut child, Signal::Kill);
            break 'wait child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    };

    let (stdout, trunc_out) = t_out
        .map(|t| t.join().unwrap_or((String::new(), false)))
        .unwrap_or((String::new(), false));
    let (stderr, trunc_err) = t_err
        .map(|t| t.join().unwrap_or((String::new(), false)))
        .unwrap_or((String::new(), false));

    HostOutcome {
        stdout,
        stderr,
        code: status.and_then(|st| st.code()),
        interrupted,
        timed_out,
        truncated: trunc_out || trunc_err,
        spawn_error: None,
    }
}

/// Читатель потока с капом: читаем до EOF, копим не больше cap байт.
fn spawn_reader<R: Read + Send + 'static>(
    handle: Option<R>,
    cap: usize,
) -> Option<std::thread::JoinHandle<(String, bool)>> {
    handle.map(|mut r| {
        std::thread::spawn(move || {
            let mut collected: Vec<u8> = Vec::new();
            let mut truncated = false;
            let mut buf = [0u8; 8192];
            loop {
                match r.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if collected.len() + n > cap {
                            // добираем до cap, остальное сливаем в никуда
                            let room = cap.saturating_sub(collected.len());
                            collected.extend_from_slice(&buf[..room]);
                            truncated = true;
                        } else {
                            collected.extend_from_slice(&buf[..n]);
                        }
                    }
                    Err(_) => break,
                }
            }
            (String::from_utf8_lossy(&collected).to_string(), truncated)
        })
    })
}

// ---------------------------------------------------------------------------
// Сигналы (POSIX)
// ---------------------------------------------------------------------------

enum Signal {
    Int,
    Kill,
}

/// Послать сигнал ГРУППЕ процессов. Группа создана `process_group(0)`
/// при спавне, поэтому pgid == pid потомка; `kill(-pid, sig)` — классика
/// POSIX для доставки сигнала всем членам группы (coreutils делают так же).
#[cfg(unix)]
fn kill_group(child: &mut Child, sig: Signal) -> std::io::Result<()> {
    let pgid = -(child.id() as i32);
    let sig_num = match sig {
        Signal::Int => 2,  // SIGINT
        Signal::Kill => 9, // SIGKILL
    };
    send_signal(pgid, sig_num)
}

/// POSIX kill(2) — единственный unsafe в gateway (изолирован, как mmap
/// в ядре движка). Нулей зависимостей: libc не подключаем.
#[cfg(unix)]
fn send_signal(pid_or_pgid: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: kill(2) — тонкая обёртка над системным вызовом; отрицательный
    // pid = группа процессов (создана нами), sig ∈ {SIGINT, SIGKILL}.
    // Работы с памятью нет, состояние гонок исключено сигнальной моделью.
    let rc = unsafe { kill_syscall(pid_or_pgid, sig) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(unix)]
extern "C" {
    #[link_name = "kill"]
    fn kill_syscall(pid: i32, sig: i32) -> i32;
}

/// Публичная обёртка kill(2): signal 0 = liveness-проба (сервисный слой),
/// отрицательный pid = группа. Возвращает код syscall (0 = успех).
#[cfg(unix)]
pub fn kill_raw(pid_or_pgid: i32, sig: i32) -> i32 {
    // SAFETY: см. send_signal — тонкая обёртка, памяти не касается.
    unsafe { kill_syscall(pid_or_pgid, sig) }
}

#[cfg(not(unix))]
pub fn kill_raw(_pid_or_pgid: i32, _sig: i32) -> i32 {
    -1
}

#[cfg(not(unix))]
fn kill_group(child: &mut Child, _sig: Signal) -> std::io::Result<()> {
    child.kill()
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filtered_env_strips_secrets() {
        // подменяем окружение теста
        std::env::set_var("POLER_TEST_GREETING", "hi");
        std::env::set_var("POLER_TEST_TOKEN", "secret-value");
        std::env::set_var("GITHUB_TOKEN", "ghp_xxx");
        std::env::set_var("MY_API_KEY", "zzz");
        std::env::set_var("POLER_GATEWAY", "0");
        let env = filtered_env();
        let get = |k: &str| {
            env.iter()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(get("POLER_TEST_GREETING").as_deref(), Some("hi"));
        assert_eq!(get("POLER_TEST_TOKEN"), None, "POLER_*_TOKEN должен быть вырезан");
        assert_eq!(get("GITHUB_TOKEN"), None);
        assert_eq!(get("MY_API_KEY"), None);
        assert_eq!(get("POLER_GATEWAY").as_deref(), Some("1"), "маркер субшелла");
        // PATH наследуется (иначе дочерние команды не найдутся)
        assert!(get("PATH").is_some());
    }

    #[test]
    fn run_echo_captures_stdout() {
        let lim = HostLimits::default();
        let out = run(&["echo".into(), "hello".into()], None, &lim, Path::new("."), &[]);
        assert!(out.spawn_error.is_none());
        assert_eq!(out.stdout.trim(), "hello");
        assert_eq!(out.code, Some(0));
    }

    #[test]
    fn run_stdin_pipe() {
        let lim = HostLimits::default();
        let out = run(&["cat".into()], Some("piped data"), &lim, Path::new("."), &[]);
        assert_eq!(out.stdout, "piped data");
    }

    #[test]
    fn run_missing_binary_is_spawn_error() {
        let lim = HostLimits::default();
        let out = run(
            &["definitely-not-a-binary-xyz".into()],
            None,
            &lim,
            Path::new("."),
            &[],
        );
        assert!(out.spawn_error.is_some());
    }

    #[test]
    fn run_timeout_kills_sleep() {
        let lim = HostLimits {
            timeout_secs: 1,
            ..Default::default()
        };
        let started = Instant::now();
        let out = run(&["sleep".into(), "30".into()], None, &lim, Path::new("."), &[]);
        assert!(out.timed_out);
        assert!(started.elapsed() < Duration::from_secs(5), "таймаут не сработал");
    }

    #[test]
    fn run_output_cap_truncates() {
        let lim = HostLimits {
            timeout_secs: 30,
            output_cap: 100,
            env_mode: EnvMode::Filtered,
        };
        // seq 1 1000 → ~4 КБ вывода, кап 100 байт
        let out = run(
            &["seq".into(), "1".into(), "1000".into()],
            None,
            &lim,
            Path::new("."),
            &[],
        );
        assert!(out.truncated);
        assert!(out.stdout.len() <= 100);
    }

    #[test]
    fn run_exit_code_propagates() {
        let lim = HostLimits::default();
        let out = run(
            &["sh".into(), "-c".into(), "exit 7".into()],
            None,
            &lim,
            Path::new("."),
            &[],
        );
        assert_eq!(out.code, Some(7));
    }

    #[test]
    fn render_shows_stderr_prefix() {
        let out = HostOutcome {
            stderr: "oops".into(),
            code: Some(2),
            ..Default::default()
        };
        let r = out.render();
        assert!(r.contains("⚠ stderr: oops"));
        assert!(r.contains("exit-код: 2"));
    }
}
