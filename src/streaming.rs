//! Потоковый конвейер с ограниченным потреблением памяти (v0.3).
//!
//! Исправление bug#1 (неконтролируемый рост RSS до 2.8 ГБ на корпусах
//! с десятками миллионов токенов): **никогда** не удерживать инвертированные
//! индексы всех файлов репозитория одновременно. Вместо этого:
//!
//! * **Литеральный предфильтр** (техника GNU grep `kwset` / ripgrep
//!   prefilter): SIMD-решёто Teddy (ASCII case-insensitive; для
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

use crate::retrieval::teddy::Teddy;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::parser::markdown_scenes::{light_meta, LightSceneMeta, SceneBounds};
use crate::parser::{detect_lang, extract_code_triples, extract_triples, CodeLang, SceneContext, Triple};
use crate::resonance::{apply_iir_resonance, calculate_epsilon, semantic_bonus};
use crate::{with_text, EngineConfig, PiiCleaner, ResonanceMode};

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
// Литеральный предфильтр (Teddy SIMD — v2.0 задача 2.5)
// ---------------------------------------------------------------------------

/// Литеральный предфильтр запроса: SIMD-решёто Teddy (v2.0 задача 2.5,
/// замена автомата Ахо-Корасик на горячем пути прохода 1).
///
/// * все токены ASCII — регистронезависимое решёто по сырому тексту
///   (семантика прежнего `ascii_case_insensitive` AC, ноль аллокаций);
/// * не-ASCII токены — точное решёто по быстрому фолду ASCII+кириллицы
///   (байт-в-байт ≈ to_lowercase; прочие письменности — полный фолд).
///
/// `None` (пустые/мусорные токены, сверх лимита паттернов) — вызывающий
/// код падает на прежний lowercase-contains фолбэк.
pub fn literal_prefilter(query_tokens: &[String]) -> Option<Teddy> {
    if query_tokens.is_empty() || query_tokens.iter().any(|t| t.is_empty()) {
        return None;
    }
    let pats: Vec<&[u8]> = query_tokens.iter().map(|s| s.as_bytes()).collect();
    if query_tokens.iter().all(|t| t.is_ascii()) {
        return Teddy::build_ascii_ci(&pats).ok();
    }
    Teddy::build(&pats).ok()
}

/// Есть ли в сыром тексте необходимое условие совпадения (литерал
/// какого-либо токена запроса)? Вызывается ДО PII-маскирования и
/// токенизации — на каждом файле корпуса в проходе 1.
pub fn literal_present(raw: &str, query_tokens: &[String], pre: &Option<Teddy>) -> bool {
    if query_tokens.is_empty() {
        return false;
    }
    match pre {
        // ASCII-запрос: SIMD-решёто по сырому тексту, ноль аллокаций.
        Some(t) if t.is_ascii_ci() => t.is_present(raw.as_bytes()),
        // Кириллица и прочий не-ASCII: регистр меняет UTF-8 байты —
        // быстрый фолд ASCII+кириллицы (≈to_lowercase, ~1 ГБ/с) и один
        // проход решётом; прочие письменности — прежний полный фолд.
        Some(t) => match crate::retrieval::teddy::fold_ascii_cyrillic(raw.as_bytes()) {
            Some(folded) => t.is_present(&folded),
            None => t.is_present(raw.to_lowercase().as_bytes()),
        },
        // Фолбэк (пустые/избыточные токены): прежнее поведение.
        None => {
            let lower = raw.to_lowercase();
            query_tokens.iter().any(|q| lower.contains(q.as_str()))
        }
    }
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
    _cleaner: &PiiCleaner,
    pre: &Option<Teddy>,
) -> Option<Pass1File> {
    with_text(path, config.max_file_bytes, |raw| {
        // Предфильтр по сырому тексту: файлы без литерала не токенизируются,
        // но всё равно участвуют в глобальной статистике (streaming counts).
        //
        // PII-маскирование НЕ применяется на уровне токенизации (v0.4):
        // регексы по всему корпусу стоили ~65% времени. Маскирование
        // выполняется только на материализации выходных сцен (engine.rs),
        // где текст получает AI-потребитель.
        if !literal_present(raw, query_tokens, pre) {
            let (counts, total) = streaming_counts(raw);
            return Pass1File {
                counts,
                total,
                hits: Vec::new(),
            };
        }
        let ft = FileTokens::build(raw);
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
/// Третий элемент — все байтовые позиции хитов файла (честный total_hits, diff).
pub type Pass2Result = (
    Vec<HitRecord>,
    HashMap<(usize, usize), SceneInfo>,
    Vec<usize>,
);

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

/// Целевой размер чанка гиганта (файл режется на ~2 МБ кусков).
pub const CHUNK_TARGET_BYTES: usize = 2 * 1024 * 1024;

/// Делит текст гиганта на чанки по границам ПАРАГРАФОВ (\n\n) и
/// markdown-заголовков, выравнивая байтовые позиции к границам UTF-8 и
/// строк. Граница сцены — единственный безопасный разрез: enclosing
/// scope не рвётся посреди логического блока.
///
/// Каждый чанк — (start, end) в байтах оригинального текста.
pub fn chunk_boundaries(text: &str) -> Vec<(usize, usize)> {
    let len = text.len();
    if len <= CHUNK_TARGET_BYTES {
        return vec![(0, len)];
    }
    let mut cuts = Vec::new();
    let mut search_from = 0usize;
    while search_from < len {
        let target = (search_from + CHUNK_TARGET_BYTES).min(len);
        if target >= len {
            cuts.push((search_from, len));
            break;
        }
        // ищем ближайшую границу абзаца (пустая строка) или заголовка
        // в окне [target - 512K, target + 512K]; выравниваем границы
        // окна по границам символов UTF-8
        let mut win_lo = search_from + CHUNK_TARGET_BYTES / 2;
        while win_lo < len && !text.is_char_boundary(win_lo) {
            win_lo += 1;
        }
        let mut win_hi = (target + CHUNK_TARGET_BYTES / 2).min(len);
        while win_hi > win_lo && !text.is_char_boundary(win_hi) {
            win_hi += 1;
        }
        let win_hi = win_hi.min(len);
        let zone = &text[win_lo.min(len)..win_hi.min(len)];
        let mut best: Option<usize> = None;
        // предпочитаем границу заголовка, затем границу абзаца
        if let Some(rel) = zone.find("\n\n#") {
            best = Some(win_lo + rel + 1);
        } else if let Some(rel) = zone.find("\n\n") {
            best = Some(win_lo + rel + 2);
        }
        let cut = best.unwrap_or(target);
        // выравниваем на границу символа и строки
        let mut cut = cut.min(len).max(search_from + 1);
        while cut < len && !text.is_char_boundary(cut) {
            cut += 1;
        }
        if cut < len && text.as_bytes()[cut] != b'\n' && cut + 1 < len {
            // дотягиваем до конца строки
            if let Some(nl) = text[cut..].find('\n') {
                cut += nl;
            }
        }
        if cut <= search_from {
            cut = target; // защита от зацикливания
            while cut < len && !text.is_char_boundary(cut) {
                cut += 1;
            }
        }
        cuts.push((search_from, cut));
        search_from = cut;
    }
    cuts
}

/// Параллельная обработка гигантского файла: чанки по границам сцен
/// обрабатываются независимо в rayon-пуле; результат — слитые записи
/// (байтовые позиции глобальные: chunk.offset прибавляется к локальным).
///
/// Память: каждый поток держит только свой чанк (zero-copy срез mmap),
/// пиковое потребление ограничено ~CHUNK_TARGET_BYTES × threads.
#[allow(clippy::too_many_arguments)]
pub fn pass2_giant_parallel(
    path: &Path,
    query_tokens: &[String],
    config: &EngineConfig,
    _cleaner: &PiiCleaner,
    global_counts: &HashMap<String, usize>,
    n_total: usize,
    hits: &[u32],
) -> Option<Pass2Result> {
    use rayon::prelude::*;

    with_text(path, config.max_file_bytes, |raw| {
        let text: &str = raw;

        // Разбиение на чанки и распределение хитов по чанкам.
        let chunks = chunk_boundaries(text);
        if chunks.len() <= 1 {
            // один чанк — обычный последовательный путь
            return pass2_file(path, query_tokens, config, _cleaner, global_counts, n_total, hits);
        }

        // Распределение хитов по чанкам: байтовые позиции растут вместе
        // с позициями токенов, поэтому достаточно одного прохода
        // (FileTokens уже построен внутри per_chunk-замыкания? нет —
        // строим временный для маппинга токен→байт).
        let per_chunk: Vec<Vec<u32>> = {
            let mut v: Vec<Vec<u32>> = vec![Vec::new(); chunks.len()];
            let mut chunk_idx = 0usize;
            let ft_hint = FileTokens::build(text);
            for &h in hits {
                let byte = ft_hint.window_byte_range(h as usize, h as usize + 1).0;
                while chunk_idx + 1 < chunks.len() && chunks[chunk_idx].1 <= byte {
                    chunk_idx += 1;
                }
                v[chunk_idx].push(h);
            }
            v
        };

        let lang = detect_lang(path);
        let code_file = is_code_file(path);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        // Каждый чанк обрабатывается как независимый «файл»: локальные
        // границы сцен смещаются на chunk.start → глобальные ключи.
        let results: Vec<Option<Pass2Result>> = chunks
            .par_iter()
            .zip(per_chunk.into_par_iter())
            .map(|((start, end), chunk_hits)| {
                if chunk_hits.is_empty() {
                    return None;
                }
                let sub_text = &text[*start..*end];
                let sub_ft = FileTokens::build(sub_text);
                let sub_hits = sub_ft.find_phrase(query_tokens);
                if sub_hits.is_empty() {
                    return None;
                }
                let local_counts: HashMap<String, usize> = sub_ft
                    .counts
                    .iter()
                    .map(|(k, v)| (k.to_string(), *v as usize))
                    .collect();

                let sub_eps_res: Vec<(f64, f64)> = match config.resonance_mode {
                    ResonanceMode::Psi => {
                        let mut epsilons: Vec<f64> = Vec::with_capacity(sub_hits.len());
                        for &h in &sub_hits {
                            let c = h as usize;
                            let s0 = c.saturating_sub(config.window_radius);
                            let e0 = (c + config.window_radius + 1).min(sub_ft.toks_len());
                            let (b, e) = sub_ft.window_byte_range(s0, e0.max(s0 + 1));
                            let wtext = &sub_text[b.min(sub_text.len())..e.min(sub_text.len())];
                            let bonus = semantic_bonus(wtext);
                            let window: Vec<&str> = (s0..e0)
                                .map(|i| sub_ft.tok(i))
                                .collect();
                            epsilons.push(calculate_epsilon(
                                &window,
                                query_tokens,
                                global_counts,
                                n_total,
                                config.kappa,
                                bonus,
                            ));
                        }
                        let forbidden = vec![false; sub_hits.len()];
                        let psi =
                            crate::psi::psi_resonances(&epsilons, &forbidden, config.psi_params);
                        epsilons.into_iter().zip(psi).collect()
                    }
                    // Канонический POLER-цикл (P3_Engine)
                    ResonanceMode::Poler => {
                        let mut epsilons: Vec<f64> = Vec::with_capacity(sub_hits.len());
                        for &h in &sub_hits {
                            let c = h as usize;
                            let s0 = c.saturating_sub(config.window_radius);
                            let e0 = (c + config.window_radius + 1).min(sub_ft.toks_len());
                            let (b, e) = sub_ft.window_byte_range(s0, e0.max(s0 + 1));
                            let wtext = &sub_text[b.min(sub_text.len())..e.min(sub_text.len())];
                            let bonus = semantic_bonus(wtext);
                            let window: Vec<&str> = (s0..e0)
                                .map(|i| sub_ft.tok(i))
                                .collect();
                            epsilons.push(calculate_epsilon(
                                &window,
                                query_tokens,
                                global_counts,
                                n_total,
                                config.kappa,
                                bonus,
                            ));
                        }
                        let forbidden = vec![false; sub_hits.len()];
                        let poler = crate::poler::poler_resonances(
                            &epsilons,
                            &forbidden,
                            config.poler_params,
                        );
                        epsilons.into_iter().zip(poler).collect()
                    }
                    ResonanceMode::Hits => {
                        let mut epsilons: Vec<f64> = Vec::with_capacity(sub_hits.len());
                        for &h in &sub_hits {
                            let c = h as usize;
                            let s0 = c.saturating_sub(config.window_radius);
                            let e0 = (c + config.window_radius + 1).min(sub_ft.toks_len());
                            let (b, e) = sub_ft.window_byte_range(s0, e0.max(s0 + 1));
                            let wtext = &sub_text[b.min(sub_text.len())..e.min(sub_text.len())];
                            let bonus = semantic_bonus(wtext);
                            let window: Vec<&str> = (s0..e0)
                                .map(|i| sub_ft.tok(i))
                                .collect();
                            epsilons.push(calculate_epsilon(
                                &window,
                                query_tokens,
                                global_counts,
                                n_total,
                                config.kappa,
                                bonus,
                            ));
                        }
                        let res = apply_iir_resonance(&epsilons, config.phi_decay);
                        epsilons.into_iter().zip(res).collect()
                    }
                    ResonanceMode::Field => {
                        let hit_set: std::collections::HashSet<usize> =
                            sub_hits.iter().map(|&h| h as usize).collect();
                        let mut out = vec![(0.0, 0.0); sub_hits.len()];
                        let log_n = (n_total.max(1) as f64).ln();
                        let qset: HashSet<&str> =
                            query_tokens.iter().map(|s| s.as_str()).collect();
                        let rarity2 = |tok: &str| -> f64 {
                            let freq = *global_counts.get(tok).unwrap_or(&1) as f64;
                            let rr = (log_n - freq.ln()).max(0.0);
                            rr * rr
                        };
                        let mut win: HashMap<&str, u32> = HashMap::new();
                        let mut unique_sum = 0.0f64;
                        let mut kw = 0u32;
                        let mut head = 0usize;
                        let mut tail = 0usize;
                        let mut r = 0.0f64;
                        let radius = config.window_radius;
                        let n_toks = sub_ft.toks_len();
                        for center in 0..n_toks {
                            let s0 = center.saturating_sub(radius);
                            let e0 = (center + radius + 1).min(n_toks);
                            while head < e0 {
                                let t = sub_ft.tok(head);
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
                            while tail < s0 {
                                let t = sub_ft.tok(tail);
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
                            let eps =
                                config.kappa * (1.0 + ((kw as f64) + 1.0).ln()) * unique_sum;
                            r = eps + config.phi_decay * r;
                            if hit_set.contains(&center) {
                                if let Some(pos) =
                                    sub_hits.iter().position(|&h| h as usize == center)
                                {
                                    out[pos] = (eps, r);
                                }
                            }
                        }
                        out
                    }
                };
                let _ = local_counts;

                // ── Стриминговый Top-K в чанке (bug PYCCLE) ──
                use std::cmp::Reverse;
                let chunk_locator =
                    crate::parser::markdown_scenes::SceneLocator::new(sub_text, path);
                let bit_key = |r: f64| -> u64 {
                    let b = r.to_bits();
                    if b >> 63 == 0 {
                        b ^ 0x8000_0000_0000_0000
                    } else {
                        !b
                    }
                };
                let mut heap: std::collections::BinaryHeap<Reverse<(u64, usize)>> =
                    std::collections::BinaryHeap::new();
                // (hit_idx, byte_pos, key, is_code) — лёгкие данные чанка
                let mut light: Vec<(usize, usize, (usize, usize), bool)> =
                    Vec::with_capacity(sub_hits.len().min(1 << 20));

                for (i, &h) in sub_hits.iter().enumerate() {
                    let c = h as usize;
                    let Some(&local_byte_u32) = sub_ft.pos.get(c) else {
                        continue;
                    };
                    let local_byte = local_byte_u32 as usize;
                    let byte_pos = (start + local_byte).min(text.len());

                    let (key, is_code) = if code_file {
                        match crate::parser::ast_code::locate_scope(sub_text, local_byte, lang) {
                            Some((b, e)) => ((start + b, start + e), true),
                            None => ((*start, *end), true),
                        }
                    } else {
                        // локатор ЧАНКА: заголовки один раз на чанк
                        let b = chunk_locator.locate(sub_text, local_byte);
                        ((*start + b.start, *start + b.end), false)
                    };

                    light.push((i, byte_pos, key, is_code));
                    heap.push(Reverse((bit_key(sub_eps_res[i].1), light.len() - 1)));
                    if heap.len() > config.top_n {
                        heap.pop();
                    }
                }
                let chunk_hit_keys: Vec<usize> = light.iter().map(|(_, b, _, _)| *b).collect();
                let survivors: Vec<usize> = {
                    let mut v: Vec<usize> =
                        heap.into_iter().map(|Reverse((_, li))| li).collect();
                    v.sort_unstable();
                    v
                };

                // Тяжёлые сцены — только для выживших чанка
                let mut scenes: HashMap<(usize, usize), SceneInfo> = HashMap::new();
                let mut records: Vec<HitRecord> = Vec::with_capacity(survivors.len());
                for li in survivors {
                    let (i, byte_pos, key, is_code) = light[li];
                    let info = scenes.entry(key).or_insert_with(|| {
                        if is_code {
                            let scope_text = &text[key.0..key.1.min(text.len())];
                            let name = crate::parser::ast_code::light_signature(text, key.0);
                            SceneInfo {
                                triples: extract_code_triples(scope_text, name.as_deref(), &stem),
                                metric_tag: None,
                            }
                        } else {
                            let b = crate::parser::markdown_scenes::SceneBounds {
                                chapter: stem.clone(),
                                start: key.0,
                                end: key.1,
                                structured: false,
                            };
                            let meta = crate::parser::markdown_scenes::light_meta(text, &b);
                            let scope_text = truncate_slice(
                                &text[key.0..key.1.min(text.len())],
                                config.max_scope_bytes,
                            );
                            let scene = SceneContext::from_meta(&meta);
                            SceneInfo {
                                triples: extract_triples(scope_text, &scene, query_tokens),
                                metric_tag: meta.metric_tag,
                            }
                        }
                    });
                    records.push(HitRecord {
                        path: path.to_path_buf(),
                        byte_pos,
                        epsilon: sub_eps_res[i].0,
                        resonance: sub_eps_res[i].1,
                        scene_key: key,
                        is_code,
                        metric_tag: info.metric_tag.clone(),
                    });
                }
                Some((records, scenes, chunk_hit_keys))
            })
            .collect();

        // Слияние чанков: объединение per-chunk top-N ⊇ глобального top-N
        // (IIR в чанках независим) -> финальная обрезка по резонансу.
        let mut all_light: Vec<HitRecord> = Vec::new();
        let mut all_scenes: HashMap<(usize, usize), SceneInfo> = HashMap::new();
        let mut all_hit_keys: Vec<usize> = Vec::new();
        for (recs, scns, keys) in results.into_iter().flatten() {
            all_hit_keys.extend(keys);
            all_light.extend(recs);
            for (k, v) in scns {
                all_scenes.entry(k).or_insert(v);
            }
        }
        all_light.sort_by(|a, b| {
            b.resonance
                .partial_cmp(&a.resonance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_light.truncate(config.top_n);
        // осиротевшие сцены после финальной обрезки
        let alive: std::collections::HashSet<(usize, usize)> =
            all_light.iter().map(|r| r.scene_key).collect();
        all_scenes.retain(|k, _| alive.contains(k));
        Some((all_light, all_scenes, all_hit_keys))
    })?
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
    _cleaner: &PiiCleaner,
    global_counts: &HashMap<String, usize>,
    n_total: usize,
    hits: &[u32],
) -> Option<Pass2Result> {
    with_text(path, config.max_file_bytes, |raw| {
        let text: &str = raw;
        let ft = FileTokens::build(text);
        let lang = detect_lang(path);
        let code_file = is_code_file(path);
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        // Кэш заголовков: один проход по файлу вместо O(файл) на каждый хит
        let locator = crate::parser::markdown_scenes::SceneLocator::new(text, path);

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
            // POLER[Ψ]: наблюдения = ε окон, эволюция внимания по
            // каноническому уравнению p_{t+1} = p_t + ηΠ(−∇F + γ∇ε).
            // ψ-резонанс нормируется к масштабу ε (|p| ≤ 1 после tanh) —
            // умножаем на интенсивность последнего окна.
            ResonanceMode::Psi => {
                let mut epsilons: Vec<f64> = Vec::with_capacity(hits.len());
                for &h in hits {
                    let c = h as usize;
                    let start = c.saturating_sub(config.window_radius);
                    let end = (c + config.window_radius + 1).min(ft.toks_len());
                    let (b, e) = ft.window_byte_range(start, end.max(start + 1));
                    let wtext = &text[b.min(text.len())..e.min(text.len())];
                    let bonus = semantic_bonus(wtext);
                    let window: Vec<&str> = (start..end)
                        .map(|i| ft.tok(i))
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
                let forbidden = vec![false; hits.len()];
                let psi = crate::psi::psi_resonances(&epsilons, &forbidden, config.psi_params);
                epsilons.into_iter().zip(psi).collect()
            }
            // Канонический POLER-цикл (P3_Engine): p −= ηΠ_Λ(D·p + γJ·p + ∇F)
            ResonanceMode::Poler => {
                let mut epsilons: Vec<f64> = Vec::with_capacity(hits.len());
                for &h in hits {
                    let c = h as usize;
                    let start = c.saturating_sub(config.window_radius);
                    let end = (c + config.window_radius + 1).min(ft.toks_len());
                    let (b, e) = ft.window_byte_range(start, end.max(start + 1));
                    let wtext = &text[b.min(text.len())..e.min(text.len())];
                    let bonus = semantic_bonus(wtext);
                    let window: Vec<&str> = (start..end)
                        .map(|i| ft.tok(i))
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
                let forbidden = vec![false; hits.len()];
                let poler =
                    crate::poler::poler_resonances(&epsilons, &forbidden, config.poler_params);
                epsilons.into_iter().zip(poler).collect()
            }
            ResonanceMode::Hits => {
                let mut epsilons: Vec<f64> = Vec::with_capacity(hits.len());
                for &h in hits {
                    let c = h as usize;
                    let start = c.saturating_sub(config.window_radius);
                    let end = (c + config.window_radius + 1).min(ft.toks_len());
                    let (b, e) = ft.window_byte_range(start, end.max(start + 1));
                    let wtext = &text[b.min(text.len())..e.min(text.len())];
                    let bonus = semantic_bonus(wtext);
                    let window: Vec<&str> = (start..end)
                        .map(|i| ft.tok(i))
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

        // ── Стриминговый Top-K (bug PYCCLE: суперчастотные слова) ──
        // SceneInfo (тройки — дорого) НЕ строится на лету для каждого хита:
        // лёгкие записи собираются в ограниченную кучу top_n по резонансу,
        // тяжёлые сцены строятся только для выживших. Математическая
        // корректность: резонанс хита зависит только от предыдущих хитов
        // (IIR), поэтому отсев по ходу не меняет значений выживших.
        use std::cmp::Reverse;

        // Куча top-N: ключ — монотонное отображение f64 в u64
        // (каноничный total-order transform): не-/отрицательные ветки.
        let bit_key = |r: f64| -> u64 {
            let b = r.to_bits();
            if b >> 63 == 0 {
                b ^ 0x8000_0000_0000_0000
            } else {
                !b
            }
        };
        let mut heap: std::collections::BinaryHeap<Reverse<(u64, usize)>> =
            std::collections::BinaryHeap::new();
        // Лёгкие данные хита: (hit_idx, byte_pos, key, is_code)
        let mut light: Vec<(usize, usize, (usize, usize), bool)> =
            Vec::with_capacity(hits.len().min(1 << 20));
        let mut bounds_cache: HashMap<(usize, usize), SceneBounds> = HashMap::new();

        for (i, &h) in hits.iter().enumerate() {
            let c = h as usize;
            let Some(&byte_pos_u32) = ft.pos.get(c) else {
                continue;
            };
            let byte_pos = (byte_pos_u32 as usize).min(text.len());

            let (key, is_code) = if code_file {
                match crate::parser::ast_code::locate_scope(text, byte_pos, lang) {
                    Some((b, e)) => ((b, e), true),
                    None => ((0, text.len()), true),
                }
            } else {
                let b = locator.locate(text, byte_pos);
                let key = (b.start, b.end);
                bounds_cache.entry(key).or_insert_with(|| b);
                (key, false)
            };

            light.push((i, byte_pos, key, is_code));
            heap.push(Reverse((bit_key(eps_res[i].1), light.len() - 1)));
            if heap.len() > config.top_n {
                heap.pop();
            }
        }

        // Полный набор байтовых позиций (компактно) — до отсева
        let hit_keys: Vec<usize> = light.iter().map(|(_, b, _, _)| *b).collect();

        // Выжившие индексы (top-N по резонансу) — сцены строим только для них
        let survivors: Vec<usize> = {
            let mut v: Vec<usize> = heap.into_iter().map(|Reverse((_, li))| li).collect();
            v.sort_unstable();
            v
        };

        let mut scenes: HashMap<(usize, usize), SceneInfo> = HashMap::new();
        let mut records: Vec<HitRecord> = Vec::with_capacity(survivors.len());
        for li in survivors {
            let (i, byte_pos, key, is_code) = light[li];
            let bounds = if is_code {
                None
            } else {
                Some(SceneBounds {
                    chapter: stem.clone(),
                    start: key.0,
                    end: key.1,
                    structured: false,
                })
            };
            // bounds из кэша точнее (chapter/structured), fallback — синтетика
            let info = scenes.entry(key).or_insert_with(|| {
                let b = bounds_cache.get(&key).cloned().unwrap_or_else(|| {
                    bounds.clone().unwrap_or(SceneBounds {
                        chapter: stem.clone(),
                        start: key.0,
                        end: key.1,
                        structured: false,
                    })
                });
                build_scene_info(text, &b, key, is_code, &stem, query_tokens, config)
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

        Some((records, scenes, hit_keys))
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
        let ft_owned: Vec<&str> = (0..ft.toks_len()).map(|i| ft.tok(i)).collect();
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
            let window: Vec<&str> = (start..end).map(|i| ft.tok(i)).collect();
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
        // ASCII: CI-решёто Teddy по сырому тексту (как прежде AC-CI).
        let pre = literal_prefilter(&q(&["process"]));
        assert!(pre.is_some());
        assert!(pre.as_ref().unwrap().is_ascii_ci());
        assert!(literal_present("call PROCESS now", &q(&["process"]), &pre));
        assert!(!literal_present("nothing here", &q(&["process"]), &pre));

        // Кириллица: теперь тоже Teddy — точное решёто по lowercase-копии
        // (раньше — N × contains на ту же копию).
        let cyr = q(&["нокс"]);
        let pre = literal_prefilter(&cyr);
        assert!(pre.is_some());
        assert!(!pre.as_ref().unwrap().is_ascii_ci());
        assert!(literal_present("Здесь Нокс действует", &cyr, &pre));
        assert!(literal_present("НОКС и нокс", &cyr, &pre));
        assert!(!literal_present("Здесь Соболь", &cyr, &pre));

        // Фолбэк без решёта: прежний lowercase-contains.
        let none: Option<Teddy> = None;
        assert!(literal_present("Здесь Нокс действует", &cyr, &none));
        assert!(!literal_present("Здесь Соболь", &cyr, &none));
        // Много-токенный запрос: достаточно любого литерала.
        let both = q(&["alpha", "omega"]);
        let pre = literal_prefilter(&both);
        assert!(literal_present("только ALPHA тут", &both, &pre));
        assert!(literal_present("только omega тут", &both, &pre));
        assert!(!literal_present("ни одного литерала", &both, &pre));
    }

    #[test]
    fn window_byte_range_correct() {
        let ft = FileTokens::build("alpha beta gamma delta");
        let (b, e) = ft.window_byte_range(1, 3);
        assert_eq!(&"alpha beta gamma delta"[b..e], "beta gamma");
    }

    #[test]
    fn chunk_boundaries_small_file_single_chunk() {
        let text = "короткий текст";
        assert_eq!(chunk_boundaries(text), vec![(0, text.len())]);
    }

    #[test]
    fn chunk_boundaries_respect_paragraph_edges() {
        // большой текст: границы только по \n\n и UTF-8
        let mut text = String::new();
        // ~3 МБ: абзацы по ~400 байт
        for i in 0..12000 {
            text.push_str(&format!(
                "Абзац номер {i} содержит осмысленный текст средней длины \
                 с несколькими словами и точками для объёма. Ещё предложения.\n\n"
            ));
        }
        let chunks = chunk_boundaries(&text);
        assert!(chunks.len() > 1, "ожидается несколько чанков");
        // покрытие полное и без пересечений
        assert_eq!(chunks[0].0, 0);
        assert_eq!(chunks.last().unwrap().1, text.len());
        for w in chunks.windows(2) {
            assert_eq!(w[0].1, w[1].0);
        }
        // каждая граница — на границе символа
        for &(s, e) in &chunks {
            assert!(text.is_char_boundary(s));
            assert!(text.is_char_boundary(e));
            // разрез проходит по границе абзаца или заголовка:
            // перед границей или сразу после неё есть перевод строки
            if e < text.len() {
                let lo = {
                    let mut i = e.saturating_sub(4);
                    while i < e && !text.is_char_boundary(i) {
                        i += 1;
                    }
                    i
                };
                let hi = {
                    let mut i = (e + 4).min(text.len());
                    while i > e && !text.is_char_boundary(i) {
                        i -= 1;
                    }
                    i
                };
                let around = &text[lo..hi];
                assert!(
                    around.contains('\n'),
                    "разрез не на границе строки: байт {e}"
                );
            }
        }
    }

    #[test]
    fn truncate_slice_char_safe() {
        let s = "кириллица".repeat(50);
        let t = truncate_slice(&s, 25);
        assert!(t.len() <= 25);
        assert!(s.starts_with(t));
    }
}
