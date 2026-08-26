//! Микробенчмарк poler-engine на синтетическом корпусе.
//!
//! Запуск: `cargo run --release --example bench`
#![allow(clippy::field_reassign_with_default)]

use poler_engine::{scan_path_with_stats, EngineConfig};
use std::fs;
use std::time::Instant;
use tempfile::TempDir;

fn main() {
    let files = 400usize;
    let paragraphs = 25usize;

    let dir = TempDir::new().expect("tempdir");
    let mut total_bytes = 0usize;

    for f in 0..files {
        let mut text = format!(
            "# Глава {f}\n\n**Метрика: Т-{}**\n**Локация: Сектор {}**\n**Субъекты: Оператор, Нокс**\n\n",
            f % 50,
            f % 10
        );
        for p in 0..paragraphs {
            text.push_str(&format!(
                "Абзац {p} содержит редкие минералы кварц и базальт с индексом {}. \
                 Нокс ведёт наблюдение {p}. Система не должна превышать лимит давления. \
                 Критически важно поддерживать баланс шунта.\n\n",
                f * paragraphs + p
            ));
        }
        fs::write(dir.path().join(format!("chapter_{f:04}.md")), &text).unwrap();
        total_bytes += text.len();
    }

    let cfg = EngineConfig::default();
    println!(
        "Корпус: {files} файлов, {:.2} МБ, {} абзацев",
        total_bytes as f64 / 1024.0 / 1024.0,
        files * paragraphs
    );

    // --- прогрев (page cache) ---
    let _ = scan_path_with_stats(dir.path(), "нокс", &cfg);

    // --- замер ---
    let t = Instant::now();
    let (res, stats) = scan_path_with_stats(dir.path(), "нокс", &cfg);
    let scan_elapsed = t.elapsed();

    println!(
        "Хитов: {} (возвращено {}), токенов в корпусе: {}",
        stats.total_hits,
        res.anchors.len(),
        stats.total_tokens
    );
    println!(
        "Граф: {} узлов, {} рёбер",
        stats.graph_nodes, stats.graph_edges
    );
    println!(
        "Время: {:.1} мс | Пропускная: {:.1} МБ/с | {:.0} файлов/с",
        scan_elapsed.as_secs_f64() * 1000.0,
        total_bytes as f64 / 1024.0 / 1024.0 / scan_elapsed.as_secs_f64(),
        files as f64 / scan_elapsed.as_secs_f64()
    );

    // --- режим field (строго O(N)) ---
    let mut cfg_field = EngineConfig::default();
    cfg_field.resonance_mode = poler_engine::ResonanceMode::Field;
    let t = Instant::now();
    let (res2, stats2) = scan_path_with_stats(dir.path(), "нокс", &cfg_field);
    let field_elapsed = t.elapsed();
    println!(
        "Field-режим: {:.1} мс, хитов {}, возвращено {}",
        field_elapsed.as_secs_f64() * 1000.0,
        stats2.total_hits,
        res2.anchors.len()
    );

    // --- Разреженный корпус: техника GNU grep kwset (литеральный
    // SIMD-предфильтр до токенизации) ---
    let sparse_files = 400usize;
    let sdir = TempDir::new().expect("tempdir");
    for f in 0..sparse_files {
        // только каждый 20-й файл содержит искомый литерал
        let has_hit = f % 20 == 0;
        let mut text = format!("# Документ {f}\n\nТехническая записка номер {f}.\n\n");
        if has_hit {
            text.push_str(
                "Нокс проводит калибровку сенсоров. Система не должна превышать лимит.\n\n",
            );
        }
        text.push_str("Стандартный параграф с описанием регламента обслуживания узлов.\n\n");
        fs::write(sdir.path().join(format!("doc_{f:04}.md")), &text).unwrap();
    }
    let _ = scan_path_with_stats(sdir.path(), "нокс", &EngineConfig::default());

    let t = Instant::now();
    let (res_full, stats_full) = scan_path_with_stats(sdir.path(), "нокс", &EngineConfig::default());
    let full = t.elapsed();

    let mut cfg_fast = EngineConfig::default();
    cfg_fast.prefilter = true;
    let t = Instant::now();
    let (res_fast, stats_fast) = scan_path_with_stats(sdir.path(), "нокс", &cfg_fast);
    let fast = t.elapsed();

    println!(
        "Разреженный корпус: полный проход {:.0} мс (токенов {}), --fast {:.0} мс (токенов {}), хитов {} == {}",
        full.as_secs_f64() * 1000.0,
        stats_full.total_tokens,
        fast.as_secs_f64() * 1000.0,
        stats_fast.total_tokens,
        res_full.total_hits,
        res_fast.total_hits
    );

    // --- ASCII-запрос: предфильтр применим ---
    let t = Instant::now();
    let (_, _) = scan_path_with_stats(sdir.path(), "doc_0100", &EngineConfig::default());
    let ascii_full = t.elapsed();
    let mut cfg_fast2 = EngineConfig::default();
    cfg_fast2.prefilter = true;
    let t = Instant::now();
    let (_, _) = scan_path_with_stats(sdir.path(), "doc_0100", &cfg_fast2);
    let ascii_fast = t.elapsed();
    println!(
        "ASCII-запрос по тому же корпусу: полный {:.0} мс vs --fast {:.0} мс (ускорение {:.1}x)",
        ascii_full.as_secs_f64() * 1000.0,
        ascii_fast.as_secs_f64() * 1000.0,
        ascii_full.as_secs_f64() / ascii_fast.as_secs_f64().max(1e-9)
    );
}
