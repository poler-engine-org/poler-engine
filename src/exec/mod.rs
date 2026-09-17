//! POLER Exec — идеальный исполнитель команд (E1/v0.31.0, фича `pnd-ffi`).
//!
//! Рождён диагностикой исходников GNU bash 5.2 самим POLER-Engine
//! (docs/EXEC_AUDIT.md): 337 небезопасных strcpy/sprintf/strcat в 71 C-файле,
//! `free()` внутри обработчика сигнала (trap.c:839), гонка REINSTALL_SIGCHLD
//! (потеря SIGCHLD → зомби), неограниченный захват вывода `$(...)` (subst.c),
//! ноль таймаутов на дочерние процессы.
//!
//! Ядро — `os/core/poler_exec.zig` (Zig, raw-syscall слой на ассемблере,
//! ноль libc в ребёнке между fork и exec). Этот модуль — безопасная обвязка:
//! PATH-разрешение программы, CString-мост, типизированные ошибки.
//!
//! Контракт (все классы bash-ошибок исключены конструктивно):
//! - таймаут: timerfd(MONOTONIC) + SIGTERM → grace → SIGKILL группе;
//! - вывод: кольцевой буфер — хвост N байт + флаг truncated, O(1) памяти;
//! - зомби: pidfd-пробуждение + wait4(WNOHANG) в ppoll-цикле — ребёнок
//!   отреапан всегда, даже при таймауте и провале exec;
//! - инъекции: argv передаётся массивом, БЕЗ шелл-парсинга;
//! - fds: CLOEXEC-гигиена, чужие дескрипторы ребёнку не протекают.

use std::ffi::{c_char, CString};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

// ── C-ABI слоя Zig (os/core/poler_exec.zig) ─────────────────────────────────

#[repr(C)]
struct ExecOptions {
    timeout_ms: u64,
    grace_ms: u64,
    max_out_bytes: u64,
    stdin_data: *const u8,
    stdin_len: u64,
}

#[repr(C)]
struct ExecResult {
    exit_code: i32,
    signal: i32,
    timed_out: u32,
    truncated: u32,
    duration_us: u64,
    stdout_len: u64,
    stderr_len: u64,
    pid: i32,
}

extern "C" {
    fn poler_exec_run(
        path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
        opts: *const ExecOptions,
        stdout_buf: *mut u8,
        stdout_cap: usize,
        stderr_buf: *mut u8,
        stderr_cap: usize,
        res: *mut ExecResult,
    ) -> i32;
}

// ── Публичный контракт ──────────────────────────────────────────────────────

/// Спецификация запуска. `program` — имя из PATH или путь с '/'.
#[derive(Debug, Clone)]
pub struct ExecSpec {
    pub program: String,
    pub args: Vec<String>,
    /// `None` — наследовать окружение текущего процесса.
    pub env: Option<Vec<(String, String)>>,
    /// Жёсткий таймаут, мс. 0 — без таймаута (осторожно!).
    pub timeout_ms: u64,
    /// Grace между SIGTERM и SIGKILL, мс [умолчание в обвязке: 100].
    pub grace_ms: u64,
    /// Лимит захвата НА ПОТОК (удерживается ХВОСТ вывода), байт.
    pub max_out_bytes: usize,
    /// Данные в stdin ребёнка.
    pub stdin_data: Option<Vec<u8>>,
}

impl Default for ExecSpec {
    fn default() -> Self {
        Self {
            program: String::new(),
            args: Vec::new(),
            env: None,
            timeout_ms: 30_000,
            grace_ms: 100,
            max_out_bytes: 64 * 1024,
            stdin_data: None,
        }
    }
}

/// Итог исполнения.
#[derive(Debug, Clone)]
pub struct ExecOutcome {
    /// Код выхода; `None` — процесс убит сигналом.
    pub exit_code: Option<i32>,
    /// Сигнал, убивший процесс (обычно TERM/KILL после таймаута).
    pub signal: Option<i32>,
    pub timed_out: bool,
    /// Вывод превысил лимит — удержан хвост.
    pub truncated: bool,
    /// Хвост stdout (до `max_out_bytes`).
    pub stdout: Vec<u8>,
    /// Хвост stderr.
    pub stderr: Vec<u8>,
    /// Полная длительность, мкс (CLOCK_MONOTONIC).
    pub duration_us: u64,
    /// Пид ребёнка (диагностика).
    pub pid: i32,
}

/// Ошибка запуска (исполнение, начавшееся хотя бы на мгновение,
/// ошибкой не считается — см. `ExecOutcome`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecError {
    /// Программа не найдена (PATH/путь) — ENOENT. Код 127.
    NotFound,
    /// Нет права исполнения — EACCES. Код 126.
    PermissionDenied,
    /// Прочий errno этапа запуска (EAGAIN/ENOMEM/ENOTDIR/EISDIR…).
    SpawnFailed(i32),
    /// Некорректная спецификация (NUL в строках и т.п.).
    Invalid(String),
}

impl std::fmt::Display for ExecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "программа не найдена"),
            Self::PermissionDenied => write!(f, "нет права исполнения"),
            Self::SpawnFailed(e) => write!(f, "запуск не удался (errno {e})"),
            Self::Invalid(m) => write!(f, "некорректная спецификация: {m}"),
        }
    }
}

impl std::error::Error for ExecError {}

// ── PATH-разрешение (ядро принимает только явный путь) ─────────────────────

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match p.metadata() {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

fn resolve_program(program: &str) -> Result<PathBuf, ExecError> {
    if program.is_empty() {
        return Err(ExecError::Invalid("пустое имя программы".into()));
    }
    if program.contains('/') {
        let p = PathBuf::from(program);
        return if is_executable(&p) { Ok(p) } else { Err(ExecError::NotFound) };
    }
    let path_env = std::env::var("PATH").unwrap_or_else(|_| "/bin:/usr/bin".into());
    for dir in path_env.split(':') {
        let dir = if dir.is_empty() { "." } else { dir };
        let cand = PathBuf::from(dir).join(program);
        if is_executable(&cand) {
            return Ok(cand);
        }
    }
    Err(ExecError::NotFound)
}

// ── Запуск ──────────────────────────────────────────────────────────────────

/// Выполнить команду по спецификации. Блокирует до смерти ребёнка,
/// таймаута или исчерпания дренажа — но никогда не зависает навсегда
/// (при `timeout_ms > 0`).
pub fn run(spec: &ExecSpec) -> Result<ExecOutcome, ExecError> {
    let prog = resolve_program(&spec.program)?;
    let prog_c = CString::new(prog.as_os_str().as_bytes())
        .map_err(|_| ExecError::Invalid("NUL-байт в пути программы".into()))?;

    // argv[0] — каноническое имя программы, дальше аргументы, NULL-терминатор.
    let mut argv_owned: Vec<CString> = Vec::with_capacity(spec.args.len() + 1);
    argv_owned.push(prog_c.clone());
    for a in &spec.args {
        argv_owned.push(
            CString::new(a.as_str())
                .map_err(|_| ExecError::Invalid("NUL-байт в аргументе".into()))?,
        );
    }
    let argv_ptrs: Vec<*const c_char> = argv_owned
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // envp: явное окружение или наследование текущего.
    let env_owned: Vec<CString> = match &spec.env {
        Some(pairs) => pairs
            .iter()
            .map(|(k, v)| {
                CString::new(format!("{k}={v}"))
                    .map_err(|_| ExecError::Invalid("NUL-байт в переменной окружения".into()))
            })
            .collect::<Result<_, _>>()?,
        None => std::env::vars_os()
            .map(|(k, v)| {
                let mut b = k.as_bytes().to_vec();
                b.push(b'=');
                b.extend_from_slice(v.as_bytes());
                CString::new(b)
                    .map_err(|_| ExecError::Invalid("NUL-байт в окружении процесса".into()))
            })
            .collect::<Result<_, _>>()?,
    };
    let envp_ptrs: Vec<*const c_char> = env_owned
        .iter()
        .map(|s| s.as_ptr())
        .chain(std::iter::once(std::ptr::null()))
        .collect();

    // Буферы захвата: max_out_bytes == 0 → discard (null/0 по контракту ядра).
    let cap = spec.max_out_bytes;
    let mut stdout_buf = vec![0u8; cap.max(1)];
    let mut stderr_buf = vec![0u8; cap.max(1)];
    let (so_ptr, so_cap) =
        if cap == 0 { (std::ptr::null_mut(), 0) } else { (stdout_buf.as_mut_ptr(), cap) };
    let (se_ptr, se_cap) =
        if cap == 0 { (std::ptr::null_mut(), 0) } else { (stderr_buf.as_mut_ptr(), cap) };

    let stdin_ref = spec.stdin_data.as_deref().unwrap_or(&[]);
    let opts = ExecOptions {
        timeout_ms: spec.timeout_ms,
        grace_ms: spec.grace_ms,
        max_out_bytes: cap as u64,
        stdin_data: if stdin_ref.is_empty() {
            std::ptr::null()
        } else {
            stdin_ref.as_ptr()
        },
        stdin_len: stdin_ref.len() as u64,
    };

    let mut res = ExecResult {
        exit_code: -1,
        signal: 0,
        timed_out: 0,
        truncated: 0,
        duration_us: 0,
        stdout_len: 0,
        stderr_len: 0,
        pid: -1,
    };

    // Единственный unsafe: вызов C-ABI ядра. Все указатели валидны
    // до конца вызова (argv_owned/env_owned/stdin живут на стеке).
    let rc = unsafe {
        poler_exec_run(
            prog_c.as_ptr(),
            argv_ptrs.as_ptr(),
            envp_ptrs.as_ptr(),
            &opts,
            so_ptr,
            so_cap,
            se_ptr,
            se_cap,
            &mut res,
        )
    };

    if rc != 0 {
        return Err(match -rc {
            2 => ExecError::NotFound,          // ENOENT
            13 => ExecError::PermissionDenied,  // EACCES
            e => ExecError::SpawnFailed(e),
        });
    }

    Ok(ExecOutcome {
        exit_code: if res.exit_code >= 0 { Some(res.exit_code) } else { None },
        signal: if res.signal != 0 { Some(res.signal) } else { None },
        timed_out: res.timed_out != 0,
        truncated: res.truncated != 0,
        stdout: stdout_buf[..(res.stdout_len as usize).min(cap)].to_vec(),
        stderr: stderr_buf[..(res.stderr_len as usize).min(cap)].to_vec(),
        duration_us: res.duration_us,
        pid: res.pid,
    })
}

// ── Тесты (живой fork/exec — как в os/core/poler_exec.zig) ─────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(program: &str, args: &[&str]) -> ExecSpec {
        ExecSpec {
            program: program.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
            ..ExecSpec::default()
        }
    }

    #[test]
    fn echo_roundtrip() {
        let out = run(&spec("echo", &["полер-exec", "живой"])).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(!out.timed_out);
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "полер-exec живой");
        assert!(out.pid > 0);
    }

    #[test]
    fn exit_code_42() {
        let out = run(&spec("sh", &["-c", "exit 42"])).unwrap();
        assert_eq!(out.exit_code, Some(42));
        assert_eq!(out.signal, None);
    }

    #[test]
    fn stderr_capture() {
        let out = run(&spec("sh", &["-c", "echo err-полер >&2"])).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.stdout.is_empty());
        assert_eq!(String::from_utf8_lossy(&out.stderr).trim(), "err-полер");
    }

    #[test]
    fn timeout_kills() {
        let s = ExecSpec { timeout_ms: 80, grace_ms: 40, ..spec("sleep", &["30"]) };
        let out = run(&s).unwrap();
        assert!(out.timed_out);
        assert!(out.duration_us < 2_000_000, "длительность {} мкс", out.duration_us);
        assert!(out.signal == Some(15) || out.signal == Some(9));
    }

    #[test]
    fn not_found() {
        let err = run(&spec("/несуществующий/полер", &[])).unwrap_err();
        assert_eq!(err, ExecError::NotFound);
    }

    #[test]
    fn name_from_path_not_found() {
        let err = run(&spec("полер-несуществует-12345", &[])).unwrap_err();
        assert_eq!(err, ExecError::NotFound);
    }

    #[test]
    fn big_output_tail() {
        // 256 КиБ нулей при лимите 4 КиБ: хвост + truncated.
        let s = ExecSpec {
            max_out_bytes: 4096,
            timeout_ms: 5000,
            ..spec("dd", &["if=/dev/zero", "bs=1024", "count=256"])
        };
        let out = run(&s).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.truncated);
        assert_eq!(out.stdout.len(), 4096);
        assert!(out.stdout.iter().all(|&b| b == 0));
    }

    #[test]
    fn stdin_roundtrip() {
        let payload = "stdin-полер-42";
        let s = ExecSpec { stdin_data: Some(payload.as_bytes().to_vec()), ..spec("cat", &[]) };
        let out = run(&s).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout, payload.as_bytes());
    }

    #[test]
    fn env_override() {
        let s = ExecSpec {
            env: Some(vec![("POLER_TEST_X".into(), "значение-1".into())]),
            ..spec("sh", &["-c", "printf %s \"$POLER_TEST_X\""])
        };
        let out = run(&s).unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert_eq!(out.stdout, "значение-1".as_bytes());
    }
}
