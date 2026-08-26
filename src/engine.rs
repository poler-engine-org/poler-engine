//! Ядро движка: полный и инкрементальный (watcher) прогоны.
//!
//! ## Потоковая архитектура памяти (исправление bug#1 из v0.2.1)
//!
//! Прежняя схема (удержание `Vec<FileScan>` с индексами всех файлов
//! одновременно) на корпусе 340 файлов / 12.4 млн токенов раздувала RSS
//! до гигабайт. Новая схема удерживает между фазами только:
//!
//! * глобальные частоты токенов (словарь корпуса — один раз);
//! * лёгкие `HitRecord` (без текста сцен);
//! * тройки уникальных сцен (`SceneInfo`).
//!
//! Всё тяжёлое (массивы токенов, тексты сцен) — временные структуры
//! одного файла, освобождаемые сразу после его обработки.

use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Instant, SystemTime};

use crate::graph::EntityGraph;
use crate::output::{ContextAnchor, SearchResult};
use crate::parser::markdown_scenes::truncate_char_safe;
use crate::parser::SceneContext;
use crate::streaming::{self, HitRecord, Pass2Result, SceneInfo};

use crate::{collect_files, with_text, EngineConfig, PiiCleaner, PiiMode, ScanStats};

/// Событие инкрементального прогона (watcher).
#[derive(Debug, Clone, Default, Serialize)]
pub struct WatchEvent {
    pub added: Vec<String>,
    pub changed: Vec<String>,
    pub removed: Vec<String>,
}

impl WatchEvent {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Кэшированное состояние одного файла для watcher-режима.
#[derive(Debug, Clone)]
struct FileEntry {
    mtime: SystemTime,
    size: u64,
    counts: HashMap<String, usize>,
    total: u64,
    hits: Vec<u32>,
    records: Vec<HitRecord>,
    scenes: HashMap<(usize, usize), SceneInfo>,
}

/// Поисковый движок с опциональным инкрементальным состоянием.
///
/// `watching = true` включает кэш по mtime/size: повторные прогоны
/// (`rescan`) не перечитывают и не ретокенизируют неизменённые файлы.
/// `diff_mode = true` (только в watcher): `rescan` возвращает якоря,
/// которых не было в предыдущем прогоне (новые file+byte_pos).
pub struct Engine {
    config: EngineConfig,
    watching: bool,
    diff_mode: bool,
    state: Option<HashMap<PathBuf, FileEntry>>,
    /// Ключи (путь, байт) всех хитов предыдущего прогона — для diff.
    last_hit_keys: HashSet<(PathBuf, usize)>,
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

impl Engine {
    pub fn new(config: EngineConfig, watching: bool) -> Self {
        Self {
            config,
            watching,
            diff_mode: false,
            state: None,
            last_hit_keys: HashSet::new(),
        }
    }

    /// Включает дифф-режим (только для watcher-прогонов).
    pub fn with_diff(mut self, on: bool) -> Self {
        self.diff_mode = on;
        self
    }

    /// Полный прогон: все файлы обрабатываются с нуля.
    pub fn scan(&mut self, target: &Path, query: &str) -> (SearchResult, ScanStats) {
        let (_ev, res, stats) = self.run(target, query, false);
        (res, stats)
    }

    /// Инкрементальный прогон: обрабатываются только добавленные,
    /// изменённые (mtime/size) файлы; удалённые исключаются из статистики.
    pub fn rescan(&mut self, target: &Path, query: &str) -> (WatchEvent, SearchResult, ScanStats) {
        self.run(target, query, true)
    }

    fn run(
        &mut self,
        target: &Path,
        query: &str,
        incremental: bool,
    ) -> (WatchEvent, SearchResult, ScanStats) {
        let started = Instant::now();
        let mut stats = ScanStats::default();
        let query_tokens: Vec<String> = query
            .split_whitespace()
            .map(|w| w.to_lowercase())
            .collect();
        if query_tokens.is_empty() || !target.exists() {
            return (
                WatchEvent::default(),
                SearchResult {
                    query: query.to_string(),
                    total_hits: 0,
                    anchors: Vec::new(),
                },
                stats,
            );
        }

        let config = self.config.clone();
        let watching = self.watching;
        let cleaner = PiiCleaner::new();
        let files = collect_files(target, &config);
        stats.files_scanned = files.len();
        let ac = streaming::literal_ac(&query_tokens);

        // ---------- классификация файлов (incremental) ----------
        let prev = std::mem::take(&mut self.state).unwrap_or_default();
        let mut event = WatchEvent::default();
        let mut kept: HashMap<PathBuf, FileEntry> = HashMap::new();
        let mut to_process: Vec<PathBuf> = Vec::new();

        if incremental {
            let current: HashSet<&PathBuf> = files.iter().collect();
            for (p, e) in prev {
                if current.contains(&p) {
                    let same = fs::metadata(&p)
                        .map(|m| m.len() == e.size && m.modified().ok() == Some(e.mtime))
                        .unwrap_or(false);
                    if same {
                        kept.insert(p, e);
                    } else {
                        to_process.push(p.clone());
                        event.changed.push(p.to_string_lossy().to_string());
                    }
                } else {
                    event.removed.push(p.to_string_lossy().to_string());
                }
            }
            let mut scheduled: HashSet<PathBuf> = kept.keys().cloned().collect();
            scheduled.extend(to_process.iter().cloned());
            for p in &files {
                if !scheduled.contains(p) {
                    to_process.push(p.clone());
                    event.added.push(p.to_string_lossy().to_string());
                }
            }
        } else {
            to_process = files.clone();
        }

        // ---------- Проход 1: потоковая статистика ----------
        struct PassSink {
            global: HashMap<String, usize>,
            n_total: u64,
            hit_files: HashMap<PathBuf, Vec<u32>>,
            entries: HashMap<PathBuf, FileEntry>,
        }

        impl PassSink {
            fn absorb(&mut self, path: &Path, r: streaming::Pass1File, watching: bool) {
                let meta = fs::metadata(path).ok();
                let mtime = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .unwrap_or(SystemTime::UNIX_EPOCH);
                let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                let mut entry_counts: Option<HashMap<String, usize>> =
                    if watching { Some(HashMap::new()) } else { None };
                for (k, v) in r.counts {
                    *self.global.entry(k.clone()).or_insert(0) += v;
                    if let Some(ec) = &mut entry_counts {
                        ec.insert(k, v);
                    }
                }
                self.n_total += r.total;
                // Entry создаётся ВСЕГДА: в него pass 2 кладёт records/scenes.
                // Словарь counts удерживается только в watcher-режиме
                // (нужен для вычитания при инкрементальном rescan).
                self.entries.insert(
                    path.to_path_buf(),
                    FileEntry {
                        mtime,
                        size,
                        counts: entry_counts.unwrap_or_default(),
                        total: r.total,
                        hits: r.hits.clone(),
                        records: Vec::new(),
                        scenes: HashMap::new(),
                    },
                );
                if !r.hits.is_empty() {
                    self.hit_files.insert(path.to_path_buf(), r.hits);
                }
            }

            fn add_kept(&mut self, e: &FileEntry) {
                for (k, v) in &e.counts {
                    *self.global.entry(k.clone()).or_insert(0) += v;
                }
                self.n_total += e.total;
            }
        }

        let mut sink = PassSink {
            global: HashMap::new(),
            n_total: 0,
            hit_files: HashMap::new(),
            entries: HashMap::new(),
        };
        for e in kept.values() {
            sink.add_kept(e);
        }

        let (giants, normals) = streaming::split_giants(&to_process);
        let sink_mu = Mutex::new(sink);
        normals.par_iter().for_each(|p| {
            if let Some(r) = streaming::pass1_file(p, &query_tokens, &config, &cleaner, &ac) {
                sink_mu.lock().unwrap().absorb(p, r, watching);
            }
        });
        // Гигантские файлы — строго последовательно: пик памяти ограничен
        // одним большим временным индексом.
        for p in &giants {
            if let Some(r) = streaming::pass1_file(p, &query_tokens, &config, &cleaner, &ac) {
                sink_mu.lock().unwrap().absorb(p, r, watching);
            }
        }
        let sink = sink_mu.into_inner().unwrap();
        stats.total_tokens = sink.n_total;

        // ---------- Проход 2: только hit-файлы ----------
        stats.files_with_hits = sink
            .hit_files
            .len()
            + kept.values().filter(|e| !e.hits.is_empty()).count();
        let n_total = sink.n_total as usize;
        let global = &sink.global;
        let mut fresh_entries = sink.entries;
        let hit_paths: Vec<PathBuf> = sink.hit_files.keys().cloned().collect();
        let (hg, hn) = streaming::split_giants(&hit_paths);

        let pass2 = |p: &Path| -> Option<Pass2Result> {
            let hits = sink.hit_files.get(p)?;
            streaming::pass2_file(p, &query_tokens, &config, &cleaner, global, n_total, hits)
        };

        let results_mu: Mutex<Vec<(PathBuf, Pass2Result)>> = Mutex::new(Vec::new());
        hn.par_iter().for_each(|p| {
            if let Some((records, scenes)) = pass2(p) {
                results_mu.lock().unwrap().push((p.clone(), (records, scenes)));
            }
        });
        // Гиганты: параллельная чанковая обработка (границы сцен,
        // глобальные байтовые позиции, память ограничена чанками).
        for p in &hg {
            let Some(hits) = sink.hit_files.get(p) else {
                continue;
            };
            if let Some((records, scenes)) = streaming::pass2_giant_parallel(
                p,
                &query_tokens,
                &config,
                &cleaner,
                global,
                n_total,
                hits,
            ) {
                results_mu.lock().unwrap().push((p.clone(), (records, scenes)));
            }
        }
        let results = results_mu.into_inner().unwrap();
        for (p, (records, scenes)) in results {
            if let Some(e) = fresh_entries.get_mut(&p) {
                e.records = records;
                e.scenes = scenes;
            }
        }

        // ---------- граф сущностей (бюджет рёбер) ----------
        let mut graph = EntityGraph::new();
        let mut seen: HashSet<(String, String, String)> = HashSet::new();
        let mut added_triples = 0usize;
        'graph: for e in kept.values().chain(fresh_entries.values()) {
            for info in e.scenes.values() {
                for t in &info.triples {
                    if added_triples >= config.max_graph_triples {
                        break 'graph;
                    }
                    let key = (t.subject.clone(), t.predicate.clone(), t.object.clone());
                    if seen.insert(key) {
                        graph.add_triple(
                            &t.subject,
                            &t.predicate,
                            &t.object,
                            info.metric_tag.as_deref(),
                            1.0,
                        );
                        added_triples += 1;
                    }
                }
            }
        }
        stats.graph_nodes = graph.node_count();
        stats.graph_edges = graph.edge_count();

        if let Some(sql_path) = &config.graph_export {
            let mut sql = String::new();
            graph.export_sql(&mut sql);
            let _ = fs::write(sql_path, sql);
        }

        // ---------- temporal-фильтр, сортировка, top-N ----------
        let mut records: Vec<HitRecord> = kept
            .values()
            .chain(fresh_entries.values())
            .flat_map(|e| e.records.iter().cloned())
            .collect();

        if let Some(filter) = &config.temporal_filter {
            records.retain(|r| r.metric_tag.as_deref().map_or(true, |m| m == filter));
        }
        stats.total_hits = records.len();

        // ---------- diff-режим: только новые якоря ----------
        if incremental && self.diff_mode {
            let prev = &self.last_hit_keys;
            records.retain(|r| !prev.contains(&(r.path.clone(), r.byte_pos)));
        }

        records.sort_by(|a, b| {
            b.resonance
                .partial_cmp(&a.resonance)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.path.cmp(&b.path))
                .then_with(|| a.byte_pos.cmp(&b.byte_pos))
        });
        let total_hits = records.len();
        records.truncate(config.top_n);

        // ---------- K-hop: одна корневая сущность на все якоря ----------
        let root = query_tokens[0].clone();
        let k_hop = graph.extract_k_hop(
            &root,
            config.k_hop_depth,
            config.temporal_filter.as_deref(),
            config.max_relations,
        );

        // ---------- Проход 3: материализация только top-N ----------
        let anchors: Vec<ContextAnchor> = records
            .iter()
            .map(|r| {
                let scene = materialize_scene(r, &config, &cleaner);
                ContextAnchor {
                    file: r.path.to_string_lossy().to_string(),
                    token: query.to_string(),
                    epsilon: round2(r.epsilon),
                    resonance: round2(r.resonance),
                    scene,
                    k_hop_relations: k_hop.clone(),
                }
            })
            .collect();

        stats.elapsed_ms = started.elapsed().as_millis();

        self.state = if watching {
            let mut m = kept;
            m.extend(fresh_entries);
            Some(m)
        } else {
            None
        };

        // Полный набор ключей хитов текущего прогона (для diff): в
        // watcher-режиме собирается из состояния всех файлов.
        if watching {
            self.last_hit_keys = self
                .state
                .as_ref()
                .map(|m| {
                    m.values()
                        .flat_map(|e| {
                            e.records
                                .iter()
                                .map(|r| (r.path.clone(), r.byte_pos))
                                .collect::<Vec<_>>()
                        })
                        .collect()
                })
                .unwrap_or_default();
        }

        (
            event,
            SearchResult {
                query: query.to_string(),
                total_hits,
                anchors,
            },
            stats,
        )
    }
}

/// Полная материализация сцены одного якоря (проход 3): перечитывает
/// файл через mmap и строит SceneContext только для top-N записей.
///
/// PII-маскирование (v0.4) применяется ТОЛЬКО здесь — на тексте,
/// получаемом AI-потребителем: enclosing_scope и метаданных сцены.
/// Байтовые позиции внутри конвейера остаются raw-точными; маскирование
/// не смещает индексы, потому что выполняется над готовой строкой.
fn materialize_scene(r: &HitRecord, config: &EngineConfig, cleaner: &PiiCleaner) -> SceneContext {
    let res = with_text(&r.path, config.max_file_bytes, |raw| {
        let text: &str = raw;
        let mut sc = if r.is_code {
            let scope =
                crate::parser::ast_code::materialize_scope(text, r.scene_key.0, r.scene_key.1);
            SceneContext::from_code_scope(&scope, &r.path, config.max_scope_bytes)
        } else {
            let bounds = SceneContext::locate(text, r.byte_pos, &r.path);
            SceneContext::build(text, &bounds, &r.path)
        };
        if config.pii_mode == PiiMode::Mask {
            sc.enclosing_scope = cleaner.clean(&sc.enclosing_scope).into_owned();
            for s in sc.subjects.iter_mut() {
                *s = cleaner.clean(s).into_owned();
            }
        }
        sc.enclosing_scope = truncate_char_safe(&sc.enclosing_scope, config.max_scope_bytes);
        sc
    });
    res.unwrap_or_else(|| SceneContext {
        chapter: r.path.to_string_lossy().to_string(),
        temporal_metric: None,
        location: None,
        subjects: Vec::new(),
        enclosing_scope: String::new(),
        metric_tag: None,
        subject_names: Vec::new(),
        subject_pairs: Vec::new(),
    })
}
