//! Потоковый конвейер с ограниченным потреблением памяти (v0.3).
//!
//! Исправление bug#1 (неконтролируемый рост RSS до 2.8 ГБ на корпусах
//! с десятками миллионов токенов): **никогда** не удерживать инвертированные
//! индексы всех файлов репозитория одновременно. Вместо этого:
//!
//! * **Литеральный предфильтр** (техника GNU grep `kwset` / ripgrep
//!   prefilter): SIMD-поиск aho-corasick (ASCII case-insensitive; для
//!   кириллицы — lowercase-contains) отбраковывает файлы **до** токенизации;
//! * **Проход 1**: потоковая статистика — временный [`FileTokens`]
//!   (заимствованные из mmap срезы, zero-copy) освобождается сразу после
//!   файла; из него берутся глобальные частоты и позиции совпадений;
//! * **Проход 2**: только hit-файлы токенизируются повторно (временно),
//!   считаются ε/R, локализуются сцены (лёгкие структуры без клонов текста),
//!   извлекаются тройки;
//! * **Проход 3**: полная материализация сцен только для top-N якорей.
//!
//! Файлы крупнее [`GIANT_FILE_BYTES`] обрабатываются строго последовательно:
//! в любой момент времени в памяти не более одного большого временного
//! индекса. Межпроходный кэш текста (`TEXT_CACHE_LIMIT` из v0.2) удалён
//! полностью — каждый проход читает файл заново через mmap.

use aho_corasick::{AhoCorasick, AhoCorasickBuilder};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::parser::markdown_scenes::{light_meta, LightSceneMeta, SceneBounds};
use crate::parser::{detect_lang, extract_code_triples, extract_triples, CodeLang, SceneContext, Triple};
use crate::resonance::{apply_iir_resonance, calculate_epsilon, semantic_bonus};
use crate::{effective_text, with_text, EngineConfig, PiiCleaner, ResonanceMode};

/// Порог «гигантского» файла: обрабатывается строго последовательно.
pub const GIANT_FILE_BYTES: u64 = 8 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Zero-copy токены файла
// ---------------------------------------------------------------------------

/// Набор токенов файла: заимствованные из mmap срезы + ленивая
/// lowercase-нормализация.
///
/// Токены, уже стоящие в нижнем регистре (большинство), хранятся как
/// zero-copy срезы (16 байт); заглавные нормализуются в `Box<str>`
/// только при необходимости. В отличие от [`crate::InvertedIndex`]
/// (публичный API с owned-строками на каждый токен) память на токен
/// в ~3–4 раза меньше.
pub struct FileTokens<'a> {
    /// Исходные срезы токенов.
    raw: Vec<&'a str>,
    /// lowercase-версия, если отличается от исходной.
    lower: Vec<Option<Box<str>>>,
    /// Байтовые позиции начал токенов.
    pos: Vec<u32>,
    /// Частоты токенов файла (lowercase ключи).
    pub counts: HashMap<Box<str>, u32>,
}

impl<'a> FileTokens<'a> {
    /// Один линейный проход O(N). Правила те же, что у `InvertedIndex`.
    pub fn build(text: &'a str) -> Self {
        let cap = text.len() / 6 + 8;
        let mut s = Self {
            raw: Vec::with_capacity(cap),
            lower: Vec::with_capacity(cap),
            pos: Vec::with_capacity(cap),
            counts: HashMap::new(),
        };
        let mut start = 0usize;
        let mut in_word = false;
        for (i, ch) in text.char_indices() {
            if ch.is_alphanumeric() || ch == '_' {
                if !in_word {
                    in_word = true;
                    start = i;
                }
            } else if in_word {
                in_word = false;
                s.push(&text[start..i], start);
            }
        }
        if in_word {
            s.push(&text[start..], start);
        }
        s
    }

    fn push(&mut self, tok: &'a str, at: usize) {
        let lowered = tok.to_lowercase();
        if lowered == tok {
            self.raw.push(tok);
            self.lower.push(None);
        } else {
            self.raw.push(tok);
            self.lower.push(Some(lowered.into_boxed_str()));
        }
        self.pos.push(at as u32);
        let idx = self.raw.len() - 1;
        let key: &str = match &self.lower[idx] {
            Some(s) => s,
            None => self.raw[idx],
        };
        let existing = self.counts.get_mut(key);
        match existing {
            Some(c) => *c += 1,
            None => {
                self.counts.insert(key.to_string().into_boxed_str(), 1);
            }
        }
    }

    /// Токен в нижнем регистре по индексу.
    pub fn tok(&self, i: usize) -> &str {
        match &self.lower[i] {
            Some(s) => s,
            None => self.raw[i],
        }
    }

    /// Число токенов.
    pub fn toks_len(&self) -> usize {
        self.raw.len()
    }

    /// Позиции (индексы токенов) вхождений фразы запроса.
    pub fn find_phrase(&self, query: &[String]) -> Vec<u32> {
        if query.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let n = self.raw.len();
        if query.len() == 1 {
            let q = query[0].as_str();
            for i in 0..n {
                if self.tok(i) == q {
                    out.push(i as u32);
                }
            }
            return out;
        }
        let last_start = n.saturating_sub(query.len());
        'outer: for i in 0..=last_start {
            if self.tok(i) == query[0] {
                for (off, q) in query.iter().enumerate().skip(1) {
                    if self.raw.get(i + off).map_or(true, |_| self.tok(i + off) != q) {
                        continue 'outer;
                    }
                }
                out.push(i as u32);
            }
        }
        out
    }

    /// Байтовый диапазон текста, покрывающий окно токенов `[start, end)`.
    pub fn window_byte_range(&self, start: usize, end: usize) -> (usize, usize) {
        if start >= end || end == 0 || start >= self.raw.len() {
            return (0, 0);
        }
        let end = end.min(self.raw.len());
        let begin = self.pos[start] as usize;
        let finish = self.pos[end - 1] as usize + self.raw[end - 1].len();
        (begin, finish)
    }
}

/// Потоковый подсчёт частот токенов без построения массива токенов
/// (для файлов, отброшенных литеральным предфильтром).
pub fn streaming_counts(text: &str) -> (HashMap<String, usize>, u64) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut total = 0u64;
    let mut in_word = false;
    let mut start = 0usize;
    for (i, ch) in text.char_indices() {
        if ch.is_alphanumeric() || ch == '_' {
            if !in_word {
                in_word = true;
                start = i;
            }
        } else if in_word {
            in_word = false;
            total += 1;
            *counts.entry(text[start..i].to_string()).or_insert(0) += 1;
        }
    }
    if in_word {
        total += 1;
        *counts.entry(text[start..].to_string()).or_insert(0) += 1;
    }
    (counts, total)
}

// ---------------------------------------------------------------------------
// Литеральный предфильтр (GNU grep kwset / ripgrep prefilter)
// ---------------------------------------------------------------------------

/// Автомат Ахо-Корасик по ASCII-токенам запроса (case-insensitive).
/// Для не-ASCII запросов возвращает None (фолбэк на lowercase-contains).
pub fn literal_ac(query_tokens: &[String]) -> Option<AhoCorasick> {
    if query_tokens.is_empty() || !query_tokens.iter().all(|t| t.is_ascii()) {
        return None;
    }
    AhoCorasickBuilder::new()
        .ascii_case_insensitive(true)
        .build(query_tokens)
        .ok()
}

/// Есть ли в сыром тексте необходимое условие совпадения (литерал первого
/// токена запроса)? Вызывается ДО PII-маскирования и токенизации.
pub fn literal_present(raw: &str, query_tokens: &[String], ac: &Option<AhoCorasick>) -> bool {
    if query_tokens.is_empty() {
        return false;
    }
    if let Some(ac) = ac {
        return ac.find_iter(raw).next().is_some();
    }
    // Кириллица и прочий не-ASCII: регистр меняет UTF-8 байты —
    // проверяем вхождение токенов в lowercase-копии текста.
    let lower = raw.to_lowercase();
    query_tokens.iter().any(|q| lower.contains(q.as_str()))
}

// ---------------------------------------------------------------------------
// Проход 1: статистика + обнаружение совпадений
// ---------------------------------------------------------------------------

/// Результат прохода 1 по одному файлу.
pub struct Pass1File {
    pub counts: HashMap<String, usize>,
    pub total: u64,
    pub hits: Vec<u32>,
}

pub fn pass1_file(
    path: &Path,
    query_tokens: &[String],
    config: &EngineConfig,
    cleaner: &PiiCleaner,
    ac: &Option<AhoCorasick>,
) -> Option<Pass1File> {
    with_text(path, config.max_file_bytes, |raw| {
        // Предфильтр по сырому тексту: файлы без литерала не токенизируются,
        // но всё равно участвуют в глобальной статистике (streaming counts).
        if !literal_present(raw, query_tokens, ac) {
            let eff = effective_text(raw, config, cleaner);
            let (counts, total) = streaming_counts(&eff);
            return Pass1File {
                counts,
                total,
                hits: Vec::new(),
            };
        }
        let eff = effective_text(raw, config, cleaner);
        let ft = FileTokens::build(&eff);
        let hits = ft.find_phrase(query_tokens);
        let counts: HashMap<String, usize> = ft
            .counts
            .iter()
            .map(|(k, v)| (k.to_string(), *v as usize))
            .collect();
        Pass1File {
            counts,
            total: ft.toks_len() as u64,
            hits,
        }
    })
}

// ---------------------------------------------------------------------------
// Проход 2: ε / резонанс / локализация сцен / тройки
// ---------------------------------------------------------------------------

/// Лёгкая запись о совпадении: тяжёлая материализация (текст сцены)
/// отложена в проход 3 и выполняется только для top-N.
#[derive(Debug, Clone)]
pub struct HitRecord {
    pub path: PathBuf,
    pub byte_pos: usize,
    pub epsilon: f64,
    pub resonance: f64,
    pub scene_key: (usize, usize),
    pub is_code: bool,
    pub metric_tag: Option<String>,
}

/// Результат прохода 2 по одному файлу.
pub type Pass2Result = (Vec<HitRecord>, HashMap<(usize, usize), SceneInfo>);

/// Информация уникальной сцены: только тройки (без клонов текста сцены).
#[derive(Debug, Clone, Default)]
pub struct SceneInfo {
    pub triples: Vec<Triple>,
    pub metric_tag: Option<String>,
}

pub(crate) fn is_code_file(path: &Path) -> bool {
    matches!(detect_lang(path), CodeLang::Brace | CodeLang::Python)
}

/// Char-safe усечение среза без разрыва UTF-8.
pub(crate) fn truncate_slice(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Разделяет файлы на гигантские (последовательная обработка) и обычные.
pub fn split_giants(files: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    files.iter().cloned().partition(|p| {
        std::fs::metadata(p).map(|m| m.len()).unwrap_or(0) >= GIANT_FILE_BYTES
    })
}

/// Поле резонанса строго за один проход O(N): инкрементальное скользящее
/// окно по `&str`-токенам + IIR; сэмплы в точках совпадений.
#[allow(clippy::too_many_arguments)]
fn field_eps_res(
    ft: &FileTokens,
    hits: &[u32],
    query: &[String],
    gcounts: &HashMap<String, usize>,
    gtotal: usize,
    kappa: f64,
    phi: f64,
    radius: usize,
) -> Vec<(f64, f64)> {
    let mut out = vec![(0.0, 0.0); hits.len()];
    if ft.toks_len() == 0 {
        return out;
    }
    let log_n = (gtotal.max(1) as f64).ln();
    let qset: HashSet<&str> = query.iter().map(|s| s.as_str()).collect();
    let hit_idx: HashMap<u32, usize> = hits.iter().enumerate().map(|(i, &h)| (h, i)).collect();
    let rarity2 = |tok: &str| -> f64 {
        let freq = *gcounts.get(tok).unwrap_or(&1) as f64;
        let rr = (log_n - freq.ln()).max(0.0);
        rr * rr
    };

    let mut win: HashMap<&str, u32> = HashMap::new();
    let mut unique_sum = 0.0f64;
    let mut kw = 0u32;
    let mut head = 0usize;
    let mut tail = 0usize;
    let mut r = 0.0f64;

    for center in 0..ft.toks_len() {
        let start = center.saturating_sub(radius);
        let end = (center + radius + 1).min(ft.toks_len());
        while head < end {
            let t = ft.tok(head);
            if qset.contains(t) {
                kw += 1;
            } else {
                let e = win.entry(t).or_insert(0);
                if *e == 0 {
                    unique_sum += rarity2(t);
                }
                *e += 1;
            }
            head += 1;
        }
        while tail < start {
            let t = ft.tok(tail);
            if qset.contains(t) {
                kw = kw.saturating_sub(1);
            } else if let Some(e) = win.get_mut(t) {
                *e -= 1;
                if *e == 0 {
                    win.remove(t);
                    unique_sum -= rarity2(t);
                }
            }
            tail += 1;
        }
        let eps = kappa * (1.0 + ((kw as f64) + 1.0).ln()) * unique_sum;
        r = eps + phi * r;
        if let Some(&i) = hit_idx.get(&(center as u32)) {
            out[i] = (eps, r);
        }
    }
    out
}

/// Строит SceneInfo (тройки + метрика) для уникальной сцены — без клонов
/// текста сцены: тройки извлекаются из среза mmap.
fn build_scene_info(
    text: &str,
    bounds: &SceneBounds,
    key: (usize, usize),
    is_code: bool,
    stem: &str,
    query_tokens: &[String],
    config: &EngineConfig,
) -> SceneInfo {
    let start = key.0.min(text.len());
    let end = key.1.min(text.len()).max(start);
    if is_code {
        let scope_text = &text[start..end];
        let name = crate::parser::ast_code::light_signature(text, start);
        SceneInfo {
            triples: extract_code_triples(scope_text, name.as_deref(), stem),
            metric_tag: None,
        }
    } else {
        let meta: LightSceneMeta = light_meta(text, bounds);
        let scope_text = truncate_slice(&text[start..end], config.max_scope_bytes);
        let scene = SceneContext::from_meta(&meta);
        SceneInfo {
            triples: extract_triples(scope_text, &scene, query_tokens),
            metric_tag: meta.metric_tag,
        }
    }
}

/// Проход 2 по hit-файлу: возвращает лёгкие записи и тройки сцен.
#[allow(clippy::too_many_arguments)]
pub fn pass2_file(
    path: &Path,
    query_tokens: &[String],
    config: &EngineConfig,
    cleaner: &PiiCleaner,
    global_counts: &HashMap<String, usize>,
    n_total: usize,
    hits: &[u32],
) -> Option<Pass2Result> {
    with_text(path, config.max_file_bytes, |raw| {
        let eff = effective_text(raw, config, cleaner);
        let text: &str = &eff;
        let ft = FileTokens::build(text);
        let lang = detect_lang(path);
        let code_file = is_code_file(path);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        // Статистики для ε: глобальные по корпусу либо локальные по файлу.
        let local_counts: HashMap<String, usize>;
        let (gcounts, gtotal): (&HashMap<String, usize>, usize) = if config.local_stats {
            local_counts = ft
                .counts
                .iter()
                .map(|(k, v)| (k.to_string(), *v as usize))
                .collect();
            (&local_counts, ft.toks_len())
        } else {
            (global_counts, n_total)
        };

        // ε и IIR-резонанс по последовательности совпадений либо полю.
        let eps_res: Vec<(f64, f64)> = match config.resonance_mode {
            ResonanceMode::Hits => {
                let mut epsilons: Vec<f64> = Vec::with_capacity(hits.len());
                for &h in hits {
                    let c = h as usize;
                    let start = c.saturating_sub(config.window_radius);
                    let end = (c + config.window_radius + 1).min(ft.toks_len());
                    let (b, e) = ft.window_byte_range(start, end.max(start + 1));
                    let wtext = &text[b.min(text.len())..e.min(text.len())];
                    let bonus = semantic_bonus(wtext);
                    let window: Vec<String> = (start..end)
                        .map(|i| ft.tok(i).to_string())
                        .collect();
                    epsilons.push(calculate_epsilon(
                        &window,
                        query_tokens,
                        gcounts,
                        gtotal,
                        config.kappa,
                        bonus,
                    ));
                }
                let res = apply_iir_resonance(&epsilons, config.phi_decay);
                epsilons.into_iter().zip(res).collect()
            }
            ResonanceMode::Field => field_eps_res(
                &ft,
                hits,
                query_tokens,
                gcounts,
                gtotal,
                config.kappa,
                config.phi_decay,
                config.window_radius,
            ),
        };

        let mut scenes: HashMap<(usize, usize), SceneInfo> = HashMap::new();
        let mut bounds_cache: HashMap<(usize, usize), SceneBounds> = HashMap::new();
        let mut records: Vec<HitRecord> = Vec::with_capacity(hits.len());

        for (i, &h) in hits.iter().enumerate() {
            let c = h as usize;
            let Some(&byte_pos_u32) = ft.pos.get(c) else {
                continue;
            };
            let byte_pos = (byte_pos_u32 as usize).min(text.len());

            let (key, is_code, bounds) = if code_file {
                match crate::parser::ast_code::locate_scope(text, byte_pos, lang) {
                    Some((b, e)) => ((b, e), true, None),
                    None => ((0, text.len()), true, None),
                }
            } else {
                let b = SceneContext::locate(text, byte_pos, path);
                let key = (b.start, b.end);
                bounds_cache.entry(key).or_insert_with(|| b.clone());
                (key, false, Some(()))
            };
            let _ = bounds;

            let info = scenes
                .entry(key)
                .or_insert_with(|| match bounds_cache.get(&key) {
                    Some(b) => build_scene_info(text, b, key, is_code, &stem, query_tokens, config),
                    None => build_scene_info(
                        text,
                        &SceneBounds {
                            chapter: stem.clone(),
                            start: key.0,
                            end: key.1,
                            structured: false,
                        },
                        key,
                        is_code,
                        &stem,
                        query_tokens,
                        config,
                    ),
                });

            records.push(HitRecord {
                path: path.to_path_buf(),
                byte_pos,
                epsilon: eps_res[i].0,
                resonance: eps_res[i].1,
                scene_key: key,
                is_code,
                metric_tag: info.metric_tag.clone(),
            });
        }

        Some((records, scenes))
    })?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn file_tokens_matches_inverted_index() {
        let text = "Нокс вонзила когти в сплетение. Нокс и снова нокс";
        let ft = FileTokens::build(text);
        let idx = crate::InvertedIndex::build(text);
        let ft_owned: Vec<String> = (0..ft.toks_len()).map(|i| ft.tok(i).to_string()).collect();
        assert_eq!(ft_owned, idx.tokens);
        assert_eq!(ft.pos.len(), idx.positions.len());
        for (a, b) in ft.pos.iter().zip(idx.positions.iter()) {
            assert_eq!(*a as usize, *b);
        }
        assert_eq!(ft.find_phrase(&q(&["нокс"])), vec![0, 5, 8]);
    }

    #[test]
    fn streaming_counts_matches_file_tokens() {
        let text = "alpha beta gamma alpha beta x";
        let (counts, total) = streaming_counts(text);
        let ft = FileTokens::build(text);
        assert_eq!(total, ft.toks_len() as u64);
        assert_eq!(counts.get("alpha"), Some(&2));
        assert_eq!(counts.get("beta"), Some(&2));
        assert_eq!(counts.get("gamma"), Some(&1));
        for (k, v) in &ft.counts {
            assert_eq!(counts.get(k.as_ref()), Some(&(*v as usize)));
        }
    }

    #[test]
    fn cyrillic_uppercase_normalized() {
        let ft = FileTokens::build("Нокс и НОКС и нокс");
        assert_eq!(ft.find_phrase(&q(&["нокс"])), vec![0, 2, 4]);
        assert_eq!(ft.counts.get("нокс").copied(), Some(3));
    }

    #[test]
    fn field_mode_matches_hits_mode_without_bonus() {
        // Инвариант: field-режим (строго O(N)) на позициях совпадений
        // даёт те же ε/R, что и пакетный расчёт окна без семантического бонуса.
        let text = "alpha beta gamma delta alpha epsilon zeta alpha beta theta iota \
                    kappa lambda alpha mu nu xi omicron pi alpha";
        let ft = FileTokens::build(text);
        let query = q(&["alpha"]);
        let gcounts: HashMap<String, usize> = ft
            .counts
            .iter()
            .map(|(k, v)| (k.to_string(), *v as usize))
            .collect();
        let hits = ft.find_phrase(&query);
        let radius = 3;
        let n = ft.toks_len();

        let field = field_eps_res(&ft, &hits, &query, &gcounts, n, 1.0, 0.85, radius);

        let mut epsilons = Vec::new();
        for &h in &hits {
            let c = h as usize;
            let start = c.saturating_sub(radius);
            let end = (c + radius + 1).min(n);
            let window: Vec<String> = (start..end).map(|i| ft.tok(i).to_string()).collect();
            epsilons.push(calculate_epsilon(&window, &query, &gcounts, n, 1.0, 0.0));
        }
        // ε обязаны совпадать с пакетным расчётом окна; R в field-режиме
        // прогоняется по ВСЕМ позициям (а не только хитам), поэтому
        // сравнивается только положительностью и монотонностью аккумулятора.
        for (i, (e, r)) in field.iter().enumerate() {
            assert!((e - epsilons[i]).abs() < 1e-6, "eps mismatch at {i}");
            assert!(*r > 0.0, "resonance must be positive at {i}");
        }
        for w in field.windows(2) {
            assert!(w[1].1 >= w[0].1 * 0.85 - 1e-9);
        }
    }

    #[test]
    fn literal_prefilter_ascii_and_cyrillic() {
        let ac = literal_ac(&q(&["process"]));
        assert!(ac.is_some());
        assert!(literal_present("call PROCESS now", &q(&["process"]), &ac));
        assert!(!literal_present("nothing here", &q(&["process"]), &ac));

        let none: Option<AhoCorasick> = None;
        let cyr = q(&["нокс"]);
        assert!(literal_ac(&cyr).is_none());
        assert!(literal_present("Здесь Нокс действует", &cyr, &none));
        assert!(!literal_present("Здесь Соболь", &cyr, &none));
    }

    #[test]
    fn window_byte_range_correct() {
        let ft = FileTokens::build("alpha beta gamma delta");
        let (b, e) = ft.window_byte_range(1, 3);
        assert_eq!(&"alpha beta gamma delta"[b..e], "beta gamma");
    }

    #[test]
    fn truncate_slice_char_safe() {
        let s = "кириллица".repeat(50);
        let t = truncate_slice(&s, 25);
        assert!(t.len() <= 25);
        assert!(s.starts_with(t));
    }
}
