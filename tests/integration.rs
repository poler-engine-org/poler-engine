//! Интеграционные тесты движка: полный пайплайн на фикстурах,
//! воспроизведение выходного контракта спецификации.
#![allow(clippy::field_reassign_with_default)]

use poler_engine::{
    scan_path, scan_path_with_stats, EngineConfig, PiiMode, ResonanceMode,
};
use std::fs;
use tempfile::TempDir;

const CH36: &str = include_str!("fixtures/chapter_36.md");
const CODE_RS: &str = include_str!("fixtures/example.rs");
const CODE_PY: &str = include_str!("fixtures/example.py");
const PII_TXT: &str = include_str!("fixtures/pii.txt");

fn write_fixture(name: &str, content: &str) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    fs::write(dir.path().join(name), content).expect("write fixture");
    dir
}

// ---------------------------------------------------------------------------
// Выходной контракт спецификации
// ---------------------------------------------------------------------------

#[test]
fn spec_contract_on_chapter_36() {
    let dir = write_fixture("chapter_36.md", CH36);
    let cfg = EngineConfig::default();
    let res = scan_path(dir.path(), "нокс", &cfg);

    // 3 вхождения: метаданные, "по имени Нокс", "Нокс вонзила"
    assert!(res.total_hits >= 3, "total_hits={}", res.total_hits);
    assert!(!res.anchors.is_empty());

    let json = serde_json::to_value(&res).unwrap();
    assert_eq!(json["query"], "нокс");
    assert!(json["total_hits"].as_u64().unwrap() >= 3);

    let best = &res.anchors[0];
    assert!(best.epsilon > 0.0);
    assert!(best.resonance >= best.epsilon - 0.01, "R={} < ε={}", best.resonance, best.epsilon);
    assert!(best.file.contains("chapter_36.md"));
    assert_eq!(best.token, "нокс");

    // сцена: глава / метрика / локация / субъекты / полный скоуп
    assert_eq!(best.scene.chapter, "Глава 36. Инертный");
    assert_eq!(best.scene.temporal_metric.as_deref(), Some("Метрика: Т-23"));
    assert_eq!(best.scene.location.as_deref(), Some("Локация: Разлом Каньона"));
    assert!(best.scene.subjects[0].contains("Соболь (Нокс)"));
    assert!(best.scene.enclosing_scope.contains("шунт"));
    assert!(best.scene.enclosing_scope.len() > 200, "сцена должна быть полной");

    // K-hop: график обязан быть непустым
    assert!(!best.k_hop_relations.is_empty(), "k_hop_relations пуст");
    let rels: Vec<Vec<String>> = best
        .k_hop_relations
        .iter()
        .map(|(s, p, o)| vec![s.clone(), p.clone(), o.clone()])
        .collect();
    // обе тройки из примера контракта спецификации §5
    assert!(
        rels.contains(&vec![
            "Нокс".to_string(),
            "вонзила_когти".to_string(),
            "Солнечное сплетение".to_string()
        ]),
        "нет спец-тройки 1: {rels:?}"
    );
    assert!(
        rels.contains(&vec![
            "Шунт".to_string(),
            "сбрасывает_тепло".to_string(),
            "1300°C".to_string()
        ]),
        "нет спец-тройки 2: {rels:?}"
    );
}

#[test]
fn negation_phrase_is_findable() {
    // Устранение Negation Blindness: RAG смешал бы эти фразы,
    // точный лексический поиск обязан их различать.
    let dir = write_fixture("chapter_36.md", CH36);
    let res = scan_path(dir.path(), "не должны", &EngineConfig::default());
    assert!(res.total_hits >= 1, "total_hits={}", res.total_hits);

    let res_neg = scan_path(dir.path(), "не должен", &EngineConfig::default());
    assert!(res_neg.total_hits >= 1);

    // противоположная по смыслу фраза находится отдельно
    let res_oblig = scan_path(dir.path(), "обязана", &EngineConfig::default());
    assert!(res_oblig.total_hits >= 1);

    // Exact Lexical Anchors: фразы «не обязана» в корпусе НЕТ —
    // векторный RAG с cosine > 0.94 ошибочно нашёл бы её здесь.
    let res_absent = scan_path(dir.path(), "не обязана", &EngineConfig::default());
    assert_eq!(res_absent.total_hits, 0, "ложное совпадение отсутствующей фразы");
}

#[test]
fn temporal_filter_works() {
    let dir = write_fixture("chapter_36.md", CH36);
    let mut cfg = EngineConfig::default();
    cfg.temporal_filter = Some("Т-23".to_string());
    let res = scan_path(dir.path(), "нокс", &cfg);
    assert!(res.total_hits >= 1);
    for a in &res.anchors {
        assert!(a.scene.temporal_metric.is_some());
    }

    let mut cfg2 = EngineConfig::default();
    cfg2.temporal_filter = Some("Т-99".to_string());
    let res2 = scan_path(dir.path(), "нокс", &cfg2);
    // все сцены с метрикой Т-23 отфильтрованы
    assert_eq!(res2.total_hits, 0, "total_hits={}", res2.total_hits);
}

#[test]
fn top_n_truncates_but_total_hits_is_honest() {
    let dir = write_fixture("chapter_36.md", CH36);
    let mut cfg = EngineConfig::default();
    cfg.top_n = 1;
    let res = scan_path(dir.path(), "нокс", &cfg);
    assert_eq!(res.anchors.len(), 1);
    assert!(res.total_hits > 1);
}

#[test]
fn deterministic_output() {
    let dir = write_fixture("chapter_36.md", CH36);
    let cfg = EngineConfig::default();
    let a = scan_path(dir.path(), "нокс", &cfg);
    let b = scan_path(dir.path(), "нокс", &cfg);
    assert_eq!(
        serde_json::to_string(&a).unwrap(),
        serde_json::to_string(&b).unwrap()
    );
}

// ---------------------------------------------------------------------------
// Код: enclosing scope + call graph
// ---------------------------------------------------------------------------

#[test]
fn rust_code_scope_and_call_graph() {
    let dir = write_fixture("example.rs", CODE_RS);
    let cfg = EngineConfig::default();
    let res = scan_path(dir.path(), "compute", &cfg);

    assert!(res.total_hits >= 1);
    let scope_names: Vec<&str> = res
        .anchors
        .iter()
        .map(|a| a.scene.enclosing_scope.as_str())
        .collect();
    // каждое совпадение живёт в полном теле функции, а не в строке
    assert!(
        scope_names
            .iter()
            .any(|s| s.contains("fn process_data")),
        "нет скоупа process_data: {scope_names:?}"
    );

    // call graph: process_data -> compute
    let flat: Vec<String> = res
        .anchors
        .iter()
        .flat_map(|a| a.k_hop_relations.iter())
        .map(|(s, p, o)| format!("{s}|{p}|{o}"))
        .collect();
    assert!(
        flat.iter().any(|s| s.contains("вызывает|compute")),
        "нет call graph: {flat:?}"
    );
}

#[test]
fn python_scope_by_indent() {
    let dir = write_fixture("example.py", CODE_PY);
    let res = scan_path(dir.path(), "transform", &EngineConfig::default());
    assert!(res.total_hits >= 1);
    assert!(
        res.anchors
            .iter()
            .any(|a| a.scene.enclosing_scope.contains("def run")
                || a.scene.enclosing_scope.contains("def transform"))
    );
}

// ---------------------------------------------------------------------------
// PII
// ---------------------------------------------------------------------------

#[test]
fn pii_masked_by_default() {
    let dir = write_fixture("pii.txt", PII_TXT);
    let res = scan_path(dir.path(), "отчёт", &EngineConfig::default());
    assert!(res.total_hits >= 1);
    let scope = &res.anchors[0].scene.enclosing_scope;
    assert!(scope.contains("[EMAIL]"), "scope={scope}");
    assert!(!scope.contains("user@example.com"));
}

#[test]
fn pii_off_keeps_original() {
    let dir = write_fixture("pii.txt", PII_TXT);
    let mut cfg = EngineConfig::default();
    cfg.pii_mode = PiiMode::Off;
    let res = scan_path(dir.path(), "отчёт", &cfg);
    assert!(res.anchors[0].scene.enclosing_scope.contains("user@example.com"));
}

#[test]
fn secrets_are_masked() {
    let dir = write_fixture("pii.txt", PII_TXT);
    let res = scan_path(dir.path(), "скомпрометирован", &EngineConfig::default());
    let scope = &res.anchors[0].scene.enclosing_scope;
    assert!(scope.contains("[SECRET]"), "scope={scope}");
    assert!(!scope.contains("sk-abcdef"));
}

// ---------------------------------------------------------------------------
// Резонанс и статистика
// ---------------------------------------------------------------------------

#[test]
fn field_resonance_mode_produces_results() {
    let dir = write_fixture("chapter_36.md", CH36);
    let mut cfg = EngineConfig::default();
    cfg.resonance_mode = ResonanceMode::Field;
    let res = scan_path(dir.path(), "нокс", &cfg);
    assert!(res.total_hits >= 3);
    for a in &res.anchors {
        assert!(a.resonance > 0.0);
        assert!(a.epsilon > 0.0);
    }
}

#[test]
fn resonance_accumulates_along_document() {
    // IIR накапливает R по документу: при 3+ совпадениях в одном файле
    // разброс резонансов строго положителен (поздние хиты резонируют сильнее),
    // а монотонность самого фильтра покрыта unit-тестами iir_filter.
    let dir = write_fixture("chapter_36.md", CH36);
    let cfg = EngineConfig::default();
    let res = scan_path(dir.path(), "нокс", &cfg);
    assert!(res.total_hits >= 3);
    let rs: Vec<f64> = res.anchors.iter().map(|a| a.resonance).collect();
    let max = rs.iter().cloned().fold(f64::MIN, f64::max);
    let min = rs.iter().cloned().fold(f64::MAX, f64::min);
    assert!(max > min, "резонансы одинаковы: {rs:?}");
    // результат отсортирован по убыванию R
    for w in res.anchors.windows(2) {
        assert!(w[0].resonance >= w[1].resonance);
    }
}

#[test]
fn verbose_stats_reported() {
    let dir = write_fixture("chapter_36.md", CH36);
    let (_, stats) = scan_path_with_stats(dir.path(), "нокс", &EngineConfig::default());
    assert_eq!(stats.files_scanned, 1);
    assert_eq!(stats.files_with_hits, 1);
    assert!(stats.total_tokens > 50);
    assert!(stats.graph_nodes > 3);
    assert!(stats.graph_edges > 3);
}

#[test]
fn multi_file_corpus_and_ranking() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("chapter_36.md"),
        CH36,
    )
    .unwrap();
    fs::write(
        dir.path().join("chapter_37.md"),
        "# Глава 37. Отражение\n\n**Метрика: Т-24**\n\nНокс снова появилась на горизонте.\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("notes.txt"),
        "техническая записка без искомого слова\n",
    )
    .unwrap();

    let (res, stats) = scan_path_with_stats(dir.path(), "нокс", &EngineConfig::default());
    assert_eq!(stats.files_scanned, 3);
    assert_eq!(stats.files_with_hits, 2);
    assert!(res.total_hits >= 4);
    // резонансы отсортированы по убыванию
    for w in res.anchors.windows(2) {
        assert!(w[0].resonance >= w[1].resonance);
    }
}

#[test]
fn local_stats_mode_works() {
    let dir = write_fixture("chapter_36.md", CH36);
    let mut cfg = EngineConfig::default();
    cfg.local_stats = true;
    let res = scan_path(dir.path(), "нокс", &cfg);
    assert!(res.total_hits >= 3);
    assert!(res.anchors[0].epsilon > 0.0);
}
