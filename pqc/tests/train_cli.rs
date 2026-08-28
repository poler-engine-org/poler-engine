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

// ─────────────────────────────────────────────────────────────────────────────
// RQ9: фазовая сборка (curriculum) + resume
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn train_resume_carries_memory_between_sessions() {
    let dir = tmp_dir("resume");
    write_corpus(&dir);
    let out1 = dir.join("session1.pqw");
    let out2 = dir.join("session2.pqw");

    // Сессия 1: первый проход.
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
            "--out",
            out1.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let text1 = String::from_utf8_lossy(&st.stdout).to_string();
    let nnz1: f64 = {
        // простейший парсер: "nnz_lens":N до запятой
        let i = text1.find("\"nnz_lens\":").unwrap() + "\"nnz_lens\":".len();
        let rest = &text1[i..];
        rest.split(',').next().unwrap().parse().unwrap()
    };
    assert!(nnz1 >= 4.0, "сессия 1 должна накопить дуги, nnz={nnz1}");

    // Сессия 2: resume + ДРУГОЙ корпус (следующая порция интернета).
    let dir2 = tmp_dir("resume2");
    fs::write(
        dir2.join("c.rs"),
        "impl fock resonance topological kernel coherence projector operator spectrum\n".repeat(40),
    )
    .unwrap();
    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir2.to_str().unwrap(),
            "--resume",
            out1.to_str().unwrap(),
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
            "--out",
            out2.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let text2 = String::from_utf8_lossy(&st.stdout).to_string();
    assert!(text2.contains("\"resume_path\""), "{text2}");
    assert!(text2.contains("\"resume_nnz\""), "{text2}");
    let nnz2: f64 = {
        let i = text2.find("\"nnz_lens\":").unwrap() + "\"nnz_lens\":".len();
        let rest = &text2[i..];
        rest.split(',').next().unwrap().parse().unwrap()
    };
    assert!(
        nnz2 >= nnz1,
        "resume обязан сохранить память: nnz {nnz1} → {nnz2}"
    );
}

#[test]
fn train_resume_rejects_dimension_mismatch() {
    let dir = tmp_dir("resume_dim");
    write_corpus(&dir);
    let out = dir.join("state.pqw");
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
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(st.status.success());

    // Resume с другой размерностью — отказ с кодом 1.
    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir.to_str().unwrap(),
            "--resume",
            out.to_str().unwrap(),
            "--dim",
            "1024",
            "--epsilon",
            "0.05",
            "--block",
            "256",
        ])
        .output()
        .unwrap();
    assert_eq!(st.status.code(), Some(1));
    let err = String::from_utf8_lossy(&st.stderr).to_string();
    assert!(err.contains("d_pol"), "{err}");
}

#[test]
fn train_curriculum_grows_chunk_and_memory() {
    let dir = tmp_dir("curriculum");
    write_corpus(&dir);
    let fp = dir.join("fingerprint.bin");

    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir.to_str().unwrap(),
            "--curriculum",
            "16:1:200,64:1:200,256:2:200",
            "--dim",
            "512",
            "--epsilon",
            "0.05",
            "--shots",
            "200",
            "--fingerprint",
            fp.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let text = String::from_utf8_lossy(&st.stdout).to_string();
    assert!(text.contains("\"curriculum\":"), "{text}");
    // Три уровня в JSON-массиве.
    let starts = text.matches("\"level\":").count();
    assert!(starts >= 3, "ожидались 3 уровня, найдено {starts}: {text}");
    assert!(text.contains("\"nnz_after\":"), "{text}");
    // Итоговый nnz > 0: память не пуста.
    let nnz: f64 = {
        let i = text.find("\"nnz_lens\":").unwrap() + "\"nnz_lens\":".len();
        let rest = &text[i..];
        rest.split(',').next().unwrap().parse().unwrap()
    };
    assert!(nnz >= 4.0, "curriculum должен собрать дуги, nnz={nnz}");
    // Отпечаток — raw Packed4: 512/4 = 128 байт.
    assert_eq!(fs::metadata(&fp).unwrap().len(), 128);
}

#[test]
fn train_curriculum_default_schedule() {
    let dir = tmp_dir("curriculum_default");
    write_corpus(&dir);
    // Без значения — расписание по умолчанию (4 уровня).
    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir.to_str().unwrap(),
            "--curriculum",
            "--dim",
            "512",
            "--epsilon",
            "0.05",
            "--shots",
            "200",
            "--steps",
            "2",
            "--block",
            "512",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let text = String::from_utf8_lossy(&st.stdout).to_string();
    assert!(text.contains("УРОВЕНЬ 1/4"), "{text}");
    assert!(text.contains("УРОВЕНЬ 4/4"), "{text}");
    assert!(text.contains("РЕГИСТРЫ"), "{text}");
    assert!(text.contains("LENS-ТОПОЛОГИЯ"), "{text}");
}

#[test]
fn train_curriculum_rejects_bad_schedule() {
    for sched in ["", "0:1", "abc", "16:1:200:64:extra"] {
        let st = Command::new(bin_path())
            .args(["train", "--stdin", "--curriculum", sched])
            .output()
            .unwrap();
        assert_eq!(
            st.status.code(),
            Some(2),
            "расписание '{sched}' должно отклоняться"
        );
    }
}

// ── RQ10: гироскоп J = A − Aᵀ и контейнер v3 ──────────────────────────

#[test]
fn train_gyro_writes_v3_container() {
    let dir = tmp_dir("gyro_v3");
    write_corpus(&dir);
    let out = dir.join("state.pqw");
    let fp = dir.join("fingerprint.bin");
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
            "512",
            "--steps",
            "1",
            "--shots",
            "300",
            "--out",
            out.to_str().unwrap(),
            "--fingerprint",
            fp.to_str().unwrap(),
            "--gyro",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let text = String::from_utf8_lossy(&st.stdout).to_string();
    assert!(text.contains("J = A − Aᵀ"), "{text}");
    assert!(text.contains("POLER_Q3"), "{text}");

    // Контейнер — валидный v3 с гироскопной секцией.
    let raw = fs::read(&out).unwrap();
    assert_eq!(&raw[..8], b"POLER_Q3");
    let reader = pqw::PqwReader::from_bytes(&raw).unwrap();
    assert!(reader.header().is_gyro());
    let g = reader.gyro().expect("секция гироскопа обязана быть");
    assert!(g.pairs().len() > 0, "циркуляция после корпуса обязана выжить");
    assert!(g.ticks() > 0);
    // Фазы читаются как в v2: nnz контейнера = дугам.
    assert_eq!(reader.nnz() as usize, reader.decoded().count());

    // Отпечаток — только фазовые блоки: размер d/4 неизменен.
    assert_eq!(fs::metadata(&fp).unwrap().len(), 512 / 4);
}

#[test]
fn train_gyro_json_stats() {
    let dir = tmp_dir("gyro_json");
    write_corpus(&dir);
    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir.to_str().unwrap(),
            "--dim",
            "256",
            "--epsilon",
            "0.05",
            "--block",
            "512",
            "--shots",
            "200",
            "--steps",
            "1",
            "--gyro",
            "128",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let text = String::from_utf8_lossy(&st.stdout).to_string();
    assert!(text.contains("\"gyro\""), "{text}");
    assert!(text.contains("\"pairs_stored\""), "{text}");
    assert!(text.contains("\"lambda_top\""), "{text}");
}

#[test]
fn train_gyro_resume_grows_ticks() {
    // Сессия 1 → v3; сессия 2 (resume) продолжает счётчик тактов.
    let dir = tmp_dir("gyro_resume");
    write_corpus(&dir);
    let out1 = dir.join("s1.pqw");
    let out2 = dir.join("s2.pqw");

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
            "512",
            "--steps",
            "1",
            "--shots",
            "300",
            "--out",
            out1.to_str().unwrap(),
            "--gyro",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let j1 = pqc::json::Json::parse(
        String::from_utf8_lossy(&st.stdout).trim(),
    )
    .unwrap();
    let g1 = j1.get("gyro").unwrap();
    let ticks1 = g1.get("ticks").unwrap().as_f64().unwrap();
    let pairs1 = g1.get("pairs_stored").unwrap().as_f64().unwrap();
    assert!(ticks1 > 0.0);
    assert!(pairs1 > 0.0);

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
            "512",
            "--steps",
            "1",
            "--shots",
            "300",
            "--out",
            out2.to_str().unwrap(),
            "--resume",
            out1.to_str().unwrap(),
            "--gyro",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let j2 = pqc::json::Json::parse(
        String::from_utf8_lossy(&st.stdout).trim(),
    )
    .unwrap();
    let g2 = j2.get("gyro").unwrap();
    let ticks2 = g2.get("ticks").unwrap().as_f64().unwrap();
    // Такты второй сессии = поднятое + новое: строго больше первой.
    assert!(
        ticks2 > ticks1,
        "такты {ticks2} должны расти поверх {ticks1}"
    );
    assert!(g2.get("pairs_stored").unwrap().as_f64().unwrap() > 0.0);
}

#[test]
fn train_gyro_rejects_bad_args() {
    for bad in [
        vec!["train", "--stdin", "--gyro", "0"],
        vec!["train", "--stdin", "--gyro", "abc"],
        vec!["train", "--stdin", "--gyro-budget", "8"],
    ] {
        let st = Command::new(bin_path()).args(&bad).output().unwrap();
        assert_eq!(
            st.status.code(),
            Some(2),
            "аргументы {bad:?} должны отклоняться"
        );
    }
}

#[test]
fn train_without_gyro_stays_v2() {
    // Регресс: без --gyro контейнер остаётся POLER_Q2 (флаг не включён).
    let dir = tmp_dir("no_gyro");
    write_corpus(&dir);
    let out = dir.join("state.pqw");
    let st = Command::new(bin_path())
        .args([
            "train",
            "--corpus",
            dir.to_str().unwrap(),
            "--dim",
            "256",
            "--epsilon",
            "0.05",
            "--block",
            "512",
            "--shots",
            "200",
            "--steps",
            "1",
            "--out",
            out.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let raw = fs::read(&out).unwrap();
    assert_eq!(&raw[..8], b"POLER_Q2");
}

// ============================================================================
// RQ15: квантованный curriculum — born-шаг в тритовой решётке (--quantized)
// ============================================================================

/// Корпус одного словаря: нет конфликтов знаков между файлами — решётка
/// сходится к стабильным полюсам (для бит-экзактных проверок resume).
fn write_uniform_corpus(dir: &std::path::Path) {
    fs::write(
        dir.join("q.txt"),
        "phase lattice born trit crystal memory\n".repeat(24),
    )
    .unwrap();
}

#[test]
fn train_quantized_crystallizes_lattice() {
    let dir = tmp_dir("q15_crystallize");
    write_uniform_corpus(&dir);
    let out = dir.join("state.pqw");

    let st = Command::new(bin_path())
        .args([
            "train",
            "--quantized",
            "--corpus",
            dir.to_str().unwrap(),
            "--dim",
            "512",
            "--epsilon",
            "0.05",
            "--block",
            "512",
            "--shots",
            "8192",
            "--steps",
            "2",
            "--out",
            out.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&st.stdout),
        String::from_utf8_lossy(&st.stderr)
    );

    let j = pqc::json::Json::parse(String::from_utf8_lossy(&st.stdout).trim()).unwrap();
    assert_eq!(j.get("mode").unwrap().as_str().unwrap(), "quantized");
    assert_eq!(j.get("command").unwrap().as_str().unwrap(), "train");
    // Кристалл двинулся: триты перещёлкнулись, дуги кристаллизовались.
    assert!(j.get("moved_total").unwrap().as_f64().unwrap() >= 1.0);
    assert!(j.get("nnz_lens").unwrap().as_f64().unwrap() >= 1.0);
    assert_eq!(j.get("lattice_bytes").unwrap().as_f64().unwrap(), 128.0);
    // Контейнер v2: заголовок 128 B + решётка бит-в-бит.
    assert_eq!(
        j.get("container_bytes").unwrap().as_f64().unwrap(),
        (128 + 512 / 4) as f64
    );
    let raw = fs::read(&out).unwrap();
    assert_eq!(raw.len(), 128 + 512 / 4);
    assert_eq!(&raw[..8], b"POLER_Q2");

    // RQ14-консистентность: pqc bloch читает квантованный чекпоинт.
    let bl = Command::new(bin_path())
        .args(["bloch", out.to_str().unwrap(), "--json"])
        .output()
        .unwrap();
    assert!(bl.status.success());
    let bj = pqc::json::Json::parse(String::from_utf8_lossy(&bl.stdout).trim()).unwrap();
    assert_eq!(bj.get("encoding").unwrap().as_str().unwrap(), "packed4");
    assert!(bj.get("density").unwrap().as_f64().unwrap() > 0.0);
}

#[test]
fn train_quantized_resume_is_bit_exact() {
    // Context-Free Resilience в одном тесте: решётка переживает рестарт
    // процесса БИТ-В-БИТ — выученное мнение не теряется, второй прогон
    // по тому же корпусу не двигает ни одного трита.
    let dir = tmp_dir("q15_resume");
    write_uniform_corpus(&dir);
    let out1 = dir.join("s1.pqw");
    let out2 = dir.join("s2.pqw");
    let base = [
        "train",
        "--quantized",
        "--corpus",
        dir.to_str().unwrap(),
        "--dim",
        "512",
        "--epsilon",
        "0.05",
        "--block",
        "512",
        "--shots",
        "8192",
        "--steps",
        "2",
    ];

    let mut args1 = base.to_vec();
    args1.push("--out");
    args1.push(out1.to_str().unwrap());
    args1.push("--json");
    let st1 = Command::new(bin_path()).args(&args1).output().unwrap();
    assert!(st1.status.success(), "stderr: {}", String::from_utf8_lossy(&st1.stderr));
    let j1 = pqc::json::Json::parse(String::from_utf8_lossy(&st1.stdout).trim()).unwrap();
    let nnz1 = j1.get("nnz_lens").unwrap().as_f64().unwrap();
    assert!(nnz1 >= 1.0, "первый прогон обязан кристаллизовать дуги");

    let mut args2 = base.to_vec();
    args2.push("--resume");
    args2.push(out1.to_str().unwrap());
    args2.push("--out");
    args2.push(out2.to_str().unwrap());
    args2.push("--json");
    let st2 = Command::new(bin_path()).args(&args2).output().unwrap();
    assert!(st2.status.success(), "stderr: {}", String::from_utf8_lossy(&st2.stderr));
    let j2 = pqc::json::Json::parse(String::from_utf8_lossy(&st2.stdout).trim()).unwrap();
    assert_eq!(
        j2.get("resume_nnz").unwrap().as_f64().unwrap(),
        nnz1,
        "поднятая решётка обязана сохранить все дуги"
    );
    assert_eq!(
        j2.get("moved_total").unwrap().as_f64().unwrap(),
        0.0,
        "выученное мнение не требует движений при повторе"
    );

    // Бит-экзактность: фазовая секция (решётка) идентична между сессиями.
    let r1 = fs::read(&out1).unwrap();
    let r2 = fs::read(&out2).unwrap();
    assert_eq!(r1.len(), r2.len());
    assert_eq!(&r1[128..], &r2[128..], "решётка изменилась после рестарта");
}

#[test]
fn train_quantized_eta_dead_zone() {
    // Физика решётки: плотный дефолт η = 0.25 не пересекает порог
    // кристаллизации (η·v* = 0.5 < π/6) — мёртвая зона честная, решётка
    // не двинется ни на один трит. Поэтому --quantized без явного --eta0
    // берёт калибровку RQ14 (η = 0.6), а ЯВНАЯ подача 0.25 даёт тишину.
    let dir = tmp_dir("q15_deadzone");
    write_uniform_corpus(&dir);
    let st = Command::new(bin_path())
        .args([
            "train",
            "--quantized",
            "--corpus",
            dir.to_str().unwrap(),
            "--dim",
            "512",
            "--epsilon",
            "0.05",
            "--block",
            "512",
            "--shots",
            "8192",
            "--steps",
            "2",
            "--eta0",
            "0.25",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(st.status.success());
    let j = pqc::json::Json::parse(String::from_utf8_lossy(&st.stdout).trim()).unwrap();
    assert_eq!(j.get("moved_total").unwrap().as_f64().unwrap(), 0.0);
    assert_eq!(j.get("nnz_lens").unwrap().as_f64().unwrap(), 0.0);
}

#[test]
fn train_quantized_rejects_gyro_and_decay() {
    for bad in [
        vec!["train", "--quantized", "--stdin", "--gyro"],
        vec!["train", "--quantized", "--stdin", "--decay"],
    ] {
        let st = Command::new(bin_path()).args(&bad).output().unwrap();
        assert_eq!(
            st.status.code(),
            Some(2),
            "аргументы {bad:?} должны отклоняться"
        );
    }
}

#[test]
fn train_quantized_curriculum_stages() {
    // Квантованная фазовая сборка: уровни меняют чанк/шаги/выстрелы,
    // решётка одна на все уровни.
    let dir = tmp_dir("q15_curriculum");
    write_uniform_corpus(&dir);
    let st = Command::new(bin_path())
        .args([
            "train",
            "--quantized",
            "--corpus",
            dir.to_str().unwrap(),
            "--dim",
            "512",
            "--epsilon",
            "0.05",
            "--curriculum",
            "64:1:512:8192,512:2:8192:0",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        st.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&st.stderr)
    );
    let j = pqc::json::Json::parse(String::from_utf8_lossy(&st.stdout).trim()).unwrap();
    assert_eq!(j.get("mode").unwrap().as_str().unwrap(), "quantized");
    let cur = j.get("curriculum").unwrap();
    assert_eq!(
        cur.as_arr().map(|a| a.len()).unwrap_or(0),
        2,
        "два уровня расписания"
    );
    assert!(j.get("nnz_lens").unwrap().as_f64().unwrap() >= 1.0);
}
