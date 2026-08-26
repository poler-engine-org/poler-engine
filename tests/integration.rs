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

// ---------------------------------------------------------------------------
// Техники из исходников: ripgrep (ignore-обход), GNU grep (предфильтр),
// super-z-skills (SQL-схема memory_graph)
// ---------------------------------------------------------------------------

#[test]
fn gitignore_is_respected() {
    // WalkBuilder из крейта ignore (ripgrep): .gitignore работает даже
    // вне git-репозитория при require_git(false)
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("keep.md"),
        "# Глава 1\n\nЗдесь есть искомое слово ВЕГА.\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("skipme.md"),
        "# Глава 2\n\nА тут тоже ВЕГА, но файл в .gitignore.\n",
    )
    .unwrap();
    fs::write(dir.path().join(".gitignore"), "skipme.md\n").unwrap();

    let (_, stats) = scan_path_with_stats(dir.path(), "ВЕГА", &EngineConfig::default());
    assert_eq!(stats.files_scanned, 1, "файл из .gitignore должен быть пропущен");
    assert_eq!(stats.files_with_hits, 1);
}

#[test]
fn literal_prefilter_always_on_preserves_corpus_stats() {
    // Предфильтр (GNU grep kwset-техника) активен всегда, но статистика
    // считается по ВСЕМУ корпусу: отброшенные файлы участвуют в N_total.
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("hit.md"),
        "# Глава\n\nФункция process_data обрабатывает поток.\n",
    )
    .unwrap();
    fs::write(
        dir.path().join("miss.md"),
        "# Другая глава\n\nЗдесь нет искомого литерала совсем. Совсем другое содержимое.\n",
    )
    .unwrap();

    let (res, stats) = scan_path_with_stats(dir.path(), "process_data", &EngineConfig::default());
    assert_eq!(stats.files_scanned, 2);
    assert_eq!(stats.files_with_hits, 1);
    // оба файла в статистике корпуса
    assert!(stats.total_tokens >= 15, "total_tokens={}", stats.total_tokens);
    // якоря только из hit-файла
    assert!(res.total_hits >= 1);
    assert!(res.anchors.iter().all(|a| a.file.contains("hit.md")));
}

#[test]
fn ascii_query_case_insensitive() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("code.rs"),
        "fn ALPHA_FUNC() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    let res = scan_path(dir.path(), "alpha_func", &EngineConfig::default());
    assert!(res.total_hits >= 1, "CI-предфильтр не нашёл вхождение");
}

#[test]
fn non_ascii_query_full_path() {
    // кириллица: предфильтр через lowercase-contains, полный путь
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("ch.md"),
        "# Глава\n\nНокс действует решительно.\n",
    )
    .unwrap();
    let res = scan_path(dir.path(), "нокс", &EngineConfig::default());
    assert!(res.total_hits >= 1);
}

#[test]
fn graph_triples_budget_is_enforced() {
    let dir = write_fixture("chapter_36.md", CH36);
    let mut cfg = EngineConfig::default();
    cfg.max_graph_triples = 3;
    let (_, stats) = scan_path_with_stats(dir.path(), "нокс", &cfg);
    assert!(stats.graph_edges <= 3, "edges={}", stats.graph_edges);
}

// ---------------------------------------------------------------------------
// Watcher: инкрементальный рескан по mtime/size
// ---------------------------------------------------------------------------

#[test]
fn watcher_detects_modification_and_stability() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("scene.md"), "# Глава\n\nНокс тут.\n").unwrap();

    let mut engine = poler_engine::Engine::new(EngineConfig::default(), true);
    let (res1, _) = engine.scan(dir.path(), "нокс");
    assert_eq!(res1.total_hits, 1);

    // без изменений: пустое событие, стабильный результат
    let (ev0, res0, _) = engine.rescan(dir.path(), "нокс");
    assert!(ev0.is_empty(), "{ev0:?}");
    assert_eq!(res0.total_hits, 1);

    // модификация файла
    std::thread::sleep(std::time::Duration::from_millis(60));
    fs::write(
        dir.path().join("scene.md"),
        "# Глава\n\nНокс тут. И снова Нокс.\n",
    )
    .unwrap();
    let (ev1, res1, _) = engine.rescan(dir.path(), "нокс");
    assert_eq!(ev1.changed.len(), 1, "{ev1:?}");
    assert_eq!(res1.total_hits, 2);

    // повторный рескан без изменений — результат сохранён
    let (ev2, res2, _) = engine.rescan(dir.path(), "нокс");
    assert!(ev2.is_empty());
    assert_eq!(res2.total_hits, 2);
}

#[test]
fn watcher_add_and_remove_files() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.md"), "# A\n\nНокс первый.\n").unwrap();

    let mut engine = poler_engine::Engine::new(EngineConfig::default(), true);
    let (res, _) = engine.scan(dir.path(), "нокс");
    assert_eq!(res.total_hits, 1);

    // добавление файла
    std::thread::sleep(std::time::Duration::from_millis(60));
    fs::write(dir.path().join("b.md"), "# B\n\nНокс второй.\n").unwrap();
    let (ev, res, _) = engine.rescan(dir.path(), "нокс");
    assert_eq!(ev.added.len(), 1, "{ev:?}");
    assert_eq!(res.total_hits, 2);

    // удаление файла
    std::thread::sleep(std::time::Duration::from_millis(60));
    fs::remove_file(dir.path().join("a.md")).unwrap();
    let (ev, res, _) = engine.rescan(dir.path(), "нокс");
    assert_eq!(ev.removed.len(), 1, "{ev:?}");
    assert_eq!(res.total_hits, 1);
    assert!(res.anchors.iter().all(|a| a.file.contains("b.md")));
}

#[test]
fn watcher_stats_survive_removal() {
    // удалённый файл исключается из глобальной статистики
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.md"), "# A\n\nНокс и ещё немного слов для статистики.\n").unwrap();
    fs::write(dir.path().join("b.md"), "# B\n\nНокс и другие слова тут.\n").unwrap();

    let mut engine = poler_engine::Engine::new(EngineConfig::default(), true);
    let (_, s1) = engine.scan(dir.path(), "нокс");
    assert_eq!(s1.files_scanned, 2);

    std::thread::sleep(std::time::Duration::from_millis(60));
    fs::remove_file(dir.path().join("b.md")).unwrap();
    let (_, _, s2) = engine.rescan(dir.path(), "нокс");
    assert_eq!(s2.files_scanned, 1);
    assert!(s2.total_tokens < s1.total_tokens, "токены удалённого файла должны уйти");
}

// ---------------------------------------------------------------------------
// AIDDE: таблица символов + impact-паспорт
// ---------------------------------------------------------------------------

#[test]
fn aidde_impact_passport_upstream_downstream() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("core.rs"),
        "pub fn core_fn(x: i32) -> i32 {\n    x + 1\n}\n",
    )
    .unwrap();
    fs::write(dir.path().join("mid.rs"), "pub fn mid() {\n    core_fn(1);\n}\n").unwrap();
    fs::write(dir.path().join("top.rs"), "pub fn top() {\n    mid();\n}\n").unwrap();

    let files = vec![
        dir.path().join("core.rs"),
        dir.path().join("mid.rs"),
        dir.path().join("top.rs"),
    ];
    let table = poler_engine::aidde::SymbolTable::build(&files, 1024 * 1024);
    let report = poler_engine::aidde::impact_analysis(&table, "core_fn", 3, 100)
        .expect("impact для core_fn");

    assert!(report.file.ends_with("core.rs"));
    // upstream: mid (прямые) и top (транзитивно)
    let callers: Vec<&str> = report
        .upstream_dependents
        .iter()
        .map(|d| d.caller.as_str())
        .collect();
    assert!(callers.contains(&"mid::mid"), "{callers:?}");
    assert!(callers.contains(&"top::top"), "{callers:?}");
    // 2 файла затронуты
    assert!(report.danger_level_if_modified.contains("MEDIUM"), "{}", report.danger_level_if_modified);

    // downstream от top: mid и core_fn
    let down = poler_engine::aidde::impact_analysis(&table, "top", 3, 100).unwrap();
    let callees: Vec<&str> = down
        .downstream_dependencies
        .iter()
        .map(|d| d.callee.as_str())
        .collect();
    assert!(callees.contains(&"mid"), "{callees:?}");
    assert!(callees.contains(&"core_fn"), "{callees:?}");
}

#[test]
fn aidde_side_effects_reported() {
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("sys.rs"),
        "pub fn alloc_buffer(n: usize) -> Vec<u8> {\n    unsafe { GLOBAL_GAUGE += 1 };\n    ALLOC_MUTEX.lock();\n    std::fs::write(\"/tmp/x\", b\"y\").ok();\n    vec![0; n]\n}\n",
    )
    .unwrap();
    let files = vec![dir.path().join("sys.rs")];
    let table = poler_engine::aidde::SymbolTable::build(&files, 1024 * 1024);
    let report = poler_engine::aidde::impact_analysis(&table, "alloc_buffer", 2, 100).unwrap();
    assert!(report.side_effects.iter().any(|s| s.contains("unsafe")), "{:?}", report.side_effects);
    assert!(report.side_effects.iter().any(|s| s.contains("мьютекса")), "{:?}", report.side_effects);
    assert_eq!(report.danger_level_if_modified, "LOW (прямых зависимых не найдено)");
}

#[test]
fn aidde_python_cross_file() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("proc.py"), "def process(data):\n    return data\n").unwrap();
    fs::write(dir.path().join("run.py"), "def run():\n    return process(1)\n").unwrap();
    let files = vec![dir.path().join("proc.py"), dir.path().join("run.py")];
    let table = poler_engine::aidde::SymbolTable::build(&files, 1024 * 1024);
    let report = poler_engine::aidde::impact_analysis(&table, "process", 2, 100).unwrap();
    assert!(
        report.upstream_dependents.iter().any(|d| d.caller == "run::run"),
        "{:?}",
        report.upstream_dependents
    );
}

#[test]
fn aidde_missing_symbol() {
    let table = poler_engine::aidde::SymbolTable::build(&[], 1024 * 1024);
    assert!(poler_engine::aidde::impact_analysis(&table, "nope", 2, 10).is_none());
}

#[test]
fn hidden_files_skipped_by_default() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("visible.md"), "# Глава\n\nВЕГА видима.\n").unwrap();
    fs::create_dir(dir.path().join(".secret")).unwrap();
    fs::write(dir.path().join(".secret/hidden.md"), "# Глава\n\nВЕГА скрыта.\n").unwrap();

    let (_, stats) = scan_path_with_stats(dir.path(), "ВЕГА", &EngineConfig::default());
    assert_eq!(stats.files_scanned, 1, "скрытые файлы пропускаются по умолчанию");

    let mut cfg = EngineConfig::default();
    cfg.include_hidden = true;
    let (_, stats2) = scan_path_with_stats(dir.path(), "ВЕГА", &cfg);
    assert_eq!(stats2.files_scanned, 2, "--hidden включает скрытые");
}

// ---------------------------------------------------------------------------
// Регрессия bug: гигантская строка-простыня с «Субъекты:» внутри (Eteryya)
// ---------------------------------------------------------------------------

#[test]
fn megabyte_line_with_subjects_marker_does_not_explode() {
    // Реальный кейс Eteryya: строка 1.7 МБ содержит «- Персонажи:» в глубине
    // текста с тысячами запятых. Прежний light_meta забирал остаток строки
    // после двоеточия как значение → 9315 «субъектов» → OOM.
    let dir = TempDir::new().unwrap();
    let mut giant = String::with_capacity(1_200_000);
    giant.push_str("# Дамп\n\n## Начало\n\n");
    giant.push_str("Обычный текст начала сцены и нокс. ");
    // мегабайтная простыня без переносов строк
    for i in 0..15_000 {
        giant.push_str(&format!("Сегмент {i} повествования, "),
        );
    }
    giant.push_str(" Персонажи: а, б, в, г, д, е, ж, з, и, к, л, м, н, о, п, р, с, т, ");
    for i in 0..20_000 {
        giant.push_str(&format!("имя{i}, "));
    }
    giant.push_str("конец гигантской строки.\n");
    giant.push_str("\nФинальный абзац с нокс.\n");
    fs::write(dir.path().join("giant.md"), &giant).unwrap();

    let (res, stats) = scan_path_with_stats(dir.path(), "нокс", &EngineConfig::default());
    assert!(res.total_hits >= 1);
    // граф не взорвался: разумное число рёбер (кап 256 троек на сцену)
    assert!(stats.graph_edges < 300, "edges={}", stats.graph_edges);
    // субъекты не распухли: карточка ищется только в первых 8 КБ сцены
    let best = &res.anchors[0];
    assert!(best.scene.subjects.len() <= 1);
    if let Some(s) = best.scene.subjects.first() {
        assert!(s.len() < 600, "subjects len={}", s.len());
    }
}

#[test]
fn subjects_value_capped_even_at_scene_start() {
    // «Субъекты:» в ПЕРВОЙ строке сцены — значение обрезается до 256 байт
    let dir = TempDir::new().unwrap();
    let mut head = String::from("# Глава\n\n**Субъекты: ");
    for i in 0..500 {
        head.push_str(&format!("персонаж{i}, "));
    }
    head.push_str("**\n\nНокс действует.\n");
    fs::write(dir.path().join("sub.md"), &head).unwrap();

    let res = scan_path(dir.path(), "нокс", &EngineConfig::default());
    assert!(res.total_hits >= 1);
    let s = &res.anchors[0].scene.subjects;
    assert!(!s.is_empty());
    assert!(s[0].len() < 400, "len={}", s[0].len());
}

#[test]
fn watcher_diff_mode_returns_only_new_anchors() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.md"), "# A\n\nНокс первый.\n").unwrap();
    fs::write(dir.path().join("b.md"), "# B\n\nДругой текст.\n").unwrap();

    let mut engine = poler_engine::Engine::new(EngineConfig::default(), true).with_diff(true);
    let (res1, _) = engine.scan(dir.path(), "нокс");
    assert_eq!(res1.total_hits, 1);
    assert!(res1.anchors.iter().all(|a| a.file.contains("a.md")));

    // без изменений — пустой дифф
    let (_, res0, _) = engine.rescan(dir.path(), "нокс");
    assert_eq!(res0.anchors.len(), 0, "дифф без изменений должен быть пуст");

    // b.md модифицируется: появляется Нокс
    std::thread::sleep(std::time::Duration::from_millis(60));
    fs::write(dir.path().join("b.md"), "# B\n\nТеперь и Нокс здесь.\n").unwrap();
    let (_, res2, _) = engine.rescan(dir.path(), "нокс");
    // только новый якорь b.md; a.md не повторяется
    assert_eq!(res2.anchors.len(), 1, "{:?}", res2.anchors);
    assert!(res2.anchors[0].file.contains("b.md"));

    // повторный rescan без изменений — снова пусто (ключи запомнились)
    let (_, res3, _) = engine.rescan(dir.path(), "нокс");
    assert_eq!(res3.anchors.len(), 0);
}

#[test]
fn watcher_diff_mode_new_file() {
    let dir = TempDir::new().unwrap();
    fs::write(dir.path().join("a.md"), "# A\n\nНокс.\n").unwrap();
    let mut engine = poler_engine::Engine::new(EngineConfig::default(), true).with_diff(true);
    let _ = engine.scan(dir.path(), "нокс");

    std::thread::sleep(std::time::Duration::from_millis(60));
    fs::write(dir.path().join("new.md"), "# N\n\nНокс новый.\n").unwrap();
    let (_, res, _) = engine.rescan(dir.path(), "нокс");
    assert_eq!(res.anchors.len(), 1);
    assert!(res.anchors[0].file.contains("new.md"));
}

// ---------------------------------------------------------------------------
// POLER[Ψ] и параллельные гиганты (v0.4)
// ---------------------------------------------------------------------------

#[test]
fn psi_mode_ranks_hits() {
    // POLER[Ψ]: каноническое уравнение внимания из POLER-Quantum
    let dir = write_fixture("chapter_36.md", CH36);
    let mut cfg = EngineConfig::default();
    cfg.resonance_mode = poler_engine::ResonanceMode::Psi;
    let res = scan_path(dir.path(), "нокс", &cfg);
    assert!(res.total_hits >= 3);
    for a in &res.anchors {
        // ψ-резонанс ограничен перцептивным пространством Ω = tanh ∈ (−1,1)
        assert!(a.resonance.abs() <= 1.0 + 1e-9, "R={}", a.resonance);
        assert!(a.epsilon > 0.0);
    }
    // сортировка по ψ сохраняется
    for w in res.anchors.windows(2) {
        assert!(w[0].resonance >= w[1].resonance);
    }
}

#[test]
fn giant_chunked_equals_sequential_results() {
    // Эквивалентность: гигантский файл, обработанный чанками параллельно,
    // даёт те же сцены/хиты, что и малый файл с тем же содержимым
    // (порог GIANT_FILE_BYTES = 8 МБ).
    let dir = TempDir::new().unwrap();
    let mut text = String::with_capacity(9 * 1024 * 1024);
    text.push_str("# Документ\n\n");
    // ~8.5 МБ текста с абзацами и несколькими вхождениями
    for i in 0..90_000 {
        text.push_str(&format!(
            "Абзац {i} обычного текста с разными словами и смыслами. Слово ещё. Конец.\n\n"
        ));
    }
    // вхождения в разных частях гиганта
    text.push_str("## Раздел с нокс\n\nЗдесь Нокс упоминается впервые.\n\n");
    for i in 0..90_000 {
        text.push_str(&format!(
            "Второй блок абзацев {i} после раздела. Другие слова. Тоже конец.\n\n"
        ));
    }
    text.push_str("## Финал\n\nФинальный абзац с Нокс.\n\n");
    fs::write(dir.path().join("giant.md"), &text).unwrap();
    assert!(text.len() >= 9 * 1024 * 1024, "размер {}", text.len());

    let res = scan_path(dir.path(), "нокс", &EngineConfig::default());
    assert!(res.total_hits >= 3, "total_hits={}", res.total_hits);
    // сцены найдены и содержат вхождения из разных частей гиганта
    let scopes: Vec<&str> = res
        .anchors
        .iter()
        .map(|a| a.scene.enclosing_scope.as_str())
        .collect();
    assert!(
        scopes.iter().any(|s| s.contains("Нокс упоминается")),
        "нет средней сцены"
    );
    assert!(
        scopes.iter().any(|s| s.contains("Финальный абзац")),
        "нет финальной сцены: первые 200 символов {:?}",
        scopes.first().map(|s| s.chars().take(200).collect::<String>())
    );
}

#[test]
fn psi_params_from_cli_defaults_match_poler_quantum() {
    // Значения по умолчанию — точно из POLER_Psi_v3.py
    let p = poler_engine::psi::PsiParams::default();
    assert_eq!(p.eta, 0.05);
    assert_eq!(p.gamma, 0.5);
    assert_eq!(p.rho, 0.9);
    assert_eq!(p.memory_depth, 8);
}

// ---------------------------------------------------------------------------
// Канонический POLER-цикл из P3_Engine (p3_poler.zig, Kotokvit)
// ---------------------------------------------------------------------------

#[test]
fn poler_mode_ranks_hits() {
    // p_new = p − η·Π_Λ(D·p + γ·J·p + ∇F) с CORDIC-ренормализацией
    let dir = write_fixture("chapter_36.md", CH36);
    let mut cfg = EngineConfig::default();
    cfg.resonance_mode = poler_engine::ResonanceMode::Poler;
    let res = scan_path(dir.path(), "нокс", &cfg);
    assert!(res.total_hits >= 3);
    for a in &res.anchors {
        // POLER-амплитуда ограничена CORDIC-нормализацией на S¹
        assert!(a.resonance.is_finite() && a.resonance >= 0.0, "R={}", a.resonance);
        assert!(a.epsilon > 0.0);
    }
    for w in res.anchors.windows(2) {
        assert!(w[0].resonance >= w[1].resonance);
    }
}

#[test]
fn poler_dissipator_distinguishes_sparse_and_dense_hits() {
    // Диссипатор D=LLᵀ — энтропийный горел: при серии наблюдений
    // внимание накапливается, при паузе — сгорает. Плотная серия
    // наблюдений даёт больший POLER-резонанс, чем разовая вспышка.
    let dir = TempDir::new().unwrap();
    let mut dense = String::from("# Плотная сцена\n\n");
    for i in 0..12 {
        dense.push_str(&format!("Абзац {i}: нокс упоминается здесь.\n\n"));
    }
    fs::write(dir.path().join("dense.md"), &dense).unwrap();

    let mut sparse = String::from("# Разреженная сцена\n\n");
    sparse.push_str("Долгое вступление без искомого слова. ");
    sparse.push_str("Много других слов и предложений для объёма текста. ");
    sparse.push_str("И только один раз — нокс.\n\n");
    fs::write(dir.path().join("sparse.md"), &sparse).unwrap();

    let mut cfg = EngineConfig::default();
    cfg.resonance_mode = poler_engine::ResonanceMode::Poler;
    cfg.top_n = 20;
    let res = scan_path(dir.path(), "нокс", &cfg);

    let dense_max = res
        .anchors
        .iter()
        .filter(|a| a.file.contains("dense"))
        .map(|a| a.resonance)
        .fold(0.0, f64::max);
    let sparse_max = res
        .anchors
        .iter()
        .filter(|a| a.file.contains("sparse"))
        .map(|a| a.resonance)
        .fold(0.0, f64::max);
    assert!(
        dense_max > sparse_max,
        "диссипатор не различает плотность: dense={dense_max} sparse={sparse_max}"
    );
}

#[test]
fn sqlite_impact_reuse_skips_rebuild() {
    use poler_engine::aidde::{impact_analysis_sqlite, SymbolStore};
    let dir = TempDir::new().unwrap();
    fs::write(
        dir.path().join("a.rs"),
        "pub fn core_fn(x: i32) -> i32 {\n    x + 1\n}\n",
    )
    .unwrap();
    fs::write(dir.path().join("b.rs"), "pub fn mid() {\n    core_fn(1);\n}\n").unwrap();
    let files = vec![dir.path().join("a.rs"), dir.path().join("b.rs")];
    let db = dir.path().join("sym.db");

    // первая сборка
    let mut s1 = SymbolStore::open(&db).unwrap();
    s1.build(&files, 1024 * 1024).unwrap();
    let (d1, c1) = s1.stats();
    drop(s1);

    // Изменим исходники: reuse обязан проигнорировать содержимое
    fs::write(dir.path().join("b.rs"), "pub fn other() {\n    nothing();\n}\n").unwrap();

    // reuse: схема заполнена -> перестройки нет, данные прежние
    let (s2, has) = SymbolStore::open_existing(&db).unwrap();
    assert!(has, "reuse должен увидеть заполненную схему");
    let (d2, c2) = s2.stats();
    assert_eq!((d1, c1), (d2, c2), "reuse не должен перестраивать");
    // старые вызовы доступны
    assert!(!s2.calls_of_callee("core_fn").is_empty());
    drop(s2);

    // impact по reuse-базе работает
    let (s3, _) = SymbolStore::open_existing(&db).unwrap();
    assert!(impact_analysis_sqlite(&s3, "core_fn", 2, 100).is_some());
}
