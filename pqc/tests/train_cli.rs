//! Интеграционные тесты CLI `pqc train` (RQ8: плотный LENS-граф).
//!
//! Проверяют накопительную память движка (union support между блоками),
//! живые снапшоты отпечатка и валидность итогового контейнера.

use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn bin_path() -> PathBuf {
    env!("CARGO_BIN_EXE_pqc").into()
}

fn tmp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pqc_train_test_{name}_{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn write_corpus(dir: &std::path::Path) {
    // Два файла с пересекающейся лексикой: второй блок должен НАСЛЕДОВАТЬ
    // дуги первого (union support), а не затирать их.
    fs::write(
        dir.join("a.rs"),
        "fn phase_shift(psi: f64) -> f64 { let theta = psi.acos(); theta * 0.5 }\n".repeat(40),
    )
    .unwrap();
    fs::write(
        dir.join("b.md"),
        "lens entanglement arcs born sampling phase memory epsilon density graph topology\n"
            .repeat(40),
    )
    .unwrap();
}

#[test]
fn train_accumulates_dense_lens_graph() {
    let dir = tmp_dir("dense");
    write_corpus(&dir);
    let fp = dir.join("fingerprint.bin");
    let out = dir.join("state.pqw");

    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir.to_str().unwrap(),
            "--dim",
            "1024",
            "--epsilon",
            "0.05",
            "--block",
            "512",
            "--steps",
            "2",
            "--shots",
            "500",
            "--snapshot-every",
            "2",
            "--fingerprint",
            fp.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "stdout: {}",
        String::from_utf8_lossy(&st.stdout)
    );

    // Отпечаток: raw Packed4, размер d_pol/4.
    let fp_bytes = fs::read(&fp).unwrap();
    assert_eq!(fp_bytes.len(), 1024 / 4);

    // Контейнер: заголовок 128 B + Packed4.
    let out_bytes = fs::read(&out).unwrap();
    assert_eq!(out_bytes.len(), 128 + 1024 / 4);
    assert_eq!(&out_bytes[..8], b"POLER_Q2");
    assert_eq!(out_bytes[8], 2, "format_version v2");

    // Плотный граф: два файла дают заметно больше дуг, чем один микрочанк
    // (регрессия против затирания памяти последним чанком). nnz @ 0x60 LE.
    let nnz: u64 = fs::read(&out).unwrap()[0x60..0x68]
        .iter()
        .rev()
        .fold(0u64, |acc, &b| (acc << 8) | b as u64);
    assert!(nnz >= 8, "ожидались накопленные дуги, nnz={nnz}");
}

#[test]
fn train_json_report_is_parseable() {
    let dir = tmp_dir("json");
    write_corpus(&dir);
    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir.to_str().unwrap(),
            "--dim",
            "512",
            "--epsilon",
            "0.05",
            "--block",
            "256",
            "--steps",
            "1",
            "--shots",
            "200",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let text = String::from_utf8_lossy(&st.stdout).to_string();
    let trimmed = text.trim();
    assert!(trimmed.starts_with('{') && trimmed.ends_with('}'), "{text}");
    for key in [
        "\"command\":\"train\"",
        "\"nnz_lens\":",
        "\"support\":",
        "\"blocks\":",
        "\"tokens\":",
    ] {
        assert!(text.contains(key), "нет ключа {key} в {text}");
    }
}

#[test]
fn train_rejects_bad_args() {
    for (args, code) in [
        (vec!["train"], 2),                                     // нет источника
        (vec!["train", "--corpus", "/nonexistent-dir-xyz"], 1), // пустой корпус
        (vec!["train", "--stdin", "--epsilon", "2.0"], 2),      // ε вне [0,1]
        (vec!["train", "--stdin", "--block", "0"], 2),
        (vec!["train", "--stdin", "--dim", "0"], 2),
        (vec!["train", "--stdin", "--wat"], 2), // неизвестный флаг
    ] {
        let st = Command::new(bin_path()).args(&args).output().unwrap();
        assert_eq!(st.status.code(), Some(code), "args: {args:?}");
    }
}
