//! Интеграционные тесты CLI `pqc encrypt` / `pqc decrypt` (RQ12).
//!
//! Схема алгебры архетипа (файл 285): `p* = a ⊗_ε p* ⊕ m`,
//! восстановление `m = p* ⊕ (a ⊗_ε p*)`. Ключ — контейнер v3 с
//! гироскопом J; проектор — плотные моды Im(P) (идемпотентность
//! `a ⊗_ε a = a` точна, невязки ~1e-16).

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_pqc");

/// Делает ключ .pqw v3: d=256, путь + гребёнка (делокализованные моды).
fn make_key(path: &std::path::Path) {
    // Генерируем ключ через уже проверенный модульный API: пишем
    // контейнер v3 с гироскопом из python-free Rust-теста.
    // Здесь проще всего — зашить готовый контейнер, созданный в тесте.
    use pqw::{GyroData, PqwWriter};
    let mut w = PqwWriter::new(256).unwrap().hyperparams(0.05, 0.5, 0.75, 0.05);
    for i in 0u32..256 {
        let p = if i % 3 == 0 { -0.7 } else { 0.6 };
        w.add_phase(i, p).unwrap();
    }
    let mut pairs: Vec<(u32, u32, f64)> =
        (0u32..255).map(|i| (i, i + 1, 1.0)).collect();
    pairs.extend((0u32..252).map(|i| (i, i + 4, 0.6)));
    let gyro = GyroData::new(256, 5000, pairs, 256).unwrap();
    let bytes = w.to_bytes_v3(&gyro).unwrap();
    std::fs::write(path, bytes).unwrap();
}

fn tmp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pqc-crypto-cli-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(BIN)
        .args(args)
        .env_remove("RUST_BACKTRACE")
        .output()
        .expect("pqc binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn cli_encrypt_decrypt_roundtrip() {
    let dir = tmp_dir("roundtrip");
    let key = dir.join("key.pqw");
    make_key(&key);
    let msg = dir.join("msg.bin");
    let cipher = dir.join("msg.pqc");
    let plain = dir.join("plain.bin");
    let payload: Vec<u8> = (0..777u32).map(|i| (i * 37 + 11) as u8).collect();
    std::fs::write(&msg, &payload).unwrap();

    let (code, out, err) = run(&[
        "encrypt",
        msg.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--out",
        cipher.to_str().unwrap(),
        "--seed",
        "42",
    ]);
    assert_eq!(code, 0, "encrypt failed: {err}\n{out}");
    assert!(out.contains("RQ12"));
    assert!(out.contains("p* = a ⊗_ε p* ⊕ m"));
    assert!(cipher.exists());

    let (code, out, err) = run(&[
        "decrypt",
        cipher.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
        "--out",
        plain.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "decrypt failed: {err}\n{out}");
    assert!(out.contains("m = p* ⊕ (a ⊗_ε p*)"));
    assert_eq!(std::fs::read(&plain).unwrap(), payload);
}

#[test]
fn cli_encrypt_text_stdin_json() {
    let dir = tmp_dir("text");
    let key = dir.join("key.pqw");
    make_key(&key);
    let cipher = dir.join("t.pqc");

    // --text
    let (code, out, err) = run(&[
        "encrypt",
        "--text",
        "POLER archetype cipher",
        "--key",
        key.to_str().unwrap(),
        "--out",
        cipher.to_str().unwrap(),
        "--seed",
        "7",
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(cipher.exists());

    // stdout без --out: текст возвращается в терминал
    let (code, out, err) = run(&[
        "decrypt",
        cipher.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("POLER archetype cipher"), "out: {out}");

    // --stdin + --json
    let mut child = Command::new(BIN)
        .args([
            "encrypt",
            "--stdin",
            "--key",
            key.to_str().unwrap(),
            "--out",
            cipher.to_str().unwrap(),
            "--seed",
            "9",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"stdin message 123")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let json = String::from_utf8_lossy(&out.stdout);
    // Числа в JSON — научная запись (1.7e1); проверяем ключи.
    assert!(json.contains("\"msg_len\":"));
    assert!(json.contains("\"k_modes\":"));
    assert!(json.contains("\"digest\":\""));
}

#[test]
fn cli_wrong_key_rejected_or_garbage() {
    let dir = tmp_dir("wrongkey");
    let key_a = dir.join("a.pqw");
    let key_b = dir.join("b.pqw");
    make_key(&key_a);
    // Другой ключ: другая структура русел (сдвиги {2, 7} вместо {1, 4})
    // — другие моды и другое подпространство. (Инверсия весов НЕ годится:
    // J → −J лишь меняет местами (u, v) плоскостей — проекторы те же.)
    use pqw::{GyroData, PqwWriter};
    let mut w = PqwWriter::new(256).unwrap().hyperparams(0.05, 0.5, 0.75, 0.05);
    for i in 0u32..256 {
        let p = if i % 3 == 0 { 0.7 } else { -0.6 };
        w.add_phase(i, p).unwrap();
    }
    let mut pairs: Vec<(u32, u32, f64)> =
        (0u32..254).map(|i| (i, i + 2, 1.0)).collect();
    pairs.extend((0u32..249).map(|i| (i, i + 7, 0.8)));
    let gyro = GyroData::new(256, 5000, pairs, 256).unwrap();
    std::fs::write(&key_b, w.to_bytes_v3(&gyro).unwrap()).unwrap();

    let msg = dir.join("m.bin");
    let cipher = dir.join("m.pqc");
    std::fs::write(&msg, b"secret message for the archetype").unwrap();
    let (code, _, err) = run(&[
        "encrypt",
        msg.to_str().unwrap(),
        "--key",
        key_a.to_str().unwrap(),
        "--out",
        cipher.to_str().unwrap(),
        "--seed",
        "1",
    ]);
    assert_eq!(code, 0, "{err}");

    let plain = dir.join("p.bin");
    let (code, out, err) = run(&[
        "decrypt",
        cipher.to_str().unwrap(),
        "--key",
        key_b.to_str().unwrap(),
        "--out",
        plain.to_str().unwrap(),
    ]);
    // Чужой ключ: либо отказ (структура/ёмкость/моды), либо мусор.
    if code == 0 {
        let got = std::fs::read(&plain).unwrap();
        assert_ne!(got, b"secret message for the archetype");
        assert!(out.contains("RQ12"));
    } else {
        assert!(!err.is_empty());
    }
}

#[test]
fn cli_corruption_and_missing_args() {
    let dir = tmp_dir("errors");
    let key = dir.join("key.pqw");
    make_key(&key);
    let cipher = dir.join("c.pqc");
    let (code, _, _) = run(&[
        "encrypt",
        "--text",
        "hello",
        "--key",
        key.to_str().unwrap(),
        "--out",
        cipher.to_str().unwrap(),
        "--seed",
        "3",
    ]);
    assert_eq!(code, 0);

    // Порча: digest ловит.
    let mut bytes = std::fs::read(&cipher).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0xFF;
    std::fs::write(&cipher, bytes).unwrap();
    let (code, _, err) = run(&[
        "decrypt",
        cipher.to_str().unwrap(),
        "--key",
        key.to_str().unwrap(),
    ]);
    assert_eq!(code, 1);
    assert!(err.contains("digest"));

    // Без ключа — подсказка.
    let (code, _, err) = run(&["encrypt", "--text", "x", "--out", "/tmp/x.pqc"]);
    assert_eq!(code, 2);
    assert!(err.contains("--key"));

    // Без входа — подсказка.
    let (code, _, err) = run(&[
        "encrypt",
        "--key",
        key.to_str().unwrap(),
        "--out",
        "/tmp/x.pqc",
    ]);
    assert_eq!(code, 2);
    assert!(err.contains("--text") || err.contains("stdin"));
}
