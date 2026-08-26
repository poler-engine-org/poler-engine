//! # POLER-Engine
//!
//! AI-Native поисково-аналитический движок, вытесняющий `grep`/`ripgrep`
//! и слепой векторный RAG из архитектуры LLM-агентов.
//!
//! ## Что устраняется
//!
//! | Проблема grep/RAG | Механизм POLER |
//! |---|---|
//! | Graph Blindness | Enclosing Scope + K-hop подграф связей |
//! | Cosine Collapse / Negation Blindness | Exact Lexical Anchors + маркеры отрицаний в ε |
//! | Chunk Fragmentation | Сцены/функции целиком как границы окон |
//! | BM25/TF-IDF частотная ловушка | Информационная плотность ε на локальной энтропии |
//! | Temporal Blindness | Temporal Metric Tagging + фильтрация графа |
//!
//! ## Пайплайн
//!
//! ```text
//! files ──► [Pass A, rayon] mmap → PII-mask → tokenize → inverted index
//!              │                    (глобальные частоты токенов корпуса)
//!              ▼
//!        merge global stats (N_total, freq)
//!              ▼
//!        [Pass B, rayon] для файлов с совпадениями:
//!              окна W_t → ε (энтропия + semantic bonus)
//!                       → IIR-резонанс R_t = ε_t + φ·R_{t-1}
//!                       → SceneContext / CodeScope
//!                       → SVO-тройки
//!              ▼
//!        EntityGraph (petgraph) → K-hop BFS от корневой сущности
//!              ▼
//!        temporal-фильтр → сортировка по R → top_n → ContextAnchor JSON
//! ```
//!
//! Чтение файлов — через `mmap` (zero-copy I/O); очистка PII возвращает
//! `Cow::Borrowed` (ноль аллокаций на чистом тексте).

pub mod graph;
pub mod output;
pub mod parser;
pub mod resonance;
pub mod tokenizer;

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Instant;

use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use memmap2::Mmap;
use rayon::prelude::*;
use ignore::WalkBuilder;

pub use graph::EntityGraph;
pub use output::{render_markdown, render_simple, ContextAnchor, SearchResult};
pub use parser::{
    detect_lang, extract_code_triples, extract_enclosing_scope, extract_triples, CodeLang,
    CodeScope, SceneContext, Triple,
};
pub use resonance::{apply_iir_resonance, calculate_epsilon, semantic_bonus, IirFilter, SlidingEpsilon};
pub use tokenizer::{InvertedIndex, PiiCleaner, PiiMode};

/// Директории, исключаемые из обхода (аналог .gitignore-lite).
pub const SKIP_DIRS: &[&str] = &[".git", "target", "node_modules", ".svn", ".hg", "__pycache__"];

/// Расширения, сканируемые по умолчанию.
pub const DEFAULT_EXTENSIONS: &str =
    "md,markdown,txt,rst,rs,py,c,h,cpp,hpp,cc,js,jsx,ts,tsx,java,go,kt,swift,cs,scala,dart,json,toml,yaml,yml,sql,sh";

/// Режим накопления резонанса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResonanceMode {
    /// IIR по последовательности совпадений (ε окна каждого хита).
    Hits,
    /// Поле резонанса по всему документу: ε на каждой позиции (O(N),
    /// инкрементальное скользящее окно), IIR по всем позициям, сэмплирование
    /// в точках совпадений. Семантический бонус не начисляется.
    Field,
}

/// Конфигурация движка.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Радиус окна в токенах (полуокно).
    pub window_radius: usize,
    /// Коэффициент затухания IIR-резонанса φ ∈ [0.75, 0.90].
    pub phi_decay: f64,
    /// Масштабный коэффициент калибровки ε.
    pub kappa: f64,
    /// Число возвращаемых якорей.
    pub top_n: usize,
    /// Глубина K-hop обхода графа.
    pub k_hop_depth: usize,
    /// Режим очистки PII.
    pub pii_mode: PiiMode,
    /// Режим резонанса.
    pub resonance_mode: ResonanceMode,
    /// Temporal-фильтр (например, `Т-23`).
    pub temporal_filter: Option<String>,
    /// Считать ε по статистикам файла, а не корпуса.
    pub local_stats: bool,
    /// Сканируемые расширения (lowercase, без точек).
    pub extensions: Vec<String>,
    /// Максимальный размер файла в байтах.
    pub max_file_bytes: u64,
    /// Максимальная длина enclosing_scope в байтах.
    pub max_scope_bytes: usize,
    /// Максимум K-hop отношений на якорь.
    pub max_relations: usize,
    /// Литеральный SIMD-предфильтр (GNU grep/ripgrep техника): файлы без
    /// ASCII-литерала запроса не токенизируются вовсе. Статистика ε тогда
    /// считается по matched-подмножеству, а не по всему корпусу.
    pub prefilter: bool,
    /// Обход скрытых файлов (аналог rg --hidden).
    pub include_hidden: bool,
    /// Дамп графа сущностей в SQL-файл (схема super-z memory_graph).
    pub graph_export: Option<PathBuf>,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            window_radius: 40,
            phi_decay: 0.85,
            kappa: 1.0,
            top_n: 10,
            k_hop_depth: 2,
            pii_mode: PiiMode::Mask,
            resonance_mode: ResonanceMode::Hits,
            temporal_filter: None,
            local_stats: false,
            extensions: DEFAULT_EXTENSIONS
                .split(',')
                .map(|s| s.trim().to_lowercase())
                .collect(),
            max_file_bytes: 32 * 1024 * 1024,
            max_scope_bytes: 16 * 1024,
            max_relations: 64,
            prefilter: false,
            include_hidden: false,
            graph_export: None,
        }
    }
}

/// Статистика прогона (для `--verbose`).
#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub files_scanned: usize,
    pub files_with_hits: usize,
    pub total_tokens: u64,
    pub total_hits: usize,
    pub graph_nodes: usize,
    pub graph_edges: usize,
    pub elapsed_ms: u128,
}

// ---------------------------------------------------------------------------
// Чтение файлов: mmap + UTF-8 + PII, без лишних копий
// ---------------------------------------------------------------------------

/// Открывает файл через mmap и вызывает `f(text)`.
/// Невалидный UTF-8 конвертируется lossy; пустые файлы дают `""`.
fn with_text<T>(path: &Path, max_bytes: u64, f: impl FnOnce(&str) -> T) -> Option<T> {
    let file = File::open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    if meta.len() == 0 {
        return Some(f(""));
    }
    let mmap = unsafe { Mmap::map(&file) }.ok()?;
    let bytes = &mmap[..];
    let lossy;
    let text = match std::str::from_utf8(bytes) {
        Ok(s) => s,
        Err(_) => {
            lossy = String::from_utf8_lossy(bytes).into_owned();
            lossy.as_str()
        }
    };
    Some(f(text))
}

/// Применяет PII-маскирование, возвращая эффективный текст.
fn effective_text<'a>(raw: &'a str, config: &EngineConfig, cleaner: &PiiCleaner) -> Cow<'a, str> {
    match config.pii_mode {
        PiiMode::Off => Cow::Borrowed(raw),
        PiiMode::Mask => cleaner.clean(raw),
    }
}

// ---------------------------------------------------------------------------
// Сбор файлов
// ---------------------------------------------------------------------------

/// Литеральный SIMD-предфильтр (техника GNU grep kwset / ripgrep prefilter):
/// aho-corasick по байтам до токенизации. Применим к ASCII-запросам;
/// не-ASCII запросы проходят полный путь (регистр кириллицы меняет байты).
fn literal_ac(query_tokens: &[String]) -> Option<AhoCorasick> {
    if query_tokens.is_empty() || !query_tokens.iter().all(|t| t.is_ascii()) {
        return None;
    }
    AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .build(query_tokens)
        .ok()
}

fn collect_files(target: &Path, config: &EngineConfig) -> Vec<PathBuf> {
    if target.is_file() {
        return vec![target.to_path_buf()];
    }
    let exts: Vec<String> = config.extensions.clone();
    // WalkBuilder из крейта `ignore` (BurntSushi, ripgrep): уважение
    // .gitignore/.ignore, пропуск скрытых файлов, фильтр нежелательных директорий.
    WalkBuilder::new(target)
        .hidden(!config.include_hidden)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .require_git(false)
        .filter_entry(move |e| {
            e.file_type().map_or(true, |t| !t.is_dir()) || !SKIP_DIRS.contains(&e.file_name().to_string_lossy().as_ref())
        })
        .build()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .map(|s| exts.iter().any(|x| x == s))
                .unwrap_or(false)
        })
        .map(|e| e.into_path())
        .collect()
}

// ---------------------------------------------------------------------------
// Проход A: статистики + индексы
// ---------------------------------------------------------------------------

struct FileScan {
    /// Индекс, если в файле есть совпадения (иначе освобождён).
    index: Option<InvertedIndex>,
    /// Частоты токенов файла, если индекс освобождён.
    dropped_counts: Option<HashMap<String, usize>>,
    /// Замаскированный текст hit-файла (кэш между проходами, файлы <= 1 МБ):
    /// избавляет от повторного PII-сканирования в проходе B.
    text: Option<String>,
    total: usize,
}

/// Файлы меньше этого размера кэшируют текст между проходами.
const TEXT_CACHE_LIMIT: u64 = 1024 * 1024;

impl FileScan {
    fn empty() -> Self {
        Self {
            index: None,
            dropped_counts: None,
            text: None,
            total: 0,
        }
    }
}

fn scan_file_stats(
    path: &Path,
    query_tokens: &[String],
    config: &EngineConfig,
    cleaner: &PiiCleaner,
    literal: &Option<AhoCorasick>,
) -> FileScan {
    let size = path.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
    with_text(path, config.max_file_bytes, |raw| {
        // Литеральный SIMD-предфильтр (Commentz-Walter идея из GNU grep kwset):
        // байтовый автомат Ахо-Корасик отбраковывает файл до токенизации.
        if config.prefilter {
            if let Some(ac) = literal {
                if !ac.find_iter(raw).next().is_some() {
                    return FileScan::empty();
                }
            }
        }
        let eff = effective_text(raw, config, cleaner);
        let mut index = InvertedIndex::build(&eff);
        let hits = index.find_phrase(query_tokens);
        let total = index.total_tokens;
        if hits.is_empty() {
            let counts = std::mem::take(&mut index.token_counts);
            FileScan {
                index: None,
                dropped_counts: Some(counts),
                text: None,
                total,
            }
        } else {
            let text = if size <= TEXT_CACHE_LIMIT {
                Some(eff.into_owned())
            } else {
                None
            };
            FileScan {
                index: Some(index),
                dropped_counts: None,
                text,
                total,
            }
        }
    })
    .unwrap_or_else(FileScan::empty)
}

// ---------------------------------------------------------------------------
// Проход B: якоря
// ---------------------------------------------------------------------------

/// Лёгкая запись о совпадении: тяжёлые payload (сцены, тройки)
/// материализуются только для top-N после сортировки.
#[derive(Clone)]
struct HitRecord {
    /// Позиция в `file_hits`.
    fh_idx: usize,
    /// Глобальный индекс файла в `files`.
    file_idx: usize,
    byte_pos: usize,
    epsilon: f64,
    resonance: f64,
    scene_key: (usize, usize),
    metric_tag: Option<String>,
}

/// Тяжёлый payload уникальной сцены.
struct ScenePayload {
    scene: SceneContext,
    triples: Vec<Triple>,
    metric_tag: Option<String>,
}

/// Результат обработки файла в проходе B.
struct FileHits {
    hits: Vec<HitRecord>,
    scenes: HashMap<(usize, usize), ScenePayload>,
}

/// Тип локации сцены для отложенной материализации.
#[derive(Clone)]
enum SceneRef {
    Text(parser::markdown_scenes::SceneBounds),
    Code(usize, usize),
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

fn is_code_file(path: &Path) -> bool {
    matches!(detect_lang(path), CodeLang::Brace | CodeLang::Python)
}

/// Локализует сцену для совпадения: код (лекс-скан) либо текст (заголовки).
fn locate_scene_ref(
    text: &str,
    byte_pos: usize,
    path: &Path,
    code_file: bool,
    lang: CodeLang,
) -> Option<SceneRef> {
    if code_file {
        parser::ast_code::locate_scope(text, byte_pos, lang).map(|(b, e)| SceneRef::Code(b, e))
    } else {
        Some(SceneRef::Text(SceneContext::locate(text, byte_pos, path)))
    }
}

/// Строит payload уникальной сцены (текст либо код).
fn build_scene_payload(
    text: &str,
    scene_ref: &SceneRef,
    path: &Path,
    query_tokens: &[String],
    config: &EngineConfig,
) -> ScenePayload {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default();
    match scene_ref {
        SceneRef::Text(bounds) => {
            let mut scene = SceneContext::build(text, bounds, path);
            scene.enclosing_scope =
                parser::markdown_scenes::truncate_char_safe(&scene.enclosing_scope, config.max_scope_bytes);
            let metric = scene.metric_tag.clone();
            let triples = extract_triples(&scene.enclosing_scope, &scene, query_tokens);
            ScenePayload {
                scene,
                triples,
                metric_tag: metric,
            }
        }
        SceneRef::Code(begin, end) => {
            let scope = parser::ast_code::materialize_scope(text, *begin, *end);
            let scene = SceneContext::from_code_scope(&scope, path, config.max_scope_bytes);
            let triples = extract_code_triples(&scope.text, scope.name.as_deref(), &stem);
            ScenePayload {
                scene,
                triples,
                metric_tag: None,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Публичное API
// ---------------------------------------------------------------------------

/// Синхронное сканирование пути. См. [`scan_path_with_stats`].
pub fn scan_path(target: &Path, query: &str, config: &EngineConfig) -> SearchResult {
    scan_path_with_stats(target, query, config).0
}

/// Полный прогон движка: два прохода, граф, K-hop, temporal-фильтр.
///
/// Возвращает результат в формате ContextAnchor и статистику прогона.
pub fn scan_path_with_stats(
    target: &Path,
    query: &str,
    config: &EngineConfig,
) -> (SearchResult, ScanStats) {
    let started = Instant::now();
    let mut stats = ScanStats::default();

    let query_tokens: Vec<String> = query
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();
    if query_tokens.is_empty() || !target.exists() {
        return (
            SearchResult {
                query: query.to_string(),
                total_hits: 0,
                anchors: Vec::new(),
            },
            stats,
        );
    }

    let files = collect_files(target, config);
    stats.files_scanned = files.len();
    let cleaner = PiiCleaner::new();

    // ---------- Проход A ----------
    let literal = literal_ac(&query_tokens);
    let scans: Vec<FileScan> = files
        .par_iter()
        .map(|p| scan_file_stats(p, &query_tokens, config, &cleaner, &literal))
        .collect();

    let mut global_counts: HashMap<String, usize> = HashMap::new();
    let mut n_total: u64 = 0;
    for s in &scans {
        n_total += s.total as u64;
        let counts = s
            .index
            .as_ref()
            .map(|i| &i.token_counts)
            .or(s.dropped_counts.as_ref());
        if let Some(counts) = counts {
            for (t, c) in counts {
                *global_counts.entry(t.clone()).or_insert(0) += c;
            }
        }
    }
    stats.total_tokens = n_total;

    // ---------- Проход B ----------
    let hit_indices: Vec<usize> = scans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.index.is_some())
        .map(|(i, _)| i)
        .collect();
    stats.files_with_hits = hit_indices.len();

    let n_total_usize = n_total as usize;

    // Обработка одного hit-файла: замыкание переиспользуется и для
    // кэшированного текста (малые файлы), и для повторного чтения.
    let process_hit_file = |text: &str,
                            path: &Path,
                            fi: usize,
                            fh_pos: usize,
                            index: &InvertedIndex,
                            hits: &[usize]|
     -> FileHits {
        let (gcounts, gtotal): (&HashMap<String, usize>, usize) = if config.local_stats {
            (
                scans[fi]
                    .index
                    .as_ref()
                    .map(|i| &i.token_counts)
                    .unwrap_or(&global_counts),
                scans[fi].total,
            )
        } else {
            (&global_counts, n_total_usize)
        };

        let code_file = is_code_file(path);
        let lang = detect_lang(path);

        // ---- лёгкие записи: локализация сцены без клонирования ----
        let mut records: Vec<HitRecord> = Vec::with_capacity(hits.len());
        let mut unique_refs: HashMap<(usize, usize), SceneRef> = HashMap::new();

        let eps_res: Vec<(f64, f64)> = match config.resonance_mode {
            ResonanceMode::Hits => {
                let mut epsilons: Vec<f64> = Vec::with_capacity(hits.len());
                for &h in hits {
                    let start = h.saturating_sub(config.window_radius);
                    let end = (h + config.window_radius + 1).min(index.total_tokens);
                    let (b, e) = index.window_byte_range(start, end.max(start + 1));
                    let wtext = &text[b.min(text.len())..e.min(text.len())];
                    let bonus = semantic_bonus(wtext);
                    epsilons.push(calculate_epsilon(
                        &index.tokens[start..end],
                        &query_tokens,
                        gcounts,
                        gtotal,
                        config.kappa,
                        bonus,
                    ));
                }
                let resonances = apply_iir_resonance(&epsilons, config.phi_decay);
                epsilons.into_iter().zip(resonances).collect()
            }
            ResonanceMode::Field => {
                let mut slider = SlidingEpsilon::new(
                    index,
                    &query_tokens,
                    gcounts,
                    gtotal,
                    config.kappa,
                    config.window_radius,
                );
                let mut iir = IirFilter::new(config.phi_decay);
                let hit_set: std::collections::HashSet<usize> = hits.iter().copied().collect();
                let mut sampled: HashMap<usize, (f64, f64)> = HashMap::new();
                for center in 0..index.total_tokens {
                    let e = slider.advance_to(center, index);
                    let r = iir.push(e);
                    if hit_set.contains(&center) {
                        sampled.insert(center, (e, r));
                    }
                }
                hits.iter()
                    .map(|h| *sampled.get(h).unwrap_or(&(0.0, 0.0)))
                    .collect()
            }
        };

        for (i, &h) in hits.iter().enumerate() {
            let byte_pos = match index.positions.get(h) {
                Some(&p) => p.min(text.len()),
                None => continue,
            };
            let Some(scene_ref) = locate_scene_ref(text, byte_pos, path, code_file, lang) else {
                continue;
            };
            let scene_key = match &scene_ref {
                SceneRef::Text(b) => (b.start, b.end),
                SceneRef::Code(b, e) => (*b, *e),
            };
            unique_refs.insert(scene_key, scene_ref);
            records.push(HitRecord {
                fh_idx: fh_pos,
                file_idx: fi,
                byte_pos,
                epsilon: eps_res[i].0,
                resonance: eps_res[i].1,
                scene_key,
                metric_tag: None,
            });
        }

        // ---- уникальные сцены: тяжёлые payload один раз ----
        let mut scenes: HashMap<(usize, usize), ScenePayload> = HashMap::new();
        for (key, sref) in unique_refs {
            scenes.insert(key, build_scene_payload(text, &sref, path, &query_tokens, config));
        }
        for r in &mut records {
            if let Some(p) = scenes.get(&r.scene_key) {
                r.metric_tag = p.metric_tag.clone();
            }
        }

        FileHits {
            hits: records,
            scenes,
        }
    };

    let file_hits: Vec<FileHits> = hit_indices
        .par_iter()
        .enumerate()
        .map(|(fh_pos, &fi)| {
            let path = &files[fi];
            let scan = &scans[fi];
            let index = match scan.index.as_ref() {
                Some(i) => i,
                None => {
                    return FileHits {
                        hits: Vec::new(),
                        scenes: HashMap::new(),
                    }
                }
            };
            let hits = index.find_phrase(&query_tokens);
            if hits.is_empty() {
                return FileHits {
                    hits: Vec::new(),
                    scenes: HashMap::new(),
                };
            }
            // Кэшированный текст из прохода A (малые hit-файлы):
            // PII-маскирование не выполняется повторно.
            if let Some(text) = scan.text.as_deref() {
                return process_hit_file(text, path, fi, fh_pos, index, &hits);
            }
            with_text(path, config.max_file_bytes, |raw| {
                let eff = effective_text(raw, config, &cleaner);
                process_hit_file(&eff, path, fi, fh_pos, index, &hits)
            })
            .unwrap_or(FileHits {
                hits: Vec::new(),
                scenes: HashMap::new(),
            })
        })
        .collect();

    // ---------- сборка записей ----------
    let mut records: Vec<HitRecord> = file_hits
        .iter()
        .flat_map(|fh| fh.hits.iter().cloned())
        .collect();

    // ---------- temporal-фильтр ----------
    if let Some(filter) = &config.temporal_filter {
        records.retain(|r| r.metric_tag.as_deref().map_or(true, |m| m == filter));
    }
    stats.total_hits = records.len();

    // ---------- граф сущностей (уникальные тройки всех сцен) ----------
    let mut graph = EntityGraph::new();
    let mut seen_triples: HashSet<(String, String, String)> = HashSet::new();
    for fh in &file_hits {
        for payload in fh.scenes.values() {
            for t in &payload.triples {
                let key = (t.subject.clone(), t.predicate.clone(), t.object.clone());
                if seen_triples.insert(key) {
                    graph.add_triple(
                        &t.subject,
                        &t.predicate,
                        &t.object,
                        payload.metric_tag.as_deref(),
                        1.0,
                    );
                }
            }
        }
    }
    stats.graph_nodes = graph.node_count();
    stats.graph_edges = graph.edge_count();

    // ---------- SQL-дамп графа (схема super-z memory_graph) ----------
    if let Some(sql_path) = &config.graph_export {
        let mut sql = String::new();
        graph.export_sql(&mut sql);
        let _ = std::fs::write(sql_path, sql);
    }

    // ---------- сортировка и усечение ----------
    records.sort_by(|a, b| {
        b.resonance
            .partial_cmp(&a.resonance)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| files[a.file_idx].cmp(&files[b.file_idx]))
            .then_with(|| a.byte_pos.cmp(&b.byte_pos))
    });
    let total_hits = records.len();
    records.truncate(config.top_n);

    // ---------- K-hop: одна корневая сущность для всех якорей ----------
    let root = query_tokens[0].clone();
    let k_hop = graph.extract_k_hop(
        &root,
        config.k_hop_depth,
        config.temporal_filter.as_deref(),
        config.max_relations,
    );

    // ---------- материализация якорей (только top-N) ----------
    let anchors: Vec<ContextAnchor> = records
        .into_iter()
        .map(|r| {
            let scene = file_hits
                .get(r.fh_idx)
                .and_then(|fh| fh.scenes.get(&r.scene_key))
                .map(|p| p.scene.clone())
                .unwrap_or_else(|| SceneContext {
                    chapter: String::new(),
                    temporal_metric: None,
                    location: None,
                    subjects: Vec::new(),
                    enclosing_scope: String::new(),
                    metric_tag: None,
                    subject_names: Vec::new(),
                    subject_pairs: Vec::new(),
                });
            ContextAnchor {
                file: files[r.file_idx].to_string_lossy().to_string(),
                token: query.to_string(),
                epsilon: round2(r.epsilon),
                resonance: round2(r.resonance),
                scene,
                k_hop_relations: k_hop.clone(),
            }
        })
        .collect();

    stats.elapsed_ms = started.elapsed().as_millis();

    (
        SearchResult {
            query: query.to_string(),
            total_hits,
            anchors,
        },
        stats,
    )
}

/// Вспомогательная функция для CLI: удобная обёртка над `io::Result`.
pub fn ensure_target_exists(target: &Path) -> io::Result<()> {
    if target.exists() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("путь не найден: {}", target.display()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_sane() {
        let c = EngineConfig::default();
        assert!(c.window_radius > 0);
        assert!((0.75..=0.90).contains(&c.phi_decay));
        assert!(c.top_n > 0);
        assert!(c.k_hop_depth >= 2);
        assert!(c.extensions.contains(&"md".to_string()));
    }

    #[test]
    fn empty_query_returns_empty() {
        let res = scan_path(Path::new("/nonexistent"), "", &EngineConfig::default());
        assert_eq!(res.total_hits, 0);
        let res2 = scan_path(Path::new("/nonexistent"), "   ", &EngineConfig::default());
        assert_eq!(res2.total_hits, 0);
    }
}
