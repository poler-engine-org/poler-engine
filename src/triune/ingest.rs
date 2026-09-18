//! Автономный интернет-инжектор памяти (S2/v0.37.0): потоковое обучение
//! Кристалла Знаний (.t5c) из локальных папок и веб-краула.
//!
//! ## Проблема, которую решает этот модуль
//!
//! [`crate::triune::crystal::Crystal::build`] принимает корпус одним
//! `&str` — 50 ГБ интернет-дампа в RAM не влезут (OOM). Здесь текст
//! **никогда не накапливается**: обработка идёт символ за символом,
//! в памяти живёт только:
//! - текущее слово (≤ 256 байт — предел u8-длины токена в .t5c),
//! - хвост незавершённого UTF-8 символа (≤ 3 байт),
//! - счётчики: словарь (с капом + детерминированной эвакуацией) и
//!   биграммы (с капом + эвакуацией).
//!
//! ## Архитектура
//!
//! ```text
//! [ Веб-краулер / Интернет ] ───► [ Локальные папки / Код ]
//!                │                            │
//!                ▼                            ▼
//!   1. Streaming Token Stream (символ-за-символом, RAM-буфер ≤ 256 Б)
//!      · слова = цепочки букв/цифр (паритет с tokenize);
//!      · границы предложений: . ! ? ; : \n (паритет с sentences);
//!      · Dynamic Vocab: новое слово сразу получает временный id.
//!                │
//!                ▼
//!   2. Dynamic Crystal Synapse Updater (счётчики + капы + эвакуация)
//!      · unigram: HashMap<слово, id> + Vec<u64>;
//!      · bigram:  HashMap<(id, id), u64> — пары смежных слов;
//!      · эвакуация: чисто счётчиковое правило (детерминизм).
//!                │
//!                ▼
//!   3. finalize(): ранжирование (частота ↓, лексика ↑) → квантизация
//!      PMI в Trit5 {−1, 0, +1} → Crystal::from_parts → .t5c на диске
//!                │
//!                ▼
//!   4. Мгновенный доступ: --triune-speak --triune-crystal memory.t5c
//! ```
//!
//! ## Гарантии (F-принцип: доказано до реализации)
//!
//! - **Паритет с build()**: при vocab ≥ числа уникальных слов потоковая
//!   сборка бит-в-бит совпадает с `Crystal::build` (тест
//!   `streaming_parity_with_bulk_build`).
//! - **Инвариантность к чанкам**: резать вход на чанки по 1 / 7 / 64 КБ —
//!   результат не меняется (в т.ч. сквозь границы UTF-8 символов).
//! - **Детерминизм эвакуации**: правило выживания зависит только от
//!   счётчиков, не от порядка обхода хэш-таблиц.
//! - **Честность отличия**: при vocab < уникальных слов биграммы
//!   считаются по смежным словам сырого потока; выбывшие слова не
//!   «сшивают» соседей через разрыв (в build() фильтр применяется до
//!   windows(2)) — расхождение задокументировано, на топ-словаре
//!   практически не влияет.
//!
//! ## RAM-дисциплина
//!
//! | Составляющая | Предел |
//! |---|---|
//! | текстовый буфер | 256 Б (слово) + 3 Б (UTF-8 хвост) |
//! | словарь | `word_cap` слов (по умолчанию 262 144) |
//! | биграммы | `bigram_cap` пар (по умолчанию 2 097 152) |
//!
//! При превышении капа половина ёмкости освобождается выбрасыванием
//! слов/пар с частотой ниже порога (lossy counting: тяжёлые хиты
//! переживают чистку с гарантией).

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::pqc::sha256::sha256;
use crate::triune::crystal::{
    Crystal, DEFAULT_DIMS, DEFAULT_THETA_HI, DEFAULT_THETA_LO,
};
use crate::pqc::tensor::Trit5Codec;

/// Граница предложения — та же, что у `crystal::sentences`.
fn is_sentence_delim(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | ';' | ':' | '\n')
}

/// Текстовые расширения для ингестии директорий.
const TEXT_EXTS: &[&str] = &[
    "txt", "md", "json", "rs", "c", "h", "cpp", "py", "html", "htm", "js",
    "ts", "toml", "yaml", "yml", "csv", "log", "xml", "sh", "sql", "go",
    "java", "cfg", "ini", "conf", "tex", "srt", "vtt", "rss", "atom",
];

/// Конфигурация потокового обучения.
#[derive(Debug, Clone)]
pub struct IngestConfig {
    /// Целевой размер словаря кристалла (32..=65536).
    pub vocab: usize,
    /// Размерность CSE-вложений.
    pub dims: usize,
    /// Порог притяжения PMI.
    pub theta_hi: f64,
    /// Порог отталкивания PMI.
    pub theta_lo: f64,
    /// Размер чанка чтения файлов, байт (RAM-дисциплина).
    pub chunk_bytes: usize,
    /// Максимум уникальных слов в RAM (эвакуация сверх).
    pub word_cap: usize,
    /// Максимум биграммных пар в RAM (эвакуация сверх).
    pub bigram_cap: usize,
}

impl Default for IngestConfig {
    fn default() -> Self {
        IngestConfig {
            vocab: crate::triune::crystal::DEFAULT_VOCAB.max(4096),
            dims: DEFAULT_DIMS,
            theta_hi: DEFAULT_THETA_HI,
            theta_lo: DEFAULT_THETA_LO,
            chunk_bytes: 64 * 1024,
            word_cap: 262_144,
            bigram_cap: 2_097_152,
        }
    }
}

/// Статистика процесса обучения кристалла.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestStats {
    /// Сколько источников скормлено (файлы + веб-страницы).
    pub total_sources: usize,
    /// Всего слов в потоке (включая вне словаря).
    pub total_words: u64,
    /// Всего символов UTF-8 в потоке.
    pub total_chars: u64,
    /// Уникальных слов видел билдер (до клампа словаря).
    pub unique_tokens: usize,
    /// Итоговый словарь кристалла.
    pub crystal_vocab: usize,
    /// Размер .t5c, байт.
    pub crystal_bytes: usize,
    /// sha256 артефакта.
    pub sha256_hex: String,
    /// Ненулевых биграмм в топологии.
    pub bigram_nonzeros: u64,
    /// Эвакуировано слов (переполнение word_cap).
    pub words_evicted: u64,
    /// Эвакуировано биграмм (переполнение bigram_cap).
    pub bigrams_evicted: u64,
    /// Пиковый размер текстового буфера, байт.
    pub peak_text_buffer: usize,
    /// Пропущено бинарных файлов (NUL-байт в первом чанке).
    pub files_skipped_binary: usize,
}

/// Потоковый строитель Кристалла Знаний: текст льётся чанками,
/// RAM остаётся ограниченной, результат — тот же .t5c.
pub struct StreamCrystalBuilder {
    cfg: IngestConfig,
    /// слово → динамический id (растёт по мере встречи новых слов).
    id_of: HashMap<String, u32>,
    /// id → слово (после эвакуации строка освобождается).
    words: Vec<String>,
    /// id → частота (параллельно words).
    counts: Vec<u64>,
    /// Биграммы смежных слов потока (динамические id).
    bigram: HashMap<(u32, u32), u64>,
    /// Слово в обработке (между двумя не-alphanumeric символами).
    word_buf: String,
    /// Слово превысило 255 байт — в словарь не попадёт (u8-длина .t5c).
    word_overflow: bool,
    /// Хвост незавершённого UTF-8 символа (≤ 3 байт).
    utf8_tail: Vec<u8>,
    /// Последнее слово текущего предложения (None = была граница).
    prev: Option<u32>,
    /// Статистика.
    corpus_words: u64,
    corpus_chars: u64,
    sources: usize,
    words_evicted: u64,
    bigrams_evicted: u64,
    peak_text_buffer: usize,
    files_skipped_binary: usize,
}

impl StreamCrystalBuilder {
    /// Новый билдер с заданной конфигурацией.
    pub fn new(cfg: IngestConfig) -> StreamCrystalBuilder {
        StreamCrystalBuilder {
            cfg,
            id_of: HashMap::new(),
            words: Vec::new(),
            counts: Vec::new(),
            bigram: HashMap::new(),
            word_buf: String::new(),
            word_overflow: false,
            utf8_tail: Vec::new(),
            prev: None,
            corpus_words: 0,
            corpus_chars: 0,
            sources: 0,
            words_evicted: 0,
            bigrams_evicted: 0,
            peak_text_buffer: 0,
            files_skipped_binary: 0,
        }
    }

    /// Скормить готовый фрагмент текста (UTF-8 корректен by construction).
    pub fn push_str(&mut self, text: &str) {
        self.sources += 1;
        self.consume(text);
    }

    /// Скормить сырые байты: незавершённый UTF-8 символ, разрезанный
    /// границей чанка, переносится в следующий (хвост ≤ 3 байт).
    /// Безнадёжно битые последовательности замещаются U+FFFD и
    /// пропускаются — хвост не может расти бесконечно.
    pub fn push_chunk(&mut self, bytes: &[u8]) {
        let mut buf = std::mem::take(&mut self.utf8_tail);
        buf.extend_from_slice(bytes);
        loop {
            match std::str::from_utf8(&buf) {
                Ok(s) => {
                    self.consume(s);
                    buf.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        // безопасно: префикс валиден по определению valid_up_to
                        let s = unsafe { std::str::from_utf8_unchecked(&buf[..valid]) };
                        self.consume(s);
                        buf.drain(..valid);
                    }
                    match e.error_len() {
                        Some(bad) => {
                            // битая последовательность: замена + пропуск
                            self.consume("\u{FFFD}");
                            let skip = bad.max(1).min(buf.len());
                            buf.drain(..skip);
                        }
                        None => break, // незавершённый символ: ждём продолжения
                    }
                }
            }
        }
        self.utf8_tail = buf;
        self.note_buffer_peak();
    }

    /// Завершить поток: слово без закрывающего разделителя, хвост UTF-8.
    pub fn finish(&mut self) {
        if !self.utf8_tail.is_empty() {
            let s = String::from_utf8_lossy(&self.utf8_tail).into_owned();
            self.utf8_tail.clear();
            self.consume(&s);
        }
        self.emit_word();
        self.prev = None;
        self.note_buffer_peak();
    }

    /// Символ-за-символом: слово накапливаем, разделитель — сбрасывает
    /// предложение, прочие не-буквы просто разделяют слова.
    fn consume(&mut self, text: &str) {
        for ch in text.chars() {
            self.corpus_chars += 1;
            if ch.is_alphanumeric() {
                for lc in ch.to_lowercase() {
                    if self.word_buf.len() < 256 {
                        self.word_buf.push(lc);
                    } else {
                        self.word_overflow = true;
                    }
                }
            } else {
                self.emit_word();
                if is_sentence_delim(ch) {
                    self.prev = None;
                }
            }
        }
    }

    /// Выбросить накопленное слово в счётчики.
    fn emit_word(&mut self) {
        if self.word_buf.is_empty() {
            return;
        }
        let word = std::mem::take(&mut self.word_buf);
        let overflow = std::mem::replace(&mut self.word_overflow, false);
        self.corpus_words += 1;
        if overflow || word.len() > u8::MAX as usize {
            // в словарь не попадёт ни при каком ранжировании
            return;
        }
        // Dynamic Vocab: новое слово сразу получает временный id.
        let next_id = match self.id_of.get(&word) {
            Some(&id) => id,
            None => {
                let id = self.words.len() as u32;
                self.words.push(word.clone());
                self.counts.push(0);
                self.id_of.insert(word, id);
                id
            }
        };
        self.counts[next_id as usize] += 1;
        if let Some(p) = self.prev {
            *self.bigram.entry((p, next_id)).or_insert(0) += 1;
        }
        self.prev = Some(next_id);
        self.maybe_evict();
    }

    /// Отметить пик текстового буфера (дисциплина RAM).
    fn note_buffer_peak(&mut self) {
        let now = self.word_buf.len() + self.utf8_tail.len();
        if now > self.peak_text_buffer {
            self.peak_text_buffer = now;
        }
    }

    /// Детерминированная эвакуация: остаются топ-половина ёмкости по
    /// (частота ↓, id ↑). Правило не зависит от порядка обхода хэш-таблиц,
    /// одинаковый вход → одинаковое выживание (тест детерминизма).
    fn maybe_evict(&mut self) {
        if self.id_of.len() > self.cfg.word_cap {
            let target = (self.cfg.word_cap / 2).max(1);
            let mut alive: Vec<(u64, u32)> = (0..self.words.len() as u32)
                .filter(|&id| self.counts[id as usize] > 0 && !self.words[id as usize].is_empty())
                .map(|id| (self.counts[id as usize], id))
                .collect();
            alive.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let survivors: std::collections::HashSet<u32> =
                alive[..target.min(alive.len())].iter().map(|&(_, id)| id).collect();
            let dead: Vec<u32> = (0..self.words.len() as u32)
                .filter(|&id| self.counts[id as usize] > 0 && !survivors.contains(&id))
                .collect();
            self.words_evicted += dead.len() as u64;
            for id in dead {
                let w = std::mem::take(&mut self.words[id as usize]);
                if !w.is_empty() {
                    self.id_of.remove(&w);
                }
                self.counts[id as usize] = 0;
            }
        }
        if self.bigram.len() > self.cfg.bigram_cap {
            let target = (self.cfg.bigram_cap / 2).max(1);
            let mut alive: Vec<(u64, (u32, u32))> =
                self.bigram.iter().map(|(&k, &c)| (c, k)).collect();
            alive.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
            let survivors: std::collections::HashSet<(u32, u32)> =
                alive[..target.min(alive.len())].iter().map(|&(_, k)| k).collect();
            let before = self.bigram.len() as u64;
            self.bigram.retain(|k, _| survivors.contains(k));
            self.bigrams_evicted += before - self.bigram.len() as u64;
        }
    }

    /// Ингестировать файл потоково (чанки `chunk_bytes`; NUL-байт в
    /// первом чанке = бинарник, файл пропускается). Возвращает
    /// `true`, если файл реально прочитан, `false` — пропущен как бинарник.
    pub fn feed_file<P: AsRef<Path>>(&mut self, path: P) -> std::io::Result<bool> {
        let mut file = std::fs::File::open(path.as_ref())?;
        let mut first = true;
        let mut buf = vec![0u8; self.cfg.chunk_bytes.max(1024)];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            if first && buf[..n].contains(&0) {
                self.files_skipped_binary += 1;
                return Ok(false); // бинарник — не текст
            }
            first = false;
            self.push_chunk(&buf[..n]);
        }
        self.finish();
        self.sources += 1;
        Ok(true)
    }

    /// Ингестировать директорию рекурсивно (текстовые расширения).
    /// Возвращает число прочитанных файлов.
    pub fn feed_dir<P: AsRef<Path>>(&mut self, dir: P) -> std::io::Result<usize> {
        let mut count = 0usize;
        let entries = std::fs::read_dir(dir.as_ref())?;
        let mut paths: Vec<std::path::PathBuf> =
            entries.filter_map(|e| e.ok().map(|e| e.path())).collect();
        // детерминизм обхода: сортировка имён (платформо-независимая)
        paths.sort();
        for path in paths {
            if path.is_dir() {
                count += self.feed_dir(&path)?;
            } else if let Some(ext) =
                path.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase())
            {
                if TEXT_EXTS.contains(&ext.as_str()) && self.feed_file(&path).unwrap_or(false) {
                    count += 1;
                }
            }
        }
        Ok(count)
    }

    /// Финальная сборка: ранжирование → квантизация PMI → .t5c.
    /// Потребляет билдер (повторный вызов невозможен).
    pub fn finalize(mut self) -> Result<(Crystal, IngestStats), String> {
        self.finish();
        if self.corpus_words < 16 {
            return Err(format!(
                "корпус слишком мал: {} слов (нужно ≥ 16)",
                self.corpus_words
            ));
        }
        let cfg = self.cfg.clone();

        // ── Ранжирование словаря: частота ↓, лексика ↑ (паритет build) ──
        let mut ranked: Vec<(u32, u64)> = (0..self.words.len() as u32)
            .filter(|&id| self.counts[id as usize] > 0)
            .map(|id| (id, self.counts[id as usize]))
            .collect();
        ranked.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| self.words[a.0 as usize].cmp(&self.words[b.0 as usize]))
        });
        let vocab = if ranked.len() < 32 {
            ranked.len()
        } else {
            cfg.vocab.clamp(32, ranked.len())
        };
        if vocab == 0 {
            return Err("после фильтра словаря поток пуст".into());
        }
        let unique_tokens = ranked.len();

        // ── Перенумерация: динамический id → канонический id ──
        let mut remap = vec![u32::MAX; self.words.len()];
        for (new_id, &(old_id, _)) in ranked[..vocab].iter().enumerate() {
            remap[old_id as usize] = new_id as u32;
        }
        let tokens: Vec<String> = ranked[..vocab]
            .iter()
            .map(|&(old_id, _)| self.words[old_id as usize].clone())
            .collect();
        let mut uni = vec![0u64; vocab];
        let mut total: u64 = 0;
        for &(old_id, c) in &ranked[..vocab] {
            uni[remap[old_id as usize] as usize] = c;
            total += c;
        }
        if total < 8 {
            return Err("после фильтра словаря поток пуст".into());
        }

        // ── Биграммы: фильтр выживших + ремапа, порядок детерминирован ──
        let mut pairs: Vec<((u32, u32), u64)> = self
            .bigram
            .iter()
            .filter(|(&(a, b), _)| remap[a as usize] != u32::MAX && remap[b as usize] != u32::MAX)
            .map(|(&k, &v)| ((remap[k.0 as usize], remap[k.1 as usize]), v))
            .collect();
        pairs.sort_by_key(|&(p, n)| (p, n));

        // ── Квантизация PMI в Trit5 (формулы — паритет Crystal::build) ──
        let cols_packed = vocab.div_ceil(5);
        let zero_byte = Trit5Codec::pack_5(&[0i8; 5]).unwrap_or(121);
        let mut bigram = vec![zero_byte; vocab * cols_packed];
        let mut bigram_nonzeros: u64 = 0;
        for ((prev, next), c) in pairs {
            let (prev, next) = (prev as usize, next as usize);
            let p_next = uni[next] as f64 / total as f64;
            let p_cond = c as f64 / uni[prev].max(1) as f64;
            let r = p_cond / p_next.max(1e-12);
            let trit: i8 = if r >= cfg.theta_hi {
                1
            } else if r <= cfg.theta_lo {
                -1
            } else {
                0
            };
            let byte = &mut bigram[prev * cols_packed + next / 5];
            let mut five = Trit5Codec::unpack_5(*byte);
            five[next % 5] = trit;
            if let Some(b) = Trit5Codec::pack_5(&five) {
                *byte = b;
            }
            if trit != 0 {
                bigram_nonzeros += 1;
            }
        }

        let crystal = Crystal::from_parts(
            tokens,
            bigram,
            self.corpus_words,
            self.corpus_chars,
            bigram_nonzeros,
            cfg.theta_hi,
            cfg.theta_lo,
            cfg.dims,
        )?;
        let bytes = crystal.to_bytes();
        let digest = sha256(&bytes);
        let mut sha256_hex = String::with_capacity(64);
        for b in &digest {
            use std::fmt::Write;
            let _ = write!(&mut sha256_hex, "{b:02x}");
        }
        let stats = IngestStats {
            total_sources: self.sources,
            total_words: self.corpus_words,
            total_chars: self.corpus_chars,
            unique_tokens,
            crystal_vocab: crystal.vocab(),
            crystal_bytes: bytes.len(),
            sha256_hex,
            bigram_nonzeros,
            words_evicted: self.words_evicted,
            bigrams_evicted: self.bigrams_evicted,
            peak_text_buffer: self.peak_text_buffer,
            files_skipped_binary: self.files_skipped_binary,
        };
        Ok((crystal, stats))
    }

    /// Совместимость со старым именем (v0.36.0 CrystalIngestor).
    pub fn compile(self) -> Result<(Crystal, IngestStats), String> {
        self.finalize()
    }
}

/// Декоратор источника страниц: каждая викачанная страница немедленно
/// льётся в билдер кристалла (Crawl → Ingestion Pipeline).
///
/// Краулер продолжает писать страницы в свой SQLite-индекс —
/// декоратор лишь «подслушивает» поток текстов, не меняя поведение
/// обхода (robots, politeness, дедупликация — без изменений).
pub struct IngestingFetcher<'a> {
    inner: Box<dyn crate::web::crawl::PageFetcher>,
    builder: &'a mut StreamCrystalBuilder,
    /// Сколько страниц скормлено в кристалл.
    pub pages: usize,
}

impl<'a> IngestingFetcher<'a> {
    pub fn new(
        inner: Box<dyn crate::web::crawl::PageFetcher>,
        builder: &'a mut StreamCrystalBuilder,
    ) -> IngestingFetcher<'a> {
        IngestingFetcher { inner, builder, pages: 0 }
    }
}

/// Реализация PageFetcher: страница проходит насквозь, текст — в билдер.
impl<'a> crate::web::crawl::PageFetcher for IngestingFetcher<'a> {
    fn fetch(&mut self, url: &str) -> Result<crate::web::crawl::FetchedPage, String> {
        let page = self.inner.fetch(url)?;
        let mut text = String::with_capacity(page.text.len() + page.title.len() + 2);
        if !page.title.is_empty() {
            text.push_str(&page.title);
            text.push('\n');
        }
        text.push_str(&page.text);
        self.builder.push_str(&text);
        self.pages += 1;
        Ok(page)
    }

    fn fetch_raw(&mut self, url: &str) -> Result<(u16, String), String> {
        self.inner.fetch_raw(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::triune::crystal::Crystal;

    const CORPUS: &str = "Мозг мухи держит ритм мысли. Вихрь крутит смысл по кругу. \
        Кристалл хранит слова мира. Мозг говорит через кристалл. Вихрь диспетчеризует внимание. \
        Муха задаёт пульс системы. Система слушает вход. Вход течёт через кодировщик. \
        Кодировщик превращает текст в вектор. Вектор вливается в нейроны. \
        Открой терминал и покажи статус. Запусти сборку проекта. Покажи логи системы. \
        Живой мозг дышит синусоидой. Ротор разводит слова по архетипам касты. \
        Триты живут без умножения. Решётка квантов держит синтаксис речи.";

    fn cfg(vocab: usize) -> IngestConfig {
        IngestConfig {
            vocab,
            dims: 64,
            theta_hi: 1.7,
            theta_lo: 0.5,
            chunk_bytes: 64 * 1024,
            word_cap: 1 << 20,
            bigram_cap: 1 << 21,
        }
    }

    #[test]
    fn streaming_parity_with_bulk_build() {
        let bulk = Crystal::build(CORPUS, 4096, 64, 1.7, 0.5).unwrap();
        let mut b = StreamCrystalBuilder::new(cfg(4096));
        b.push_str(CORPUS);
        let (streamed, _) = b.finalize().unwrap();
        assert_eq!(
            bulk.to_bytes(),
            streamed.to_bytes(),
            "потоковая сборка обязана совпадать с build бит-в-бит (vocab ≥ уникальных)"
        );
    }

    #[test]
    fn chunk_invariance_including_utf8_boundaries() {
        // Корпус с кириллицей режется на чанки по 1..13 байт: UTF-8
        // символы гарантированно рвутся посередине.
        for size in [1usize, 3, 7, 13, 64] {
            let mut b = StreamCrystalBuilder::new(cfg(4096));
            for chunk in CORPUS.as_bytes().chunks(size) {
                b.push_chunk(chunk);
            }
            b.finish();
            let (c, _) = b.finalize().unwrap();
            let bulk = Crystal::build(CORPUS, 4096, 64, 1.7, 0.5).unwrap();
            assert_eq!(
                bulk.to_bytes(),
                c.to_bytes(),
                "чанки по {size} байт ломают паритет"
            );
        }
    }

    #[test]
    fn finish_flushes_pending_word() {
        // Нет закрывающего разделителя — последнее слово не теряется.
        let text = "кристалл памяти держит синтаксис живого мозга мухи вихря тритов ротора \
            синусоиды решётки квантов кодировщика нейронов внимания системы";
        let mut b = StreamCrystalBuilder::new(cfg(4096));
        b.push_str(text);
        let (c, stats) = b.finalize().unwrap();
        let words: usize = text.split_whitespace().count();
        assert_eq!(stats.total_words, words as u64, "последнее слово на месте");
        assert!(c.id_of("синтаксис").is_some());
    }

    #[test]
    fn eviction_is_deterministic_and_keeps_top_words() {
        // Зипф-подобный поток: 8 тяжёлых слов + сотни одноразовых.
        let mut text = String::new();
        let heavy = ["мозг", "вихрь", "кристалл", "муха", "ротор", "трит", "синус", "код"];
        for rep in 0..40 {
            for (i, h) in heavy.iter().enumerate() {
                for _ in 0..(20 - i) {
                    text.push_str(h);
                    text.push(' ');
                }
                text.push_str(&format!("шум(rep={rep},{i}) "));
            }
            text.push_str(&format!("хлам(rep={rep}). "));
        }
        let mk = |cap: usize| IngestConfig {
            vocab: 64,
            dims: 64,
            theta_hi: 1.7,
            theta_lo: 0.5,
            chunk_bytes: 64 * 1024,
            word_cap: cap,
            bigram_cap: 1 << 21,
        };
        let mut a = StreamCrystalBuilder::new(mk(48)); // меньше числа уникальных
        a.push_str(&text);
        let (ca, sa) = a.finalize().unwrap();
        let mut b = StreamCrystalBuilder::new(mk(48));
        b.push_str(&text);
        let (cb, _) = b.finalize().unwrap();
        assert_eq!(ca.to_bytes(), cb.to_bytes(), "эвакуация детерминирована");
        assert!(sa.words_evicted > 0, "эвакуация обязана была сработать");
        assert!(sa.peak_text_buffer <= 256 + 3, "RAM-дисциплина текста");
        for h in heavy {
            assert!(ca.id_of(h).is_some(), "тяжёлое слово «{h}» пережило эвакуацию");
        }
    }

    #[test]
    fn bigram_cap_eviction_works() {
        // Длинный поток уникальных пар → эвакуация биграмм, детерминизм.
        let mut text = String::new();
        for i in 0..600 {
            text.push_str(&format!("слово{i} рядом{i} уникализм{i}. "));
        }
        let mk = || IngestConfig {
            vocab: 4096,
            dims: 64,
            theta_hi: 1.7,
            theta_lo: 0.5,
            chunk_bytes: 64 * 1024,
            word_cap: 1 << 20,
            bigram_cap: 200,
        };
        let mut a = StreamCrystalBuilder::new(mk());
        a.push_str(&text);
        let (_, sa) = a.finalize().unwrap();
        let mut b = StreamCrystalBuilder::new(mk());
        b.push_str(&text);
        let (_, sb) = b.finalize().unwrap();
        assert_eq!(sa.sha256_hex, sb.sha256_hex, "эвакуация биграмм детерминирована");
        assert!(sa.bigrams_evicted > 0, "кап биграмм обязан сработать");
    }

    #[test]
    fn learn_dir_walk_and_binary_skip() {
        let dir = std::env::temp_dir().join(format!("poler-ingest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
        std::fs::write(dir.join("a.txt"), "кристалл знаний помнит слова мира и держит ритм речи. \
            мозг мухи крутит вихрь смысла по кругу жизни.").unwrap();
        std::fs::write(dir.join("sub/b.md"), "# мозг мухи\nвихрь крутит смысл слова вектор трит решётка квант ротор синус.\n").unwrap();
        std::fs::write(dir.join("sub/deep/c.log"), "трит без умножения живёт в машинном коде процессора.\n").unwrap();
        // текстовое расширение, но бинарное содержимое → NUL-детект обязан сработать
        std::fs::write(dir.join("fake.txt"), b"\x00\x01\x02binary").unwrap();
        std::fs::write(dir.join("skip.zst"), [0u8; 64]).unwrap();
        let mut b = StreamCrystalBuilder::new(cfg(4096));
        let files = b.feed_dir(&dir).unwrap();
        let (c, stats) = b.finalize().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(files, 3, "три текстовых файла, .dat/.zst отсеяны");
        assert!(stats.files_skipped_binary >= 1, "бинарник распознан и пропущен");
        assert!(c.id_of("кристалл").is_some());
        assert!(c.id_of("мозг").is_some());
        assert!(c.id_of("трит").is_some());
    }

    /// Мок-источник страниц для проверки Crawl → Ingestion пайплайна.
    struct MockFetch {
        pages: std::collections::VecDeque<crate::web::crawl::FetchedPage>,
    }

    impl crate::web::crawl::PageFetcher for MockFetch {
        fn fetch(&mut self, _url: &str) -> Result<crate::web::crawl::FetchedPage, String> {
            self.pages.pop_front().ok_or_else(|| "frontier пуст".to_string())
        }
        fn fetch_raw(&mut self, _url: &str) -> Result<(u16, String), String> {
            Ok((404, String::new())) // robots/sitemap нет — можно всё
        }
    }

    fn mock_page(title: &str, text: &str, links: Vec<&str>) -> crate::web::crawl::FetchedPage {
        crate::web::crawl::FetchedPage {
            final_url: "https://mock.local/".into(),
            title: title.into(),
            meta_description: String::new(),
            lang: "uk".into(),
            text: text.into(),
            links: links.into_iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn ingesting_fetcher_feeds_builder_during_crawl() {
        use crate::web::crawl::{crawl, CrawlConfig};
        use crate::web::WebIndex;
        let mut ix = WebIndex::open_memory().unwrap();
        let mock = MockFetch {
            pages: vec![
                mock_page(
                    "Перша",
                    "мозг мухи держит ритм пульс жизни и дыхание синусоиды. вихрь крутит смысл по кругу архетипов.",
                    vec!["https://mock.local/p2"],
                ),
                mock_page("Друга", "кристалл хранит слова мира и триты решётки без умножения.", vec![]),
            ]
            .into(),
        };
        let mut b = StreamCrystalBuilder::new(cfg(4096));
        {
            let mut fetcher = IngestingFetcher::new(Box::new(mock), &mut b);
            let ccfg = CrawlConfig {
                max_pages: 2,
                max_depth: 1, // ссылка первой страницы ведёт на вторую
                ..CrawlConfig::default()
            };
            let stats = crawl(&mut ix, &mut fetcher, "https://mock.local/", &ccfg, false)
                .expect("краулер обязан пройти по моку");
            assert!(stats.fetched >= 1, "страницы качаются");
            assert_eq!(fetcher.pages, 2, "обе страницы скормлены в кристалл");
        }
        let (c, stats) = b.finalize().unwrap();
        assert!(stats.total_sources >= 2, "тексты страниц + заголовки в билдере");
        assert!(c.id_of("мозг").is_some());
        assert!(c.id_of("кристалл").is_some());
        assert!(c.id_of("перша").is_some(), "заголовок страницы тоже учится");
    }

    #[test]
    fn empty_stream_is_rejected() {
        let b = StreamCrystalBuilder::new(cfg(64));
        assert!(b.finalize().is_err(), "пустой поток — ошибка, не паника");
    }
}
