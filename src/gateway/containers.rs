//! # Container Jail (`box`) — жёсткая Docker-изоляция контуров 2/3 (v0.25.0)
//!
//! Живой кейс из эксплуатации (v0.23/v0.24): агент (`agy`), запущенный в
//! PTY-контуре, дергает свой Bash-tool напрямую на хосте — PATH-shim
//! медиация перехватывает вызовы через PATH/$SHELL, но хардкод `/bin/sh`
//! и прямой execve мимо шелла не накрываются (честно задокументировано в
//! §6.5). Единственная userspace-гарантия — **физическая изоляция**:
//! движок поднимает Docker-контейнер («box») и исполняет host-os-proxy
//! (контур 2) и pty-passthrough (контур 3) ВНУТРИ него через `docker exec`.
//!
//! Модель:
//! * `/workspace` ← bind-mount корня workspace (rw; `wsro=1` — read-only);
//! * `/home/poler` ← персистентный home агентов на хосте
//!   (`~/.local/share/poler-engine/box/<name>`) — конфиги/токены агентов
//!   переживают `box off/on`;
//! * хост НЕ монтируется больше нигде; docker-сокет НЕ пробрасывается —
//!   агент внутри не может управлять демоном;
//! * hardening: `--cap-drop ALL`, `no-new-privileges`, `--init` (tini —
//!   зомби-реапер), лимиты памяти/swap и pids, `--stop-timeout 2`;
//! * юзер по умолчанию — uid:gid владельца (файлы в /workspace остаются
//!   его собственными); `user=root` — root ВНУТРИ контейнера (пакеты),
//! * verdict-плоскость сохраняется: судья по-прежнему выносит вердикт ДО
//!   исполнения (деструктив — Block), но логическая граница workspace для
//!   exec-плоскости снимается — её заменяет сам контейнер (redirect-цели
//!   и движковые файл-команды судятся с границей ВСЕГДА: они физически
//!   исполняются на хосте);
//! * управление docker/podman-демоном из gateway при активном jail —
//!   Confirm (`docker run -v /:/host` ломает изоляцию).
//!
//! Нулей новых зависимостей: docker-клиент вызывается как подпроцесс
//! (как сервисный слой service.rs), pure-хелперы вынесены для юнит-тестов.
//! Тесты подменяют бинарник через `POLER_BOX_DOCKER` (по умолчанию `docker`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// Контейнер-точка монтирования workspace.
pub const WS_MOUNT: &str = "/workspace";
/// Контейнерный home (bind-mount персистентного каталога агентов).
pub const HOME_MOUNT: &str = "/home/poler";
/// Образ по умолчанию: маленький, всегда pullable; для агентов —
/// `box on image=<свой образ с node/agy>`.
pub const DEFAULT_IMAGE: &str = "debian:bookworm-slim";

// ---------------------------------------------------------------------------
// Конфигурация
// ---------------------------------------------------------------------------

/// Конфигурация Container Jail (`box on [k=v…]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoxConfig {
    /// Образ контейнера (docker run <image>).
    pub image: String,
    /// Сеть: `bridge` (агентам нужен API) | `none` (полная тишина).
    pub net: String,
    /// Root ВНУТРИ контейнера (пакеты) вместо uid владельца.
    pub user_root: bool,
    /// Лимит памяти (docker-нотация: 512m, 2g…). Swap = mem (без резерва).
    pub mem: String,
    /// Лимит процессов (анти-форк-бомба второго эшелона).
    pub pids: u32,
    /// Монтировать workspace read-only (анализ без записи).
    pub ws_readonly: bool,
}

impl Default for BoxConfig {
    fn default() -> Self {
        Self {
            image: DEFAULT_IMAGE.into(),
            net: "bridge".into(),
            user_root: false,
            mem: "2g".into(),
            pids: 512,
            ws_readonly: false,
        }
    }
}

impl BoxConfig {
    /// Строка юзера для docker run: uid:gid владельца или 0:0.
    pub fn user_arg(&self) -> String {
        if self.user_root {
            "0:0".into()
        } else {
            let (uid, gid) = uid_gid();
            format!("{uid}:{gid}")
        }
    }
}

/// Разбор аргументов `box on`. Форма: `box on [IMAGE] [k=v…]` — первый
/// позиционный аргумент без `=` трактуется как образ (UX-шорткат).
pub fn parse_on_args(args: &[String]) -> Result<BoxConfig, String> {
    let mut cfg = BoxConfig::default();
    for a in args {
        if a.is_empty() {
            continue;
        }
        if !a.contains('=') {
            if !a.starts_with('-') && cfg.image == DEFAULT_IMAGE {
                cfg.image = a.clone();
                continue;
            }
            return Err(format!(
                "box on: {a}? (k=v: image=… net=bridge|none user=me|root mem=2g pids=512 wsro=0|1)"
            ));
        }
        let (k, v) = a.split_once('=').ok_or("box on: пустой ключ")?;
        match k {
            "image" => {
                if v.is_empty() || v.contains(char::is_whitespace) {
                    return Err("box on: image= не может быть пустым или с пробелами".into());
                }
                cfg.image = v.into();
            }
            "net" => match v {
                "bridge" | "none" => cfg.net = v.into(),
                "host" => {
                    return Err(
                        "box on: net=host открывает сеть хоста — запрещено (bridge|none)".into(),
                    )
                }
                other => {
                    return Err(format!(
                        "box on: net={other}? (bridge|none; host запрещён)"
                    ))
                }
            },
            "user" => match v {
                "me" => cfg.user_root = false,
                "root" => cfg.user_root = true,
                other => return Err(format!("box on: user={other}? (me|root)")),
            },
            "mem" => {
                if !valid_mem(v) {
                    return Err(format!(
                        "box on: mem={v}? (примеры: 512m, 2g, 8g)"
                    ));
                }
                cfg.mem = v.into();
            }
            "pids" => {
                let n: u32 = v
                    .parse()
                    .map_err(|_| format!("box on: pids={v}? (число 16..=8192)"))?;
                if !(16..=8192).contains(&n) {
                    return Err("box on: pids вне диапазона 16..=8192".into());
                }
                cfg.pids = n;
            }
            "wsro" | "readonly" => match v {
                "1" | "true" | "on" | "yes" => cfg.ws_readonly = true,
                "0" | "false" | "off" | "no" => cfg.ws_readonly = false,
                other => return Err(format!("box on: {k}={other}? (0|1)")),
            },
            other => {
                return Err(format!(
                    "box on: {other}=? (ключи: image net user mem pids wsro)"
                ))
            }
        }
    }
    Ok(cfg)
}

/// docker-нотация памяти: цифры + опциональный суффикс b/k/m/g/t.
fn valid_mem(v: &str) -> bool {
    let mut chars = v.chars();
    match chars.next() {
        Some(c) if c.is_ascii_digit() => {}
        _ => return false,
    }
    let digits_end = v
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(v.len());
    let suffix = &v[digits_end..];
    suffix.is_empty() || matches!(suffix, "b" | "k" | "m" | "g" | "t")
}

// ---------------------------------------------------------------------------
// Состояние jail
// ---------------------------------------------------------------------------

/// Активный Container Jail (сессионное состояние gateway).
#[derive(Debug, Clone)]
pub struct BoxState {
    /// Имя контейнера (стабильно выводится из корня workspace).
    pub name: String,
    /// Конфигурация (образ — фактический, из label контейнера при adopt).
    pub cfg: BoxConfig,
    /// Хост-путь, смонтированный в /workspace.
    pub ws_root: PathBuf,
    /// Хост-каталог, смонтированный в /home/poler.
    pub home_dir: PathBuf,
}

/// uid/gid владельца процесса (для --user; тонкие обёртки как kill(2)).
#[cfg(unix)]
pub fn uid_gid() -> (u32, u32) {
    extern "C" {
        fn getuid() -> u32;
        fn getgid() -> u32;
    }
    // SAFETY: getuid/getgid — read-only системные вызовы без состояния.
    unsafe { (getuid(), getgid()) }
}

#[cfg(not(unix))]
pub fn uid_gid() -> (u32, u32) {
    (1000, 1000)
}

/// Бинарник docker-клиента (тесты подменяют через POLER_BOX_DOCKER).
fn docker_bin() -> String {
    std::env::var("POLER_BOX_DOCKER")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "docker".into())
}

/// Стабильное имя контейнера из корня workspace: `poler-box-<fnv1a-8>`.
/// Один workspace = один контейнер (две сессии gateway на одном корне
/// делят box — docker exec конкурирует корректно).
pub fn container_name(ws_root: &Path) -> String {
    let canon = ws_root.canonicalize().unwrap_or_else(|_| ws_root.to_path_buf());
    let mut h: u64 = 0xcbf29ce484222325;
    for b in canon.display().to_string().as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    // младшие 32 бита: компактное стабильное имя (8 hex-символов)
    format!("poler-box-{:08x}", h as u32)
}

/// Персистентный home агентов: `~/.local/share/poler-engine/box/<name>`
/// (база переопределяется POLER_BOX_HOME — тесты/встраивание).
pub fn box_home_base() -> PathBuf {
    if let Ok(d) = std::env::var("POLER_BOX_HOME") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".local/share/poler-engine/box")
}

// ---------------------------------------------------------------------------
// docker-клиент (подпроцесс с таймаутом и дренажом пайпов)
// ---------------------------------------------------------------------------

/// Дренаж потока в фон (pull-прогресс может превысить буфер пайпа).
fn drain(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut v: Vec<u8> = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if v.len() < 1_048_576 {
                        v.extend_from_slice(&buf[..n]);
                    }
                }
                Err(_) => break,
            }
        }
        v
    })
}

/// Выполнить docker <args> с таймаутом. Успех = exit 0, возврат stdout.
/// НЕ env_clear: клиенту нужны PATH/DOCKER_HOST/XDG_RUNTIME_DIR.
fn docker_cmd(args: &[&str], timeout: Duration) -> Result<String, String> {
    let bin = docker_bin();
    let mut cmd = Command::new(&bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child: Child = cmd
        .spawn()
        .map_err(|e| format!("не удалось запустить «{bin}»: {e} (docker установлен?)"))?;
    let t_out = child.stdout.take().map(drain);
    let t_err = child.stderr.take().map(drain);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if started.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            Err(e) => {
                let _ = child.kill();
                return Err(format!("docker: wait: {e}"));
            }
        }
    };
    let out = String::from_utf8_lossy(&t_out.map(|t| t.join().unwrap_or_default()).unwrap_or_default()).to_string();
    let err = String::from_utf8_lossy(&t_err.map(|t| t.join().unwrap_or_default()).unwrap_or_default()).to_string();
    match status {
        Some(st) if st.success() => Ok(out),
        Some(st) => {
            let sub = args.first().copied().unwrap_or("");
            let tail: String =
                err.lines().rev().take(6).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
            Err(format!(
                "docker {sub}: exit {}: {}",
                st.code().unwrap_or(-1),
                if tail.is_empty() { out.trim() } else { &tail }
            ))
        }
        None => {
            let sub = args.first().copied().unwrap_or("");
            Err(format!("docker {sub}: таймаут {timeout:?}"))
        }
    }
}

/// Проба демона: версия Server (клиент без демона бесполезен).
pub fn docker_probe() -> Result<String, String> {
    docker_cmd(
        &["version", "--format", "{{.Server.Version}}"],
        Duration::from_secs(8),
    )
    .map(|v| v.trim().to_string())
}

/// Контейнер существует? → Some(running). Нет → None.
pub fn box_running(name: &str) -> Option<bool> {
    match docker_cmd(
        &["inspect", "-f", "{{.State.Running}}", name],
        Duration::from_secs(10),
    ) {
        Ok(out) => Some(out.trim() == "true"),
        Err(_) => None,
    }
}

/// Label контейнера (конфигурация переживает restart gateway).
fn inspect_label(name: &str, key: &str) -> Option<String> {
    let fmt = format!("{{{{index .Config.Labels \"{key}\"}}}}");
    docker_cmd(&["inspect", "-f", &fmt, name], Duration::from_secs(10))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------------------
// argv-билдеры (pure — юнит-тестируемые)
// ---------------------------------------------------------------------------

/// argv `docker run` для box: полный hardening-набор.
pub fn docker_run_argv(name: &str, cfg: &BoxConfig, ws_root: &Path, home_dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = vec![
        "run".into(),
        "-d".into(),
        "--name".into(),
        name.into(),
        "--hostname".into(),
        "poler-box".into(),
        "--init".into(),
        "--user".into(),
        cfg.user_arg(),
        "--cap-drop".into(),
        "ALL".into(),
        "--security-opt".into(),
        "no-new-privileges".into(),
        "--memory".into(),
        cfg.mem.clone(),
        "--memory-swap".into(),
        cfg.mem.clone(),
        "--pids-limit".into(),
        cfg.pids.to_string(),
        "--network".into(),
        cfg.net.clone(),
        "--stop-timeout".into(),
        "2".into(),
    ];
    let ro = if cfg.ws_readonly { ":ro" } else { "" };
    v.push("-v".into());
    v.push(format!("{}:{WS_MOUNT}{ro}", ws_root.display()));
    v.push("-v".into());
    v.push(format!("{}:{HOME_MOUNT}", home_dir.display()));
    v.push("-w".into());
    v.push(WS_MOUNT.into());
    v.push("-e".into());
    v.push(format!("HOME={HOME_MOUNT}"));
    v.push("--label".into());
    v.push("poler.box=1".into());
    v.push("--label".into());
    v.push(format!("poler.box.image={}", cfg.image));
    v.push(cfg.image.clone());
    v.extend(["sleep", "infinity"].map(Into::into));
    v
}

/// Хост-путь → путь внутри контейнера (ws_root→/workspace, home→/home/poler;
/// прочее — якорь /workspace: exec-плоскость живёт в jail).
pub fn in_container_path(jail: &BoxState, host_path: &Path) -> String {
    let canon = host_path
        .canonicalize()
        .unwrap_or_else(|_| host_path.to_path_buf());
    if canon.starts_with(&jail.ws_root) {
        let suffix = canon.strip_prefix(&jail.ws_root).unwrap_or(Path::new(""));
        if suffix.as_os_str().is_empty() {
            WS_MOUNT.into()
        } else {
            format!("{WS_MOUNT}/{}", suffix.display())
        }
    } else if canon.starts_with(&jail.home_dir) {
        let suffix = canon.strip_prefix(&jail.home_dir).unwrap_or(Path::new(""));
        if suffix.as_os_str().is_empty() {
            HOME_MOUNT.into()
        } else {
            format!("{HOME_MOUNT}/{}", suffix.display())
        }
    } else {
        WS_MOUNT.into()
    }
}

/// Общие флаги docker exec: рабочая директория + env + имя контейнера.
fn common_exec_args(jail: &BoxState, host_cwd: &Path) -> Vec<String> {
    let term = std::env::var("TERM").unwrap_or_else(|_| "xterm-256color".into());
    vec![
        "-w".into(),
        in_container_path(jail, host_cwd),
        "-e".into(),
        format!("HOME={HOME_MOUNT}"),
        "-e".into(),
        format!("TERM={term}"),
        jail.name.clone(),
    ]
}

/// Обёртка host-сегмента (контур 2): `docker exec -i …` — пайповый режим,
/// argv передаётся насквозь (без /bin/sh — тот же принцип, что hostexec).
pub fn wrap_host_exec(jail: &BoxState, tokens: &[String], host_cwd: &Path) -> Vec<String> {
    let mut v: Vec<String> = vec!["docker".into(), "exec".into(), "-i".into()];
    v.extend(common_exec_args(jail, host_cwd));
    v.extend(tokens.iter().cloned());
    v
}

/// Обёртка PTY-команды (контур 3): `docker exec -it …` — контейнерный TTY;
/// наш PTY-насос качает байты в docker-клиент, тот — в контейнерный tty.
pub fn wrap_pty_exec(jail: &BoxState, tokens: &[String], host_cwd: &Path) -> Vec<String> {
    let mut v: Vec<String> = vec!["docker".into(), "exec".into(), "-it".into()];
    v.extend(common_exec_args(jail, host_cwd));
    v.extend(tokens.iter().cloned());
    v
}

/// `box shell` — интерактивный шелл внутри jail. Константа (не ввод
/// пользователя): bash при наличии, иначе sh; судья не нужен — граница
/// здесь сам контейнер (полезная нагрузка не содержит пользовательских
/// данных). Инвариант покрыт тестом box_shell_tokens_constant.
pub fn box_shell_tokens() -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "exec bash 2>/dev/null || exec sh".into(),
    ]
}

/// Команда управляет контейнерным демоном хоста (docker/podman/…) —
/// при активном jail это способ сломать изоляцию (`docker run -v /:/x`)
/// → Confirm-гейт в dispatch.
pub fn is_daemon_ctl(tokens: &[String]) -> bool {
    let Some(first) = tokens.first() else {
        return false;
    };
    let base = first.rsplit('/').next().unwrap_or(first);
    matches!(
        base,
        "docker" | "podman" | "docker-compose" | "podman-compose" | "nerdctl" | "ctr" | "dockerd"
            | "podman-remote" | "lima" | "colima"
    )
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

/// Поднять/принять Container Jail для корня workspace.
/// Возвращает (состояние, отчёт для владельца).
pub fn box_on(ws_root: &Path, cfg: &BoxConfig) -> Result<(BoxState, String), String> {
    docker_probe().map_err(|e| format!("docker недоступен: {e}"))?;
    let canon = ws_root
        .canonicalize()
        .map_err(|e| format!("workspace {}: {e}", ws_root.display()))?;
    let name = container_name(&canon);

    // уже есть контейнер → принять (running) или стартовать (stopped)
    if let Some(running) = box_running(&name) {
        if !running {
            docker_cmd(&["start", &name], Duration::from_secs(30))
                .map_err(|e| format!("docker start {name}: {e}"))?;
        }
        let image = inspect_label(&name, "poler.box.image").unwrap_or_else(|| cfg.image.clone());
        let jail = BoxState {
            name: name.clone(),
            cfg: BoxConfig { image: image.clone(), ..cfg.clone() },
            ws_root: canon.clone(),
            home_dir: box_home_base().join(&name),
        };
        let verb = if running { "принят работающий" } else { "стартован остановленный" };
        return Ok((
            jail,
            format!(
                "📦 Container Jail ВКЛ — {verb} контейнер {name} (образ {image})\nконтуры 2/3 исполняются внутри; конфигурация контейнера неизменна до box off/on\n"
            ),
        ));
    }

    // создание: персистентный home + docker run (pull может занять минуты)
    let home = box_home_base().join(&name);
    std::fs::create_dir_all(&home).map_err(|e| format!("home {}: {e}", home.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
    }
    let run_args = docker_run_argv(&name, cfg, &canon, &home);
    let refs: Vec<&str> = run_args.iter().map(|s| s.as_str()).collect();
    docker_cmd(&refs, Duration::from_secs(600))
        .map_err(|e| format!("docker run: {e} (образ тянулся/отсутствует? docker pull {})", cfg.image))?;
    if box_running(&name) != Some(true) {
        // диагностика: контейнер умер сразу (sleep infinity недоступен в образе?)
        let _ = docker_cmd(&["rm", "-f", &name], Duration::from_secs(30));
        return Err(format!(
            "контейнер {name} не пережил запуск — образ {} без coreutils-sleep? попробуйте другой image=",
            cfg.image
        ));
    }
    let ro = if cfg.ws_readonly { "ro" } else { "rw" };
    let user: String = if cfg.user_root {
        "root(в контейнере)".to_string()
    } else {
        cfg.user_arg()
    };
    let report = format!(
        "📦 Container Jail ВКЛ — контуры 2/3 исполняются ВНУТРИ контейнера\n\
         контейнер: {name} · образ {image} · net {net} · user {user} · mem {mem} · pids {pids}\n\
         /workspace ← {ws} ({ro}) · /home/poler ← {home} (персистентный home агентов)\n\
         изоляция: cap-drop ALL · no-new-privileges · mem/pids-лимиты · docker-сокет не проброшен\n\
         агент в PTY (agy/claude/…) физически заперт: хост виден только как /workspace\n\
         деструктив по-прежнему блокируется судьёй; box off — разобрать\n",
        name = name,
        image = cfg.image,
        net = cfg.net,
        user = user,
        mem = cfg.mem,
        pids = cfg.pids,
        ws = canon.display(),
        ro = ro,
        home = home.display(),
    );
    let jail = BoxState {
        name: name.clone(),
        cfg: cfg.clone(),
        ws_root: canon.clone(),
        home_dir: home.clone(),
    };
    Ok((jail, report))
}

/// Разобрать jail: `docker rm -f` (home и workspace остаются на хосте).
pub fn box_off(name: &str) -> Result<String, String> {
    docker_probe().map_err(|e| format!("docker недоступен: {e}"))?;
    if box_running(name).is_none() {
        return Ok(format!("контейнер {name} не найден — убирать нечего\n"));
    }
    docker_cmd(&["rm", "-f", name], Duration::from_secs(60))
        .map_err(|e| format!("docker rm {name}: {e}"))?;
    Ok(format!(
        "📦 Container Jail ВЫКЛ — контейнер {name} удалён (workspace и home сохранены на хосте)\n"
    ))
}

/// Текст `box status`: docker-демон, ожидаемый/активный контейнер, монтировки.
pub fn box_status_text(active: Option<&BoxState>, ws_root: &Path) -> String {
    let docker_line = match docker_probe() {
        Ok(v) => format!("docker: сервер {v}"),
        Err(e) => format!("docker: недоступен ({e})"),
    };
    let mut s = String::new();
    match active {
        Some(jail) => {
            let state: String = match box_running(&jail.name) {
                Some(true) => "работает".into(),
                Some(false) => "остановлен (box on — стартовать)".into(),
                None => "не найден (box on — поднять)".into(),
            };
            s.push_str(&format!(
                "box: Container Jail ВКЛ — контуры 2/3 внутри контейнера\n{docker_line}\nконтейнер {}: {} · образ {}\nмонтировано: {WS_MOUNT} ← {} · {HOME_MOUNT} ← {}\nbox off — разобрать (данные останутся)\n",
                jail.name,
                state,
                jail.cfg.image,
                jail.ws_root.display(),
                jail.home_dir.display(),
            ));
        }
        None => {
            let name = container_name(ws_root);
            let state: String = match box_running(&name) {
                Some(true) => "работает (box on — принять)".into(),
                Some(false) => "остановлен (box on — стартовать)".into(),
                None => "не создан".into(),
            };
            s.push_str(&format!(
                "box: Container Jail ВЫКЛ — контуры 2/3 исполняются на хосте (workspace-guard)\n{docker_line}\nконтейнер {name}: {state}\nbox on [image=…] [net=bridge|none] [user=me|root] [mem=2g] [pids=512] [wsro=0|1] — поднять\nобраз по умолчанию: {DEFAULT_IMAGE} (для агентов — свой образ с node/agy)\n",
            ));
        }
    }
    s
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_on_args_defaults() {
        let c = parse_on_args(&[]).unwrap();
        assert_eq!(c, BoxConfig::default());
        assert_eq!(c.image, DEFAULT_IMAGE);
        assert_eq!(c.net, "bridge");
        assert!(!c.user_root);
        assert_eq!(c.mem, "2g");
        assert_eq!(c.pids, 512);
        assert!(!c.ws_readonly);
    }

    #[test]
    fn parse_on_args_positional_image_shortcut() {
        let c = parse_on_args(&["alpine:3.20".into()]).unwrap();
        assert_eq!(c.image, "alpine:3.20");
        // k=v после шортката
        let c = parse_on_args(&["node:22-slim".into(), "net=none".into()]).unwrap();
        assert_eq!(c.image, "node:22-slim");
        assert_eq!(c.net, "none");
    }

    #[test]
    fn parse_on_args_overrides() {
        let c = parse_on_args(&[
            "image=alpine:3.20".into(),
            "net=none".into(),
            "user=root".into(),
            "mem=8g".into(),
            "pids=1024".into(),
            "wsro=1".into(),
        ])
        .unwrap();
        assert_eq!(c.image, "alpine:3.20");
        assert_eq!(c.net, "none");
        assert!(c.user_root);
        assert_eq!(c.mem, "8g");
        assert_eq!(c.pids, 1024);
        assert!(c.ws_readonly);
        assert_eq!(c.user_arg(), "0:0");
    }

    #[test]
    fn parse_on_args_rejects_bad() {
        assert!(parse_on_args(&["net=host".into()]).is_err(), "host-сеть запрещена");
        assert!(parse_on_args(&["net=x".into()]).is_err());
        assert!(parse_on_args(&["user=both".into()]).is_err());
        assert!(parse_on_args(&["mem=2x".into()]).is_err());
        assert!(parse_on_args(&["mem=".into()]).is_err());
        assert!(parse_on_args(&["pids=abc".into()]).is_err());
        assert!(parse_on_args(&["pids=1".into()]).is_err(), "ниже 16");
        assert!(parse_on_args(&["pids=99999".into()]).is_err(), "выше 8192");
        assert!(parse_on_args(&["zzz=1".into()]).is_err(), "неизвестный ключ");
        assert!(parse_on_args(&["image=with space".into()]).is_err());
    }

    #[test]
    fn container_name_stable_and_distinct() {
        let a = container_name(Path::new("/home/u/proj"));
        let a2 = container_name(Path::new("/home/u/proj/")); // canonicalize поправит
        assert_eq!(a, container_name(Path::new("/home/u/proj")));
        assert!(a.starts_with("poler-box-"), "префикс: {a}");
        assert_eq!(a.len(), "poler-box-".len() + 8, "8 hex-символов: {a}");
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        if let Ok(canon) = Path::new("/home/u/proj").canonicalize() {
            assert_eq!(a, container_name(&canon));
        }
        let _ = a2; // может не существовать — не важно
    }

    fn fake_jail() -> BoxState {
        BoxState {
            name: "poler-box-deadbeef".into(),
            cfg: BoxConfig::default(),
            ws_root: PathBuf::from("/tmp/polertest-ws"),
            home_dir: PathBuf::from("/tmp/polertest-home"),
        }
    }

    #[test]
    fn in_container_path_mapping() {
        let j = fake_jail();
        assert_eq!(in_container_path(&j, Path::new("/tmp/polertest-ws")), "/workspace");
        assert_eq!(
            in_container_path(&j, Path::new("/tmp/polertest-ws/src/main.rs")),
            "/workspace/src/main.rs"
        );
        assert_eq!(in_container_path(&j, Path::new("/tmp/polertest-home")), "/home/poler");
        assert_eq!(
            in_container_path(&j, Path::new("/tmp/polertest-home/.gemini")),
            "/home/poler/.gemini"
        );
        // вне монтировок — якорь /workspace (exec-плоскость живёт в jail)
        assert_eq!(in_container_path(&j, Path::new("/etc")), "/workspace");
    }

    #[test]
    fn wrap_host_exec_shape() {
        let j = fake_jail();
        let w = wrap_host_exec(&j, &["ls".into(), "-la".into()], Path::new("/tmp/polertest-ws"));
        assert_eq!(&w[0..3], &["docker".to_string(), "exec".into(), "-i".into()]);
        assert!(!w.contains(&"-it".to_string()), "пайповый режим без -t: {w:?}");
        assert!(w.contains(&"-w".to_string()));
        assert!(w.contains(&"/workspace".to_string()));
        assert!(w.contains(&"HOME=/home/poler".to_string()));
        assert!(w.contains(&j.name), "имя контейнера: {w:?}");
        // хвост = исходные токены насквозь
        assert_eq!(&w[w.len() - 2..], &["ls".to_string(), "-la".to_string()]);
    }

    #[test]
    fn wrap_pty_exec_shape() {
        let j = fake_jail();
        let w = wrap_pty_exec(&j, &["vim".into(), "main.rs".into()], Path::new("/tmp/polertest-ws"));
        assert_eq!(&w[0..3], &["docker".to_string(), "exec".into(), "-it".to_string()]);
        assert_eq!(w.last().unwrap(), "main.rs");
    }

    #[test]
    fn docker_run_argv_hardening() {
        let cfg = BoxConfig {
            image: "alpine:3.20".into(),
            net: "none".into(),
            user_root: false,
            mem: "512m".into(),
            pids: 256,
            ws_readonly: true,
        };
        let v = docker_run_argv("poler-box-x", &cfg, Path::new("/ws"), Path::new("/home"));
        let joined = v.join("\u{1}");
        for must in [
            "--cap-drop\u{1}ALL",
            "--security-opt\u{1}no-new-privileges",
            "--memory\u{1}512m",
            "--pids-limit\u{1}256",
            "--network\u{1}none",
            "--init",
            "--stop-timeout\u{1}2",
            "/ws:/workspace:ro",
            "/home:/home/poler",
            "--label\u{1}poler.box.image=alpine:3.20",
        ] {
            assert!(joined.contains(must), "docker run без {must}: {v:?}");
        }
        // хвост: образ + sleep infinity (keepalive)
        assert_eq!(&v[v.len() - 3..], &["alpine:3.20".to_string(), "sleep".into(), "infinity".into()]);
        assert!(v.contains(&"-w".to_string()) && v.contains(&"/workspace".to_string()));
    }

    #[test]
    fn is_daemon_ctl_vectors() {
        assert!(is_daemon_ctl(&["docker".into(), "ps".into()]));
        assert!(is_daemon_ctl(&["/usr/bin/docker".into(), "run".into(), "-v".into(), "/:/x".into()]));
        assert!(is_daemon_ctl(&["podman".into()]));
        assert!(is_daemon_ctl(&["docker-compose".into(), "up".into()]));
        assert!(is_daemon_ctl(&["nerdctl".into()]));
        assert!(is_daemon_ctl(&["colima".into()]));
        assert!(!is_daemon_ctl(&["ls".into()]));
        assert!(!is_daemon_ctl(&["dockerfile-lint".into()]), "базовое имя, не подстрока");
        assert!(!is_daemon_ctl(&[]));
    }

    #[test]
    fn box_shell_tokens_constant() {
        let t = box_shell_tokens();
        assert_eq!(t[0], "sh");
        assert_eq!(t[1], "-c");
        // константа без пользовательского ввода — границей является контейнер
        assert_eq!(t[2], "exec bash 2>/dev/null || exec sh");
    }

    #[test]
    fn docker_probe_without_docker_honest_error() {
        // подменяем бинарник на заведомо нерабочий — проба обязана честно упасть
        std::env::set_var("POLER_BOX_DOCKER", "/bin/false");
        let r = docker_probe();
        std::env::remove_var("POLER_BOX_DOCKER");
        assert!(r.is_err(), "проба с /bin/false обязана провалиться: {r:?}");
        assert!(r.unwrap_err().contains("docker"), "ошибка упоминает docker");
    }
}
