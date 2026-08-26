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
}
