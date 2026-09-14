//! Benchmark & Regression Suite (v0.21, Задача 4).
//!
//! Автоматический бенчмарк-раннер трёх контуров извлечения + ресурсы:
//!
//! * **Literal Prefilter** (v2.0, задача 2.5) — Teddy SIMD против
//!   Aho-Corasick на литеральном предфильтре streaming-прохода 1
//!   (полный скан без хитов — типичный случай отбраковки файла).
//! * **Exact Retrieval** — POLER Native Grep против внешнего эталона
//!   (ripgrep, при отсутствии — GNU grep; нет ни того ни другого —
//!   прогон без эталона). Метрики: медиана latency (мс) и ПОЛНОТА —
//!   число совпавших строк обязано совпасть с ожидаемым (посаженные
//!   вхождения считаются при генерации корпуса) и с эталоном (parity).
//! * **Explainable Lexical** — индексация корпуса в WebIndex (BM25 +
//!   PageRank + WebRank) и 10 golden-запросов через Semantic Bridge:
//!   latency + проверка top-1 (русский запрос находит EN-only документ
//!   через мост — регрессионная защита Задачи 3).
//! * **Passage Retrieval** — POLER Chunker против naive-сплиттера
//!   (жёсткое окно по символам): latency + «целостность предложений» —
//!   доля чанков, не обрывающих предложение посередине.
//! * **Resources** — VmHWM (пик) и VmRSS (текущая) из /proc/self/status.
//!
//! Регрессионная составляющая — golden-тесты в `#[cfg(test)]` этого
//! модуля (без таймингов — они не флейкуют), запускаются общим
//! `cargo test`. Тайминги — только в CLI-раннере `--benchmark`
//! (медиана ×N прогонов; кэш ФС прогревается первыми прогонами — обе
//! стороны гоняются по одному и тому же корпусу).
//!
//! Детерминизм: корпуса генерируются xorshift-ГПСЧ с фиксированным
//! сидом — два прогона на одной машине сравнимы между собой.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;

use crate::retrieval::chunk::{chunk_document, ChunkConfig, ChunkFormat};
use crate::retrieval::grep::{grep_run, GrepConfig, GrepMode, GrepOutput};
use crate::retrieval::semantic_bridge::SemanticBridge;
use crate::web::index::{content_hash, WebDoc, WebIndex};

/// Паттерн, «посаженный» в корпус точного поиска.
pub const NEEDLE: &str = "POLER_NEEDLE";
/// Сид генерации корпусов (детерминизм между прогонами).
const SEED: u64 = 0x0021_0777_0000_0001;

/// Параметры бенчмарка (уменьшаются в тестах для скорости).
#[derive(Debug, Clone)]
pub struct BenchOpts {
    /// Файлов в корпусе точного поиска.
    pub grep_files: usize,
    /// Строк в файле корпуса.
    pub grep_lines: usize,
    /// Страниц в лексическом корпусе (включая 10 тематических).
    pub lexical_docs: usize,
    /// Прогонов для медианы latency.
    pub runs: usize,
    /// Секций markdown в passage-документе.
    pub passage_sections: usize,
    /// Целевой размер чанка (токены).
    pub passage_target_tokens: usize,
}

impl Default for BenchOpts {
    fn default() -> Self {
        Self {
            grep_files: 300,
            grep_lines: 120,
            lexical_docs: 400,
            runs: 3,
            passage_sections: 40,
            passage_target_tokens: 96,
        }
    }
}

// ---------------------------------------------------------------------------
// Результаты (serde: --benchmark-json)
// ---------------------------------------------------------------------------

/// Прогон внешнего эталона (ripgrep / GNU grep).
#[derive(Debug, Clone, Serialize)]
pub struct ReferenceBench {
    pub name: String,
    pub ms: f64,
    pub matched_lines: usize,
}

/// Контур 0 (v2.0, задача 2.5): Teddy SIMD vs Aho-Corasick —
/// литеральный предфильтр streaming-прохода 1.
#[derive(Debug, Clone, Serialize)]
pub struct PrefilterBench {
    /// Размер сканируемого корпуса (байт).
    pub corpus_bytes: usize,
    /// Число паттернов в прогоне.
    pub patterns: usize,
    /// ASCII, полный скан: Aho-Corasick ascii_case_insensitive (прежде).
    pub ac_fullscan_ms: f64,
    /// ASCII, полный скан: Teddy SIMD (теперь).
    pub teddy_fullscan_ms: f64,
    /// Ускорение Teddy vs AC (×).
    pub speedup_x: f64,
    /// Кириллица, полный скан: прежний путь (lowercase + N × contains).
    pub cyr_old_ms: f64,
    /// Кириллица, полный скан: новый путь (lowercase + Teddy).
    pub cyr_teddy_ms: f64,
    /// Ускорение на кириллице (×).
    pub cyr_speedup_x: f64,
    /// Хит-случай: AC / Teddy / contains согласны (все паттерны найдены).
    pub agree: bool,
}

/// Контур 1: точный поиск.
#[derive(Debug, Clone, Serialize)]
pub struct ExactBench {
    pub files: usize,
    pub lines: usize,
    pub poler_ms: f64,
    pub poler_matched_lines: usize,
    /// Ожидаемое число совпавших строк (посчитано генератором корпуса).
    pub expected_matched_lines: usize,
    /// POLER нашёл ровно столько, сколько посажено.
    pub completeness_ok: bool,
    /// Внешний эталон (если rg/grep доступны).
    pub reference: Option<ReferenceBench>,
    /// Число строк совпало с эталоном (полнота parity).
    pub parity: Option<bool>,
}

/// Контур 2: лексический поиск.
#[derive(Debug, Clone, Serialize)]
pub struct LexicalBench {
    pub pages: usize,
    pub index_ms: f64,
    pub queries: usize,
    pub query_avg_ms: f64,
    /// Golden-запрос (русский) и ожидаемый top-1 (EN-only документ).
    pub golden_query: String,
    pub golden_expected: String,
    pub golden_top1: Option<String>,
    pub golden_ok: bool,
    /// Сколько термов-кандидатов добавил мост в golden-запрос.
    pub bridge_expanded_terms: usize,
}

/// Контур 3: passage-нарезка.
#[derive(Debug, Clone, Serialize)]
pub struct PassageBench {
    pub doc_bytes: usize,
    pub poler_ms: f64,
    pub poler_chunks: usize,
    /// % чанков, заканчивающихся границей предложения.
    pub poler_integrity_pct: f64,
    pub naive_ms: f64,
    pub naive_chunks: usize,
    pub naive_integrity_pct: f64,
}

/// RAM-снимок (Linux, /proc/self/status).
#[derive(Debug, Clone, Serialize)]
pub struct ResourceBench {
    pub vm_hwm_kb: u64,
    pub vm_rss_kb: u64,
}

/// Полный отчёт прогона.
#[derive(Debug, Clone, Serialize)]
pub struct BenchResults {
    pub prefilter: PrefilterBench,
    pub exact: ExactBench,
    pub lexical: LexicalBench,
    pub passage: PassageBench,
    pub resources: Option<ResourceBench>,
}

// ---------------------------------------------------------------------------
// Детерминированный ГПСЧ (xorshift64*) — без зависимости rand
// ---------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
}

fn median(vals: &mut [f64]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let n = vals.len();
    if n % 2 == 1 {
        vals[n / 2]
    } else {
        (vals[n / 2 - 1] + vals[n / 2]) / 2.0
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

// ---------------------------------------------------------------------------
// Корпус 0 (v2.0, задача 2.5): Teddy vs AC — предфильтр
// ---------------------------------------------------------------------------

/// Генерирует смешанный ru/en корпус (~`target_bytes`, случайная
/// капитализация — регистронезависимость честно нагружается).
fn gen_prefilter_corpus(target_bytes: usize, seed: u64) -> String {
    let mut rng = Rng::new(seed);
    let ru = [
        "система", "модуль", "резонанс", "токен", "поток", "поле",
        "вектор", "сцена", "окно", "плотность", "индекс", "строка",
        "файл", "текст", "запрос", "канал",
    ];
    let en = [
        "runtime", "module", "engine", "stream", "buffer", "token",
        "field", "vector", "window", "density", "index", "line",
        "file", "text", "query", "lane",
    ];
    let mut s = String::with_capacity(target_bytes + 128);
    while s.len() < target_bytes {
        let words = 6 + rng.below(6);
        for _ in 0..words {
            let w = if rng.below(10) < 6 {
                ru[rng.below(ru.len() as u64) as usize]
            } else {
                en[rng.below(en.len() as u64) as usize]
            };
            if rng.below(4) == 0 {
                let mut c = w.chars();
                if let Some(first) = c.next() {
                    s.extend(first.to_uppercase());
                    s.push_str(c.as_str());
                }
            } else {
                s.push_str(w);
            }
            s.push(' ');
        }
        s.push('\n');
    }
    s
}

/// Контур 0: полный скан корпуса паттернами, которых в нём НЕТ —
/// типичный случай предфильтра (большинство файлов отбраковывается до
/// токенизации, раннего выхода нет). Хит-случай проверяется на согласие
/// результатов без таймингов (позиция первого вхождения флейкует).
pub fn bench_prefilter(opts: &BenchOpts) -> Result<PrefilterBench, String> {
    use crate::retrieval::teddy::Teddy;
    use aho_corasick::AhoCorasick;

    const CORPUS_BYTES: usize = 1_500_000;
    let text = gen_prefilter_corpus(CORPUS_BYTES, SEED ^ 0x2E55);
    let hay = text.as_bytes();

    // Отсутствующие в корпусе паттерны (в т.ч. с частыми байтами —
    // «abababab» честно грузит решёто Teddy кандидатами).
    let ascii_miss: Vec<String> = [
        "xyzzy", "qqqzzz", "wkwkwk", "abababab", "mmmdire", "fluxion",
        "polyfill", "zenzizenzic",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let cyr_miss: Vec<String> = [
        "кварц", "базальт", "гранит", "слюда", "ягель", "туф",
        "долерит", "пегматит",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    // ASCII: AC (прежний путь) vs Teddy (новый путь).
    let ac = AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build(&ascii_miss)
        .map_err(|e| format!("AC: {e}"))?;
    let miss_refs: Vec<&[u8]> = ascii_miss.iter().map(|s| s.as_bytes()).collect();
    let teddy = Teddy::build_ascii_ci(&miss_refs).map_err(|e| format!("Teddy: {e}"))?;

    let mut ac_times = Vec::with_capacity(opts.runs);
    let mut ted_times = Vec::with_capacity(opts.runs);
    let mut r_ac = false;
    let mut r_ted = false;
    for _ in 0..opts.runs {
        let t = Instant::now();
        r_ac = ac.find_iter(&text).next().is_some();
        ac_times.push(ms(t));
        let t = Instant::now();
        r_ted = teddy.is_present(hay);
        ted_times.push(ms(t));
    }
    if r_ac || r_ted {
        return Err("паттерны miss-сета неожиданно найдены в корпусе".into());
    }

    // Кириллица: прежний путь (to_lowercase + N × contains) против
    // нового (быстрый фолд ASCII+кириллицы + Teddy) — как в
    // streaming::literal_present.
    let cyr_refs: Vec<&[u8]> = cyr_miss.iter().map(|s| s.as_bytes()).collect();
    let cyr_teddy = Teddy::build(&cyr_refs).map_err(|e| format!("Teddy cyr: {e}"))?;
    let mut old_times = Vec::with_capacity(opts.runs);
    let mut new_times = Vec::with_capacity(opts.runs);
    let mut r_old = false;
    let mut r_new = false;
    for _ in 0..opts.runs {
        let t = Instant::now();
        let lower = text.to_lowercase();
        r_old = cyr_miss.iter().any(|q| lower.contains(q.as_str()));
        old_times.push(ms(t));
        drop(lower);
        let t = Instant::now();
        match crate::retrieval::teddy::fold_ascii_cyrillic(hay) {
            Some(folded) => r_new = cyr_teddy.is_present(&folded),
            None => return Err("корпус содержит прочие письменности — фолд недоступен".into()),
        }
        new_times.push(ms(t));
    }
    if r_old || r_new {
        return Err("кириллические miss-паттерны неожиданно найдены".into());
    }

    // Хит-случай: все три исполнителя обязаны согласиться.
    let ascii_hit: Vec<String> = ["Runtime", "buffer", "ENGINE"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let cyr_hit: Vec<String> = ["Модуль", "резонанс"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let ac_hit = AhoCorasick::builder()
        .ascii_case_insensitive(true)
        .build(&ascii_hit)
        .map_err(|e| format!("AC hit: {e}"))?;
    let hit_refs: Vec<&[u8]> = ascii_hit.iter().map(|s| s.as_bytes()).collect();
    let teddy_hit = Teddy::build_ascii_ci(&hit_refs).map_err(|e| format!("Teddy hit: {e}"))?;
    let cyr_hit_refs: Vec<&[u8]> = cyr_hit.iter().map(|s| s.as_bytes()).collect();
    let cyr_teddy_hit = Teddy::build(&cyr_hit_refs).map_err(|e| format!("Teddy cyr hit: {e}"))?;
    let lower = text.to_lowercase();
    let agree = ac_hit.find_iter(&text).next().is_some()
        && teddy_hit.is_present(hay)
        && cyr_hit.iter().any(|q| lower.contains(q.as_str()))
        && cyr_teddy_hit.is_present(lower.as_bytes());

    let ac_ms = median(&mut ac_times);
    let ted_ms = median(&mut ted_times);
    let old_ms = median(&mut old_times);
    let new_ms = median(&mut new_times);
    Ok(PrefilterBench {
        corpus_bytes: text.len(),
        patterns: ascii_miss.len(),
        ac_fullscan_ms: ac_ms,
        teddy_fullscan_ms: ted_ms,
        speedup_x: if ted_ms > 0.0 { ac_ms / ted_ms } else { 0.0 },
        cyr_old_ms: old_ms,
        cyr_teddy_ms: new_ms,
        cyr_speedup_x: if new_ms > 0.0 { old_ms / new_ms } else { 0.0 },
        agree,
    })
}

// ---------------------------------------------------------------------------
// Корпус 1: точный поиск (grep)
// ---------------------------------------------------------------------------

/// Генерирует корпус и возвращает ОЖИДАЕМОЕ число строк с вхождением
/// [`NEEDLE`]: каждый 5-й файл, в нём строки с (i + f) % 37 == 0.
pub fn gen_grep_corpus(dir: &Path, files: usize, lines: usize, seed: u64) -> Result<usize, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("корпус {dir:?}: {e}"))?;
    let mut rng = Rng::new(seed);
    let mut expected = 0usize;
    let exts = ["rs", "py", "md", "txt"];
    for f in 0..files {
        let plant = f % 5 == 0;
        let mut buf = String::with_capacity(lines * 48);
        for i in 0..lines {
            let needle = plant && (i + f) % 37 == 0;
            if needle {
                expected += 1;
            }
            let a = rng.below(10_000);
            let b = rng.below(10_000);
            let c = rng.below(1_000);
            if needle {
                buf.push_str(&format!(
                    "line {i}: {NEEDLE} alpha_{a} beta_{b} gamma_{c} delta epsilon\n"
                ));
            } else {
                buf.push_str(&format!(
                    "line {i}: alpha_{a} beta_{b} gamma_{c} delta epsilon zeta_{b}\n"
                ));
            }
        }
        let path = dir.join(format!("file_{f:04}.{}", exts[f % exts.len()]));
        std::fs::write(&path, buf).map_err(|e| format!("запись {path:?}: {e}"))?;
    }
    Ok(expected)
}

/// Поиск бинаря в PATH (без внешних зависимостей).
fn find_in_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    path.split(':')
        .map(|dir| Path::new(dir).join(bin))
        .find(|p| p.is_file())
}

/// Сумма счётчиков из вывода `rg -c` / `grep -rc` (строки «путь:число»).
fn parse_counts(out: &str) -> usize {
    out.lines()
        .filter_map(|l| l.rsplit(':').next())
        .filter_map(|n| n.trim().parse::<usize>().ok())
        .sum()
}

/// Прогон внешнего эталона: ripgrep приоритетнее (тот же класс обхода).
fn run_reference(pattern: &str, dir: &Path) -> Option<(String, f64, usize)> {
    if let Some(rg) = find_in_path("rg") {
        let t = Instant::now();
        let out = std::process::Command::new(&rg)
            .args(["-c", "--no-messages", pattern])
            .arg(dir)
            .output()
            .ok()?;
        let elapsed = ms(t);
        let count = parse_counts(&String::from_utf8_lossy(&out.stdout));
        return Some(("ripgrep".to_string(), elapsed, count));
    }
    if let Some(grep) = find_in_path("grep") {
        let t = Instant::now();
        let out = std::process::Command::new(&grep)
            .args(["-r", "-s", "-c", pattern])
            .arg(dir)
            .output()
            .ok()?;
        let elapsed = ms(t);
        let count = parse_counts(&String::from_utf8_lossy(&out.stdout));
        return Some(("GNU grep".to_string(), elapsed, count));
    }
    None
}

/// Контур 1: POLER Native Grep vs эталон.
pub fn bench_exact(root: &Path, opts: &BenchOpts) -> Result<ExactBench, String> {
    let dir = root.join("grep-corpus");
    let expected = gen_grep_corpus(&dir, opts.grep_files, opts.grep_lines, SEED)?;
    let config = GrepConfig {
        pattern: NEEDLE.to_string(),
        mode: GrepMode::Literal,
        output: GrepOutput::Count,
        respect_ignore: false,
        ..Default::default()
    };

    let mut times: Vec<f64> = Vec::with_capacity(opts.runs);
    let mut poler_matched = 0usize;
    for r in 0..opts.runs.max(1) {
        let t = Instant::now();
        let report = grep_run(&[dir.clone()], &config).map_err(|e| format!("grep_run: {e}"))?;
        times.push(ms(t));
        if r == 0 {
            poler_matched = report.files.iter().map(|f| f.count).sum();
        }
    }
    let poler_ms = median(&mut times);

    let reference = run_reference(NEEDLE, &dir);
    let parity = reference.as_ref().map(|(_, _, c)| *c == poler_matched);
    let _ = std::fs::remove_dir_all(&dir);

    Ok(ExactBench {
        files: opts.grep_files,
        lines: opts.grep_lines,
        poler_ms,
        poler_matched_lines: poler_matched,
        expected_matched_lines: expected,
        completeness_ok: poler_matched == expected,
        reference: reference.map(|(name, elapsed, matched)| ReferenceBench {
            name,
            ms: elapsed,
            matched_lines: matched,
        }),
        parity,
    })
}

// ---------------------------------------------------------------------------
// Корпус 2: лексический поиск (BM25 + WebRank + Semantic Bridge)
// ---------------------------------------------------------------------------

/// Тематические страницы: словарь двуязычный; mutex НАМЕРЕННО EN-only —
/// русский запрос обязан найти его только через мост (регрессия §8.1).
const TOPIC_DOCS: &[(&str, &str, &str)] = &[
    (
        "https://bench.io/mutex",
        "Mutex lock",
        "A mutex lock guards shared state in concurrent programs. Threads acquire the \
         lock before touching shared data and release it after. Deadlock arises when \
         two locks are taken in opposite order.",
    ),
    (
        "https://bench.io/cache",
        "Кэш и буфер",
        "Кэш и буфер: cache buffer eviction policy LRU. Кэширование ускоряет чтение, \
         буфер сглаживает пики записи.",
    ),
    (
        "https://bench.io/ownership",
        "Владение",
        "Владение и заимствование: ownership borrowing lifetimes. Память освобождается \
         детерминированно, без сборщика мусора.",
    ),
    (
        "https://bench.io/pagerank",
        "PageRank",
        "PageRank: link analysis ranking pages by incoming links. Итерации сходятся \
         к стационарному распределению рангов.",
    ),
    (
        "https://bench.io/grep",
        "grep",
        "grep: regex pattern matching over lines. Поиск всех совпадений с гарантией \
         полноты: ноль значит ноль.",
    ),
    (
        "https://bench.io/search",
        "Индекс",
        "Search index: inverted index postings BM25. Поиск по индексу возвращает \
         релевантные документы сверху вниз.",
    ),
    (
        "https://bench.io/ranking",
        "Ranking",
        "WebRank ranking: BM25, PageRank, title, density. Ранжирование документов \
         детерминировано и объяснимо.",
    ),
    (
        "https://bench.io/db",
        "Транзакции",
        "Transactions and connections: ACID isolation pooling. Транзакции и соединения \
         с базой данных.",
    ),
    (
        "https://bench.io/network",
        "Sockets",
        "Sockets and streams: TCP connection handshake packet loss retransmission \
         timeout.",
    ),
    (
        "https://bench.io/parser",
        "Parser",
        "Parser: tokens grammar AST. Парсер строит дерево разбора из потока токенов.",
    ),
];

/// Golden-запросы: (запрос, ожидаемый top-1). Первый — регрессия моста:
/// русский запрос к EN-only документу.
const GOLDEN_QUERIES: &[(&str, &str)] = &[
    ("блокировка мьютекса", "https://bench.io/mutex"),
    ("кэш и буфер", "https://bench.io/cache"),
    ("владение и заимствование", "https://bench.io/ownership"),
    ("pagerank ссылочный анализ", "https://bench.io/pagerank"),
    ("grep регулярные выражения", "https://bench.io/grep"),
    ("поиск по индексу", "https://bench.io/search"),
    ("ранжирование документов", "https://bench.io/ranking"),
    ("транзакции и соединения", "https://bench.io/db"),
    ("mutex shared state", "https://bench.io/mutex"),
    ("cache eviction policy", "https://bench.io/cache"),
];

fn webdoc(url: &str, title: &str, text: &str) -> WebDoc {
    WebDoc {
        url: url.to_string(),
        title: title.to_string(),
        lang: "mixed".to_string(),
        meta_description: String::new(),
        text: text.to_string(),
        links: vec![],
        content_hash: content_hash(text),
    }
}

/// Лексический корпус: 10 тематических + (n-10) филлер-страниц.
pub fn lexical_corpus(n: usize) -> Vec<WebDoc> {
    let mut docs: Vec<WebDoc> = TOPIC_DOCS.iter().map(|(u, t, x)| webdoc(u, t, x)).collect();
    let filler = n.saturating_sub(TOPIC_DOCS.len());
    let mut rng = Rng::new(SEED ^ 0xF11E);
    for i in 0..filler {
        let a = rng.below(10_000);
        let b = rng.below(10_000);
        let text = format!(
            "filler doc {i}: qux_{a} zeta_{b} lorem ipsum dolor sit amet consectetur \
             adipiscing elit sed tempor incididunt labore dolore magna aliqua"
        );
        docs.push(webdoc(
            &format!("https://bench.io/filler/{i}"),
            &format!("Filler {i}"),
            &text,
        ));
    }
    docs
}

/// Контур 2: индексация + golden-запросы через мост.
pub fn bench_lexical(root: &Path, opts: &BenchOpts) -> Result<LexicalBench, String> {
    let dir = root.join("lexical");
    std::fs::create_dir_all(&dir).map_err(|e| format!("корпус {dir:?}: {e}"))?;
    let mut ix = WebIndex::open(&dir.join("bench-index.db"))
        .map_err(|e| format!("WebIndex::open: {e}"))?;

    let docs = lexical_corpus(opts.lexical_docs);
    let t = Instant::now();
    for doc in &docs {
        ix.upsert_page(doc).map_err(|e| format!("upsert: {e}"))?;
    }
    let index_ms = ms(t);

    let bridge = SemanticBridge::offline();
    let mut q_times: Vec<f64> = Vec::with_capacity(GOLDEN_QUERIES.len());
    let mut golden_top1: Option<String> = None;
    let mut bridge_terms = 0usize;
    for (qi, (q, _)) in GOLDEN_QUERIES.iter().enumerate() {
        let t = Instant::now();
        let (hits, exp) = ix
            .search_with_bridge(q, 5, &bridge)
            .map_err(|e| format!("search «{q}»: {e}"))?;
        q_times.push(ms(t));
        if qi == 0 {
            golden_top1 = hits.first().map(|h| h.url.clone());
            bridge_terms = exp.extra_terms().len();
        }
    }
    let query_avg_ms = q_times.iter().sum::<f64>() / q_times.len().max(1) as f64;
    let golden = GOLDEN_QUERIES[0];
    let golden_ok = golden_top1.as_deref() == Some(golden.1);

    let _ = std::fs::remove_dir_all(&dir);
    Ok(LexicalBench {
        pages: docs.len(),
        index_ms,
        queries: GOLDEN_QUERIES.len(),
        query_avg_ms,
        golden_query: golden.0.to_string(),
        golden_expected: golden.1.to_string(),
        golden_top1,
        golden_ok,
        bridge_expanded_terms: bridge_terms,
    })
}

// ---------------------------------------------------------------------------
// Корпус 3: passage-нарезка (POLER Chunker vs naive splitter)
// ---------------------------------------------------------------------------

/// Генератор markdown-документа: секции → абзацы → предложения.
pub fn gen_markdown(sections: usize, seed: u64) -> String {
    const WORDS: &[&str] = &[
        "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota",
        "kappa", "lambda", "mu", "nu", "xi", "omicron", "pi", "rho", "sigma", "tau",
        "upsilon",
    ];
    let mut rng = Rng::new(seed);
    let mut out = String::new();
    for s in 0..sections {
        out.push_str(&format!("# Section {s}: {}\n\n", WORDS[s % WORDS.len()]));
        for _ in 0..6 {
            let sentences = 5 + rng.below(4) as usize;
            for _ in 0..sentences {
                let len = 8 + rng.below(7) as usize;
                let mut sentence = String::new();
                for _ in 0..len {
                    sentence.push_str(WORDS[rng.below(WORDS.len() as u64) as usize]);
                    sentence.push(' ');
                }
                out.push_str(sentence.trim_end());
                out.push_str(". ");
            }
            out.push_str("\n\n");
        }
    }
    out
}

/// Naive-сплиттер (бейзлайн): жёсткое окно по символам, режет wherever.
pub fn naive_split(text: &str, window_chars: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() || window_chars == 0 {
        return vec![text.to_string()];
    }
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        let end = (i + window_chars).min(chars.len());
        out.push(chars[i..end].iter().collect());
        i += window_chars;
    }
    out
}

/// Целостность предложений: % чанков, заканчивающихся границей
/// предложения/заголовка/списка (не обрывают текст посередине).
pub fn sentence_integrity(chunks: &[&str]) -> f64 {
    if chunks.is_empty() {
        return 0.0;
    }
    let ok = chunks
        .iter()
        .filter(|t| {
            let trimmed = t.trim_end();
            trimmed.ends_with('.')
                || trimmed.ends_with('!')
                || trimmed.ends_with('?')
                || trimmed.ends_with('…')
                || trimmed.ends_with(':')
                || trimmed.ends_with(';')
                || trimmed.ends_with('#')
                || trimmed.ends_with("```")
        })
        .count();
    100.0 * ok as f64 / chunks.len() as f64
}

/// Контур 3: POLER Chunker vs naive splitter.
pub fn bench_passage(opts: &BenchOpts) -> Result<PassageBench, String> {
    let text = gen_markdown(opts.passage_sections, SEED ^ 0xACC1);
    let cfg = ChunkConfig {
        target_tokens: opts.passage_target_tokens,
        ..Default::default()
    };

    let mut times: Vec<f64> = Vec::with_capacity(opts.runs);
    let mut last = None;
    for _ in 0..opts.runs.max(1) {
        let t = Instant::now();
        last = Some(chunk_document(&text, ChunkFormat::Markdown, &cfg));
        times.push(ms(t));
    }
    let report = last.expect("runs >= 1");
    let poler_ms = median(&mut times);

    let poler_texts: Vec<&str> = report.chunks.iter().map(|c| c.text.as_str()).collect();
    let poler_integrity = sentence_integrity(&poler_texts);

    // окно naive = средний размер poler-чанка (честное сравнение)
    let mean_chars = (report
        .chunks
        .iter()
        .map(|c| c.text.chars().count())
        .sum::<usize>()
        / report.chunks.len().max(1))
    .max(64);
    let t = Instant::now();
    let naive_texts = naive_split(&text, mean_chars);
    let naive_ms = ms(t);
    let naive_refs: Vec<&str> = naive_texts.iter().map(|s| s.as_str()).collect();
    let naive_integrity = sentence_integrity(&naive_refs);

    Ok(PassageBench {
        doc_bytes: text.len(),
        poler_ms,
        poler_chunks: report.chunks.len(),
        poler_integrity_pct: poler_integrity,
        naive_ms,
        naive_chunks: naive_texts.len(),
        naive_integrity_pct: naive_integrity,
    })
}

// ---------------------------------------------------------------------------
// Resources + компоновка
// ---------------------------------------------------------------------------

/// VmHWM (пик) и VmRSS (текущая) из /proc/self/status, кБ (Linux).
pub fn read_proc_status() -> Option<ResourceBench> {
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let mut hwm: Option<u64> = None;
        let mut rss: Option<u64> = None;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmHWM:") {
                hwm = rest.trim().split_whitespace().next().and_then(|v| v.parse().ok());
            } else if let Some(rest) = line.strip_prefix("VmRSS:") {
                rss = rest.trim().split_whitespace().next().and_then(|v| v.parse().ok());
            }
        }
        Some(ResourceBench {
            vm_hwm_kb: hwm?,
            vm_rss_kb: rss?,
        })
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Полный прогон всех контуров. Корпуса — во временной директории,
/// удаляются после каждого контура (не оставляем мусора).
pub fn run_suite(opts: &BenchOpts) -> Result<BenchResults, String> {
    let root = std::env::temp_dir().join(format!(
        "poler-bench-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&root).map_err(|e| format!("tmp {root:?}: {e}"))?;
    let result = (|| {
        let prefilter = bench_prefilter(opts)?;
        let exact = bench_exact(&root, opts)?;
        let lexical = bench_lexical(&root, opts)?;
        let passage = bench_passage(opts)?;
        Ok(BenchResults {
            prefilter,
            exact,
            lexical,
            passage,
            resources: read_proc_status(),
        })
    })();
    let _ = std::fs::remove_dir_all(&root);
    result
}

// ---------------------------------------------------------------------------
// Тесты: golden-регрессии (без таймингов — тайминги не флейкуют)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn bench_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "poler-bench-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn prefilter_agreement_and_sanity() {
        // Golden: контур 0 — согласие AC/Teddy/contains и разумные метрики.
        let opts = BenchOpts { runs: 2, ..Default::default() };
        let res = bench_prefilter(&opts).expect("prefilter bench");
        assert!(res.agree, "хит-случай: исполнители разошлись");
        assert!(res.speedup_x > 0.0);
        assert!(res.corpus_bytes > 1_000_000);
    }

    #[test]
    fn exact_retrieval_completeness_planted_count() {
        // ПОЛНОТА: POLER Native Grep обязан найти ровно столько строк,
        // сколько посажено генератором (гарантия «ноль значит ноль»)
        let root = bench_root("exact");
        let opts = BenchOpts {
            grep_files: 60,
            grep_lines: 40,
            runs: 2,
            ..Default::default()
        };
        let res = bench_exact(&root, &opts).expect("exact bench");
        assert!(
            res.completeness_ok,
            "poler={} expected={}",
            res.poler_matched_lines,
            res.expected_matched_lines
        );
        assert!(res.poler_matched_lines > 0, "в корпусе обязаны быть вхождения");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exact_retrieval_parity_with_reference() {
        // если rg/grep доступны — полнота обязана совпасть с эталоном
        let root = bench_root("parity");
        let opts = BenchOpts {
            grep_files: 60,
            grep_lines: 40,
            runs: 2,
            ..Default::default()
        };
        let res = bench_exact(&root, &opts).expect("exact bench");
        if let Some(parity) = res.parity {
            assert!(
                parity,
                "poler={} reference={:?}",
                res.poler_matched_lines,
                res.reference.as_ref().map(|r| r.matched_lines)
            );
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn corpus_generator_deterministic() {
        // один сид → идентичные корпуса (сравнимость прогонов бенчмарка)
        let a = bench_root("det-a");
        let b = bench_root("det-b");
        let n1 = gen_grep_corpus(&a, 20, 30, SEED).unwrap();
        let n2 = gen_grep_corpus(&b, 20, 30, SEED).unwrap();
        assert_eq!(n1, n2);
        let sum = |d: &Path| -> u64 {
            std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .map(|e| e.metadata().map(|m| m.len()).unwrap_or(0))
                .sum()
        };
        assert_eq!(sum(&a), sum(&b), "размеры корпусов идентичны");
        let _ = std::fs::remove_dir_all(&a);
        let _ = std::fs::remove_dir_all(&b);
    }

    #[test]
    fn lexical_golden_bridge_unlocks_en_corpus() {
        // регрессия §8.1: русский запрос → EN-only документ ТОЛЬКО через мост
        let mut ix = WebIndex::open_memory().unwrap();
        for doc in lexical_corpus(40) {
            ix.upsert_page(&doc).unwrap();
        }
        // без моста — ноль (в корпусе нет кириллических термов запроса)
        let plain = ix.search("блокировка мьютекса", 5).unwrap();
        assert!(plain.is_empty(), "без моста должно быть пусто: {plain:?}");
        // с мостом — top-1 mutex-документ
        let bridge = SemanticBridge::offline();
        let (hits, exp) = ix
            .search_with_bridge("блокировка мьютекса", 5, &bridge)
            .unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].url, "https://bench.io/mutex");
        assert!(exp.extra_terms().iter().any(|t| t == "mutex"));
    }

    #[test]
    fn lexical_golden_mixed_queries_top1() {
        // все golden-запросы (рус/англ/смешанные) ранжируются в ожидаемый top-1
        let mut ix = WebIndex::open_memory().unwrap();
        for doc in lexical_corpus(60) {
            ix.upsert_page(&doc).unwrap();
        }
        let bridge = SemanticBridge::offline();
        for (q, expected) in GOLDEN_QUERIES.iter() {
            let (hits, _) = ix.search_with_bridge(q, 5, &bridge).unwrap();
            let top1 = hits.first().map(|h| h.url.clone());
            assert_eq!(
                top1.as_deref(),
                Some(*expected),
                "golden «{q}»: top1={top1:?}, ожидалось {expected}"
            );
        }
    }

    #[test]
    fn passage_poler_integrity_beats_naive() {
        let text = gen_markdown(12, SEED ^ 0xACC1);
        let cfg = ChunkConfig {
            target_tokens: 64,
            ..Default::default()
        };
        let report = chunk_document(&text, ChunkFormat::Markdown, &cfg);
        assert!(report.chunks.len() > 1, "документ обязан нарезаться");

        // инвариант чанка: text == точный срез исходника
        for c in &report.chunks {
            assert_eq!(
                &text[c.byte_start..c.byte_end],
                c.text,
                "чанк {} не является срезом исходника",
                c.index
            );
        }

        let poler: Vec<&str> = report.chunks.iter().map(|c| c.text.as_str()).collect();
        let poler_i = sentence_integrity(&poler);
        assert!(poler_i >= 90.0, "POLER целостность {poler_i}% < 90%");

        let mean = (report
            .chunks
            .iter()
            .map(|c| c.text.chars().count())
            .sum::<usize>()
            / report.chunks.len())
        .max(64);
        let naive = naive_split(&text, mean);
        let naive_refs: Vec<&str> = naive.iter().map(|s| s.as_str()).collect();
        let naive_i = sentence_integrity(&naive_refs);
        assert!(naive_i < 50.0, "naive целостность {naive_i}% unexpectedly high");
        assert!(
            poler_i > naive_i,
            "POLER обязан бить naive: {poler_i} vs {naive_i}"
        );
    }

    #[test]
    fn naive_split_window_respected() {
        let text = "abcdefghij".repeat(100);
        let chunks = naive_split(&text, 25);
        assert!(chunks.iter().all(|c| c.chars().count() <= 25));
        assert_eq!(chunks.len(), 40); // 1000 символов / 25
        // пустой текст и нулевое окно не паникуют
        assert_eq!(naive_split("", 25), vec!["".to_string()]);
        assert_eq!(naive_split("abc", 0), vec!["abc".to_string()]);
    }

    #[test]
    fn proc_status_available_on_linux() {
        // на Linux-хосте (CI и прод) снимок RAM обязан читаться
        if cfg!(target_os = "linux") {
            let r = read_proc_status().expect("/proc/self/status парсится");
            assert!(r.vm_hwm_kb > 0);
            assert!(r.vm_rss_kb > 0);
        }
    }

    #[test]
    fn run_suite_small_smoke() {
        // полный конвейер на мини-параметрах — без паник и мусора
        let opts = BenchOpts {
            grep_files: 25,
            grep_lines: 30,
            lexical_docs: 40,
            runs: 2,
            passage_sections: 8,
            passage_target_tokens: 64,
        };
        let res = run_suite(&opts).expect("suite");
        assert!(res.exact.completeness_ok);
        assert!(res.lexical.golden_ok);
        assert!(res.passage.poler_chunks > 1);
        assert!(res.resources.is_some());
    }
}

/// v0.22.0: текст бенчмарк-отчёта как String (общий для CLI --benchmark
/// и команды `benchmark` в Terminal Gateway). Перенесён из main.rs.
pub fn report_text(res: &BenchResults) -> String {
    let mut s = String::new();
    s.push_str("POLER Engine Benchmark Suite\n");
    s.push_str("══════════════════════════════════════════════════\n");

    s.push_str("[0] Literal Prefilter — Teddy SIMD vs Aho-Corasick (v2.0)\n");
    s.push_str(&format!(
        "    корпус: {:.1} MB, {} паттернов, полный скан без хитов\n",
        res.prefilter.corpus_bytes as f64 / 1048576.0,
        res.prefilter.patterns
    ));
    s.push_str(&format!(
        "    Aho-Corasick:  {:8.2} мс  ({:.2} GB/s)\n",
        res.prefilter.ac_fullscan_ms,
        res.prefilter.corpus_bytes as f64 / 1e9 / (res.prefilter.ac_fullscan_ms / 1000.0).max(1e-9)
    ));
    s.push_str(&format!(
        "    Teddy SIMD:    {:8.2} мс  ({:.2} GB/s) — ускорение {:.2}×{}\n",
        res.prefilter.teddy_fullscan_ms,
        res.prefilter.corpus_bytes as f64 / 1e9 / (res.prefilter.teddy_fullscan_ms / 1000.0).max(1e-9),
        res.prefilter.speedup_x,
        if res.prefilter.agree { " ✓ согласие" } else { " ✗ РАСХОЖДЕНИЕ" }
    ));
    s.push_str(&format!(
        "    Кириллица: было {:8.2} мс (to_lowercase+N×contains) → стало {:8.2} мс (фолд+Teddy) — {:.2}×\n",
        res.prefilter.cyr_old_ms, res.prefilter.cyr_teddy_ms, res.prefilter.cyr_speedup_x
    ));

    s.push('\n');
    s.push_str("[1] Exact Retrieval — POLER Native Grep vs эталон\n");
    s.push_str(&format!(
        "    корпус: {} файлов × {} строк (шаблон {})\n",
        res.exact.files, res.exact.lines, NEEDLE
    ));
    s.push_str(&format!(
        "    POLER grep:  {:8.1} мс — {} совпавших строк (ожидалось {}){}\n",
        res.exact.poler_ms,
        res.exact.poler_matched_lines,
        res.exact.expected_matched_lines,
        if res.exact.completeness_ok { " ✓ полнота" } else { " ✗ ПОТЕРИ" }
    ));
    if let Some(r) = &res.exact.reference {
        let parity = if res.exact.parity == Some(true) { "✓" } else { "✗" };
        s.push_str(&format!(
            "    {}: {:8.1} мс — {} совпавших строк → parity {}\n",
            r.name, r.ms, r.matched_lines, parity
        ));
    } else {
        s.push_str("    эталон (ripgrep/grep) недоступен — parity пропущен\n");
    }

    s.push('\n');
    s.push_str("[2] Explainable Lexical — BM25 + WebRank + Semantic Bridge\n");
    s.push_str(&format!(
        "    индексация {} страниц: {:.1} мс\n",
        res.lexical.pages, res.lexical.index_ms
    ));
    s.push_str(&format!(
        "    {} golden-запросов: {:.2} мс среднее\n",
        res.lexical.queries, res.lexical.query_avg_ms
    ));
    s.push_str(&format!(
        "    golden «{}» → {} {} (мост: +{} терма)\n",
        res.lexical.golden_query,
        res.lexical.golden_top1.as_deref().unwrap_or("-"),
        if res.lexical.golden_ok { "✓" } else { "✗" },
        res.lexical.bridge_expanded_terms
    ));

    s.push('\n');
    s.push_str("[3] Passage Retrieval — POLER Chunker vs naive splitter\n");
    s.push_str(&format!("    документ: {} байт\n", res.passage.doc_bytes));
    s.push_str(&format!(
        "    POLER chunker:  {:8.2} мс — {} чанков, целостность предложений {:.1}%\n",
        res.passage.poler_ms, res.passage.poler_chunks, res.passage.poler_integrity_pct
    ));
    s.push_str(&format!(
        "    naive splitter: {:8.2} мс — {} чанков, целостность предложений {:.1}%\n",
        res.passage.naive_ms, res.passage.naive_chunks, res.passage.naive_integrity_pct
    ));

    s.push('\n');
    s.push_str("[4] Resources\n");
    if let Some(r) = &res.resources {
        s.push_str(&format!(
            "    RAM: пик {:.1} MB (VmHWM), текущая {:.1} MB (VmRSS)\n",
            r.vm_hwm_kb as f64 / 1024.0,
            r.vm_rss_kb as f64 / 1024.0
        ));
    } else {
        s.push_str("    RAM-снимок недоступен (не Linux)\n");
    }

    s
}
