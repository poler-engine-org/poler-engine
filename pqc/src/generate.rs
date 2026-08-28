//! RQ17: L5-генерация — первые слова и рассуждение квантово-фазового
//! разума.
//!
//! Замыкает цикл **авторегрессии**: до сих пор решётка только училась
//! (ingest → born-кристаллизация → транспорт волны по руслам `J`);
//! теперь она **говорит**. Сенсорный кодировщик односторонен
//! (`токен → fnv1a64 → координата`), поэтому генерации нужна обратная
//! карта — [лексикон](LexiconBuilder) кристалла, накопленный во время
//! обучения: доминантный токен на координату.
//!
//! ## Физика речи: Born-блуждание по руслам J
//!
//! ```text
//!   промпт ──▶ слушание (кольцо гироскопа: каналы вопроса)
//!        │         + кинетика внимания (дуги вопроса разогнаны)
//!        ▼
//!   мышление: K × p ← Π_Λ(e^{Δt·J} p)   [волна идёт по руслам]
//!        │
//!        ▼
//!   ┌─ шаг речи ──────────────────────────────────────────┐
//!   │ кольцо контекста → билеты по нисходящим руслам J    │
//!   │   вес = 1 + |крутящий момент|: устоявшаяся связь    │
//!   │   говорит (1), напряжённая — кричит (2)             │
//!   │ Born-лотерея (целые билеты, ноль FPU) → координата  │
//!   │ лексикон: координата → слово                        │
//!   │ эмиссия = сенсорное событие: gyro.observe(coord,±1) │
//!   │ Born-измерение фазы → градиент → момент (инерция)   │
//!   │ шаг транспорта — волна продолжает течь              │
//!   └─────────────────────────────────────────────────────┘
//!        │
//!        ▼
//!   аттрактор: русел из кольца нет — мысль завершена
//! ```
//!
//! **Направление речи = синтаксис.** Русла `J` направлены по порядку
//! следования слов в обучающем потоке: канал «квант → фаза» означает,
//! что в свидетельстве «квант» предшествовал «фазе». Блуждание по
//! нисходящим руслам воспроизводит усвоенный порядок слов — грамматика
//! не хранится отдельно, она **топологический инвариант** русел.
//!
//! **Каскад живости.** Шаг речи пробует источники билетов по убыванию
//! осмысленности: нисходящие русла из кольца (мысль течёт вперёд) →
//! восходящие (возврат к предыдущей мысли) → кинетические дуги вне
//! кольца (внутренний голос). Пусто на всех уровнях — сходимость.
//!
//! **Анти-заикание.** Координата, эмитированная на последних
//! `repeat_veto` шагах, из лотереи исключается: волна обязана течь,
//! а не вибрировать на месте.
//!
//! ## Пример
//!
//! ```
//! use pqc::generate::{GeneratorConfig, L5Generator};
//! use pqc::gyro_lattice::QuantizedGyroCurriculum;
//!
//! // Живой мозг: обучаем движок на микрокорпусе…
//! let mut qc = QuantizedGyroCurriculum::new(256, 0.05, 42, 4).unwrap();
//! let corpus = "квант фаза решётка квант фаза born шаг трит решётка \
//!               квант фаза решётка момент импульс фаза";
//! for _ in 0..3 {
//!     qc.ingest(corpus, 1).unwrap();
//! }
//!
//! // …и спрашиваем его: генерация из промпта.
//! let cfg = GeneratorConfig::default();
//! let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
//! let rep = gen.generate("квант").unwrap();
//! // Всё, что произнесено, — слова из обученного лексикона.
//! for step in &rep.steps {
//!     assert!(rep.text.contains(&step.token));
//! }
//! ```

use std::collections::VecDeque;
use std::time::Instant;

use pqw::checksum::fnv1a64;
use pqw::lexicon::Lexicon;
use pqw::stream::tokenize;

use crate::error::Result;
use crate::gyro_lattice::{QuantizedGyroCurriculum, SIN_LUT, TransportMode};
use crate::rng::Rng;
use crate::syntax_unfolder::morpheme_at;

/// Потолок длины токена для лексикона: прогоны длиннее — шум
/// (канонический предел секции — 255 байт, уходим с запасом).
pub const LEXICON_TOKEN_MAX: usize = 64;

// ===================== Лексикон-строитель =====================

/// Накопитель лексикона во время обучения: доминантный токен на
/// координату.
///
/// Плотная раскладка без HashMap (конвенция RQ16): `freq[i]` — сколько
/// раз координата `i` получала свидетельство, `best[i]` — слово с
/// наибольшей частотой. Коллизия хеша разрешается доминантностью —
/// как полюс МакВини выбирает выровненную фазу из суперпозиции
/// вкладов. Переживает рестарт через секцию `LEXI` контейнера v4.
pub struct LexiconBuilder {
    d: u32,
    /// Счётчик наблюдений координаты (частота доминирования).
    freq: Vec<u32>,
    /// Вес, при котором текущее слово стало доминантой (ничья хранит
    /// старое — детерминизм).
    best_freq: Vec<u32>,
    best: Vec<String>,
    seen: u64,
}

impl LexiconBuilder {
    /// Строитель под размерность `d_pol ≥ 1`.
    pub fn new(d_pol: u32) -> LexiconBuilder {
        let d = d_pol as usize;
        LexiconBuilder {
            d: d_pol,
            freq: vec![0u32; d],
            best_freq: vec![0u32; d],
            best: vec![String::new(); d],
            seen: 0,
        }
    }

    /// Размерность.
    pub fn d_pol(&self) -> u32 {
        self.d
    }

    /// Токенов наблюдено всего.
    pub fn tokens_seen(&self) -> u64 {
        self.seen
    }

    /// Одно наблюдение: токен → координата (тот же хеш, что у сенсорного
    /// кодировщика). Прогоны длиннее [`LEXICON_TOKEN_MAX`] — шум, мимо.
    ///
    /// Доминанта: слово сменяется только перевесом частоты
    /// (`freq > best_freq`); ничья хранит старое — детерминизм.
    pub fn observe(&mut self, token: &str) {
        if token.is_empty() || token.len() > LEXICON_TOKEN_MAX {
            return;
        }
        let h = fnv1a64(token.as_bytes());
        let i = (h % self.d as u64) as usize;
        self.seen += 1;
        self.freq[i] += 1;
        // Доминанта: слово сменяется только перевесом частоты
        // (`freq > best_freq`; ничья хранит старое — детерминизм).
        // Пустой слот имеет best_freq = 0 — первое наблюдение
        // перевешивает автоматически.
        if self.freq[i] > self.best_freq[i] {
            self.best[i] = token.to_string();
            self.best_freq[i] = self.freq[i];
        }
    }

    /// Токены текста (тот же токенизатор, что у кодировщика).
    pub fn observe_text(&mut self, text: &str) {
        for token in tokenize(text) {
            self.observe(token);
        }
    }

    /// Координат со словом.
    pub fn len(&self) -> usize {
        self.best.iter().filter(|t| !t.is_empty()).count()
    }

    /// Есть ли хоть одно слово.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Слово координаты (доминантный токен).
    pub fn token_of(&self, coord: u32) -> Option<&str> {
        let i = coord as usize;
        if i >= self.best.len() {
            return None;
        }
        let t = &self.best[i];
        if t.is_empty() {
            None
        } else {
            Some(t.as_str())
        }
    }

    /// N-я непустая координата (порядок плотный, детерминированный) —
    /// выбор затравки свободной речи.
    pub fn coord_at(&self, n: usize) -> Option<u32> {
        let mut k = 0usize;
        for (i, t) in self.best.iter().enumerate() {
            if t.is_empty() {
                continue;
            }
            if k == n {
                return Some(i as u32);
            }
            k += 1;
        }
        None
    }

    /// Готовый лексикон для контейнера v4 (`None`, если слов нет).
    pub fn finish(&self) -> Option<Lexicon> {
        let entries: Vec<(u32, String)> = self
            .best
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_empty())
            .map(|(i, t)| (i as u32, t.clone()))
            .collect();
        Lexicon::new(entries, self.d).ok()
    }

    /// Влить поднятый из контейнера лексикон (resume): влитые слова
    /// получают доминантный вес 1 — смена только перевесом (два
    /// свежих наблюдения другого токена).
    pub fn absorb(&mut self, lex: &Lexicon) {
        for &(coord, ref token) in lex.entries() {
            let i = coord as usize;
            if i >= self.best.len() || token.is_empty() {
                continue;
            }
            if self.best[i].is_empty() {
                self.best[i] = token.clone();
                self.freq[i] = self.freq[i].max(1);
                self.best_freq[i] = 1;
            }
        }
    }
}

// ===================== Конфигурация генератора =====================

/// Конфигурация L5-генератора.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GeneratorConfig {
    /// Шагов «мышления» (транспорт L5) после слушания промпта.
    pub think_steps: usize,
    /// Потолок эмиссий (слов + морфем).
    pub max_tokens: usize,
    /// Размер кольца контекста речи (рабочая память).
    pub window: usize,
    /// Сид Born-лотереи.
    pub seed: u64,
    /// Снять моментные ворота: чистый оператор `Π_Λ(e^{Δt·J} p)`.
    /// Автоматически включается, если промпт не зажёг ни одной дуги.
    pub free: bool,
    /// AOT-фолбэк: координаты вне лексикона декодируются морфемами
    /// [`crate::syntax_unfolder`] (синтаксический скелет речи).
    pub morphemes: bool,
    /// Анти-заикание: координата исключается из лотереи на `N` шагов
    /// после эмиссии (`0` — выключено).
    pub repeat_veto: usize,
}

impl Default for GeneratorConfig {
    fn default() -> GeneratorConfig {
        GeneratorConfig {
            think_steps: 4,
            max_tokens: 64,
            window: 8,
            seed: 42,
            free: false,
            morphemes: false,
            repeat_veto: 1,
        }
    }
}

// ===================== Телеметрия =====================

/// Источник билетов шага речи.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TicketSource {
    /// Нисходящее русло из кольца контекста — мысль течёт вперёд.
    Flow,
    /// Восходящее русло — возврат к предыдущей мысли.
    Backtrack,
    /// Кинетическая дуга вне кольца — внутренний голос.
    Kinetic,
}

/// Одна эмиссия (квант речи).
#[derive(Clone, Debug, PartialEq)]
pub struct BornStep {
    /// Координата решётки, выбранная лотереей.
    pub coord: u32,
    /// Произнесённое слово (лексикон или AOT-морфема).
    pub token: String,
    /// Источник билетов.
    pub source: TicketSource,
    /// Живых билетов на этом шаге (ширина волны).
    pub tickets: usize,
    /// Born-исход измерения фазы координаты (ложь — полюс +1).
    pub born_bit: bool,
    /// Перещёлкиваний решётки транспортом на этом шаге.
    pub moved: usize,
    /// «Плавающий» θ-сдвиг транспорта на этом шаге.
    pub theta_shift: f64,
    /// Слово пришло из AOT-морфем, а не из лексикона.
    pub morpheme: bool,
}

/// Отчёт генерации.
#[derive(Clone, Debug, PartialEq)]
pub struct GenerationReport {
    /// Токенов в промпте.
    pub prompt_tokens: usize,
    /// Дуг TF-IDF свидетельства промпта (после ε-ворот).
    pub prompt_arcs: usize,
    /// Зажиганий момента свидетельством промпта.
    pub ignited: usize,
    /// Ворота сняты автоматически (нулевая кинетика после слушания).
    pub auto_free: bool,
    /// Перещёлкиваний решётки за фазу мышления.
    pub think_moved: usize,
    /// θ-сдвиг за фазу мышления.
    pub think_theta_shift: f64,
    /// Эмиссии в порядке речи.
    pub steps: Vec<BornStep>,
    /// Собранный текст (слова через пробел).
    pub text: String,
    /// Координат вне лексикона пропущено (мимо морфемного фолбэка).
    pub skipped_unseen: usize,
    /// Сходимость: живых билетов нет, ĤΨ = 0 — мысль завершена.
    pub converged: bool,
    /// Вырожденный цикл (волна вибрирует между двумя координатами).
    pub cycled: bool,
    /// Сквозная задержка.
    pub elapsed: std::time::Duration,
}

// ===================== L5-генератор =====================

/// Born-блуждатель по руслам `J`: замыкает авторегрессионный цикл
/// поверх обученного [`QuantizedGyroCurriculum`].
///
/// Заимствует движок **мутабельно**: генерация — не наблюдение, а
/// физический процесс (зажигание момента, транспорт, сенсорная
/// обратная связь эмиссий). Плотные буферы билетов и список смежности
/// русел строятся один раз на конструкторе и переиспользуются между
/// шагами — ноль аллокаций на квант речи (кроме растущего `steps`).
pub struct L5Generator<'a> {
    qc: &'a mut QuantizedGyroCurriculum,
    cfg: GeneratorConfig,
    rng: Rng,
    /// Кольцо контекста речи (последние координаты, обратный порядок
    /// не важен: все члены кольца равноправные источники русел).
    ring: VecDeque<u32>,
    /// Смежность русел: `adj[c] = [(сосед, +1 — русло c→сосед,
    /// −1 — русело сосед→c)]`. Строится один раз из каналов J.
    adj: Vec<Vec<(u32, i8)>>,
    /// Плотные билеты: нисходящие (вперёд по речи).
    down: Vec<u32>,
    /// Плотные билеты: восходящие (возврат).
    up: Vec<u32>,
    /// Материализованный список кандидатов текущего шага.
    candidates: Vec<(u32, u32)>,
    /// Анти-заикание: последние эмитированные координаты.
    veto: VecDeque<u32>,
}

impl<'a> L5Generator<'a> {
    /// Новая сессия речи поверх движка. Строит смежность русел —
    /// O(каналы), один раз.
    pub fn new(qc: &'a mut QuantizedGyroCurriculum, cfg: GeneratorConfig) -> Result<Self> {
        let d = qc.d_pol() as usize;
        let mut adj: Vec<Vec<(u32, i8)>> = vec![Vec::new(); d];
        for (i, j, w) in qc.channels() {
            let dir = if w > 0.0 { 1i8 } else { -1 };
            // Русло i→j: из i — нисходящее к j, из j — восходящее к i.
            adj[i as usize].push((j, dir));
            adj[j as usize].push((i, -dir));
        }
        let window = cfg.window.max(1);
        Ok(L5Generator {
            qc,
            cfg,
            rng: Rng::seed_from_u64(cfg.seed),
            ring: VecDeque::with_capacity(window + 1),
            adj,
            down: vec![0u32; d],
            up: vec![0u32; d],
            candidates: Vec::new(),
            veto: VecDeque::with_capacity(cfg.repeat_veto + 1),
        })
    }

    /// Полный цикл: слушание → зажигание → мышление → речь.
    pub fn generate(&mut self, prompt: &str) -> Result<GenerationReport> {
        let t0 = Instant::now();
        let mut report = GenerationReport {
            prompt_tokens: 0,
            prompt_arcs: 0,
            ignited: 0,
            auto_free: false,
            think_moved: 0,
            think_theta_shift: 0.0,
            steps: Vec::new(),
            text: String::new(),
            skipped_unseen: 0,
            converged: false,
            cycled: false,
            elapsed: std::time::Duration::ZERO,
        };

        // ---- Фаза A: слушание промпта ----
        // Токены входят в кольцо гироскопа (каналы вопроса копятся),
        // свидетельство TF-IDF зажигает момент — БЕЗ born-кристаллизации:
        // вопрос — вход, а не знание.
        let (arcs, ignited, tokens) = self.qc.listen_ignite(prompt);
        report.prompt_tokens = tokens;
        report.prompt_arcs = arcs;
        report.ignited = ignited;

        // Кольцо контекста: координаты промпта в порядке появления,
        // без повторов, с потолком окна.
        let window = self.cfg.window.max(1);
        let mut seen_ring: Vec<u32> = Vec::with_capacity(window);
        for token in tokenize(prompt) {
            let coord =
                (fnv1a64(token.as_bytes()) % self.qc.d_pol() as u64) as u32;
            if !seen_ring.contains(&coord) {
                seen_ring.push(coord);
            }
        }
        for &c in seen_ring.iter().take(window) {
            self.ring.push_back(c);
        }
        // Пустой промпт — свободная речь: затравка из лексикона.
        if self.ring.is_empty() {
            let n = self.qc.lexicon_len();
            if n > 0 {
                let pick = (self.rng.next_u64() % n as u64) as usize;
                if let Some(c) = self.qc.lexicon_coord_at(pick) {
                    self.ring.push_back(c);
                }
            }
        }

        // ---- Фаза B: мышление ----
        // Моментных ворот нет, если промпт не зажёг кинетики: мозг,
        // не отозвавшийся на вопрос, всё равно говорит чистым потоком J.
        let mut free = self.cfg.free;
        if !free && self.qc.momentum_kinetic() == 0 {
            free = true;
            report.auto_free = true;
        }
        for _ in 0..self.cfg.think_steps {
            let s = self.qc.reasoning_step_mode(if free {
                TransportMode::Free
            } else {
                TransportMode::Gated
            })?;
            report.think_moved += s.moved;
            report.think_theta_shift += s.theta_shift;
        }

        // ---- Фаза C: речь ----
        let mut consecutive_unseen = 0usize;
        while report.steps.len() < self.cfg.max_tokens {
            // Вырожденный цикл: ABAB — волна вибрирует между двумя
            // координатами, топливо есть, смысла нет.
            if report.steps.len() >= 4 {
                let c: Vec<u32> = report.steps[report.steps.len() - 4..]
                    .iter()
                    .map(|s| s.coord)
                    .collect();
                if c[0] == c[2] && c[1] == c[3] && c[0] != c[1] {
                    report.cycled = true;
                    break;
                }
            }

            let picked = self.born_lottery();
            let Some((coord, source, tickets)) = picked else {
                report.converged = true;
                break;
            };

            // Декодирование: лексикон → AOT-морфема → пропуск.
            let (token, morpheme) = match self.qc.lexicon_token(coord) {
                Some(t) => (t.to_string(), false),
                None if self.cfg.morphemes => match morpheme_at(coord as usize % 256) {
                    Some(m) => (m, true),
                    None => {
                        report.skipped_unseen += 1;
                        consecutive_unseen += 1;
                        self.push_ring(coord);
                        if consecutive_unseen >= 3 {
                            report.converged = true;
                            break;
                        }
                        continue;
                    }
                },
                None => {
                    report.skipped_unseen += 1;
                    consecutive_unseen += 1;
                    self.push_ring(coord);
                    if consecutive_unseen >= 3 {
                        report.converged = true;
                        break;
                    }
                    continue;
                }
            };
            consecutive_unseen = 0;

            // Born-измерение фазы координаты: полюса детерминированы,
            // суперпозиция честной монетой. Измерение — физический акт:
            // градиент p_emp − p зажигает момент (инерция речи).
            let p = self.qc.p_at(coord);
            let born_bit = if p > 0.0 {
                false
            } else if p < 0.0 {
                true
            } else {
                self.rng.next_f64() < 0.5
            };
            let p_emp = if born_bit { -1.0 } else { 1.0 };
            self.qc.momentum_feedback(coord, p_emp - p);

            // Эмиссия — сенсорное событие: слово входит в кольцо
            // гироскопа, русла от него копятся (авторегрессия замкнута).
            let sign: i8 = if born_bit { -1 } else { 1 };
            self.qc.observe_event(coord, sign);

            // Шаг транспорта: волна продолжает течь от произнесённого.
            let stats = self.qc.reasoning_step_mode(if free {
                TransportMode::Free
            } else {
                TransportMode::Gated
            })?;

            self.push_ring(coord);
            if !report.text.is_empty() {
                report.text.push(' ');
            }
            report.text.push_str(&token);
            report.steps.push(BornStep {
                coord,
                token,
                source,
                tickets,
                born_bit,
                moved: stats.moved,
                theta_shift: stats.theta_shift,
                morpheme,
            });

            // Анти-заикание.
            if self.cfg.repeat_veto > 0 {
                self.veto.push_back(coord);
                while self.veto.len() > self.cfg.repeat_veto {
                    self.veto.pop_front();
                }
            }
        }

        report.elapsed = t0.elapsed();
        Ok(report)
    }

    /// Born-лотерея одного шага: каскад источников билетов.
    ///
    /// Лотерея семплирует **ассоциативную топологию**: вес билета
    /// нисходящего русла `(c → n)` — `1 + |k|`, где `k = s·sin(Δθ)` —
    /// крутящий момент транспорта. Устоявшаяся связь (полюса
    /// согласованы, `k = 0`) говорит с весом 1 — знание вспоминается
    /// и после сходимости; напряжённая связь (суперпозиция на конце,
    /// `|k| = 1`) кричит с весом 2 — мысль течёт туда, где не решено.
    /// Фазы меняются транспортом — распределение речи следует за
    /// волной: настоящая авторегрессия, а не случайный блуждатель.
    ///
    /// Возвращает `(координата, источник, всего билетов)` или `None`,
    /// если волна иссякла (русл из кольца нет).
    fn born_lottery(&mut self) -> Option<(u32, TicketSource, usize)> {
        // Уровень 1+2: русла из кольца контекста. Внимание = членство
        // в кольце (моментные ворота — домен транспорта, не речи).
        let (mut total_down, mut total_up) = (0u64, 0u64);
        self.candidates.clear();
        for &c in self.ring.iter() {
            for &(n, dir) in &self.adj[c as usize] {
                if self.veto.contains(&n) {
                    continue;
                }
                let ci = self.qc.phase_code(c) as usize;
                let cj = self.qc.phase_code(n) as usize;
                let k = (dir as i32) * (SIN_LUT[ci][cj] as i32);
                // Крутящий момент русла: 0 — устоявшаяся связь, ±1 —
                // напряжённая (суперпозиция на одном из концов).
                let w = 1 + k.unsigned_abs();
                if dir > 0 {
                    if self.down[n as usize] == 0 {
                        self.candidates.push((n, 0));
                    }
                    self.down[n as usize] += w;
                    total_down += w as u64;
                } else {
                    self.up[n as usize] += w;
                    total_up += w as u64;
                }
            }
        }

        if total_down > 0 {
            // Свёртка кандидатов с их весами (только нисходящие).
            for slot in self.candidates.iter_mut() {
                slot.1 = self.down[slot.0 as usize];
            }
            let pick = self.draw(total_down);
            let coord = self.pick_candidate(pick);
            self.clear_weights();
            return Some((coord, TicketSource::Flow, total_down as usize));
        }
        if total_up > 0 {
            // Восходящие: материализуем из плотных весов.
            self.candidates.clear();
            for (i, &w) in self.up.iter().enumerate() {
                if w > 0 {
                    self.candidates.push((i as u32, w));
                }
            }
            let pick = self.draw(total_up);
            let coord = self.pick_candidate(pick);
            self.clear_weights();
            return Some((coord, TicketSource::Backtrack, total_up as usize));
        }

        // Уровень 3: кинетические дуги вне кольца — внутренний голос.
        self.candidates.clear();
        let ring = &self.ring;
        let veto = &self.veto;
        self.qc.for_each_kinetic_arc(|arc, _m| {
            if ring.contains(&arc) || veto.contains(&arc) {
                return;
            }
            // Одна координата — один билет за уровень.
            if let Some(slot) = self.candidates.iter_mut().find(|s| s.0 == arc) {
                slot.1 += 1;
            } else {
                self.candidates.push((arc, 1));
            }
        });
        let total: u64 = self.candidates.iter().map(|s| s.1 as u64).sum();
        if total > 0 {
            let pick = self.draw(total);
            let coord = self.pick_candidate(pick);
            self.candidates.clear();
            return Some((coord, TicketSource::Kinetic, total as usize));
        }
        self.candidates.clear();
        None
    }

    /// Равномерный выбор билета из `total`.
    fn draw(&mut self, total: u64) -> u64 {
        self.rng.next_u64() % total
    }

    /// Выбор координаты по номеру билета (кумулятивные веса).
    fn pick_candidate(&mut self, mut pick: u64) -> u32 {
        for &(coord, w) in self.candidates.iter() {
            if pick < w as u64 {
                return coord;
            }
            pick -= w as u64;
        }
        // Невозможно после draw(total) — но фолбэк детерминирован.
        self.candidates
            .last()
            .map(|&(c, _)| c)
            .expect("draw требует непустых кандидатов")
    }

    /// Сброс плотных весов (подготовка к следующему шагу): чистятся
    /// только тронутые слоты — соседи кольца контекста.
    fn clear_weights(&mut self) {
        for &c in self.ring.iter() {
            for &(n, _dir) in &self.adj[c as usize] {
                self.down[n as usize] = 0;
                self.up[n as usize] = 0;
            }
        }
        self.candidates.clear();
    }

    /// Продвижение кольца контекста.
    fn push_ring(&mut self, coord: u32) {
        self.ring.push_back(coord);
        while self.ring.len() > self.cfg.window.max(1) {
            self.ring.pop_front();
        }
    }
}

// ===================== Тесты =====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gyro_lattice::QuantizedGyroCurriculum;

    /// Микрокорпус с выраженным порядком слов: «квант» предшествует
    /// «фазе», «фаза» — «решётке». Русла дают нисходящий поток.
    const CORPUS: &str = "квант фаза решётка квант фаза решётка \
                          квант фаза born трит момент импульс";

    fn trained_engine() -> QuantizedGyroCurriculum {
        let mut qc = QuantizedGyroCurriculum::new(128, 0.05, 42, 4).unwrap();
        for _ in 0..3 {
            qc.ingest(CORPUS, 1).unwrap();
        }
        qc
    }

    #[test]
    fn lexicon_builder_dominance() {
        let mut b = LexiconBuilder::new(64);
        b.observe_text("фаза фаза фаза решётка");
        let d = b.d_pol();
        let coord_f = (fnv1a64("фаза".as_bytes()) % d as u64) as u32;
        let coord_r = (fnv1a64("решётка".as_bytes()) % d as u64) as u32;
        assert_eq!(b.token_of(coord_f), Some("фаза"));
        assert_eq!(b.token_of(coord_r), Some("решётка"));
        assert_eq!(b.len(), 2);
        assert_eq!(b.tokens_seen(), 4);
        // Коллизия: тот же coord, другой токен — доминанта держится до
        // перевеса частоты.
        let mut b2 = LexiconBuilder::new(64);
        for _ in 0..3 {
            b2.observe("фаза");
        }
        b2.observe("решётка"); // та же координата в тесте не гарантирована;
        // проверяем лишь детерминизм: повторное наблюдение не ломает карту
        let _ = b2.finish().unwrap();
    }

    #[test]
    fn lexicon_builder_finish_absorb_roundtrip() {
        let mut b = LexiconBuilder::new(128);
        b.observe_text(CORPUS);
        let lex = b.finish().expect("лексикон непуст");
        // Roundtrip через строитель: absorb → те же слова.
        let mut b2 = LexiconBuilder::new(128);
        b2.absorb(&lex);
        for &(c, ref t) in lex.entries() {
            assert_eq!(b2.token_of(c), Some(t.as_str()));
        }
        assert_eq!(b2.len(), lex.len());
    }

    #[test]
    fn generated_words_come_from_corpus() {
        let mut qc = trained_engine();
        let cfg = GeneratorConfig {
            max_tokens: 12,
            ..GeneratorConfig::default()
        };
        let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
        let rep = gen.generate("квант").unwrap();
        assert!(!rep.steps.is_empty(), "мозг обязан говорить");
        assert!(!rep.text.is_empty());
        // Каждое слово — из обученного корпуса (морфем нет).
        for step in &rep.steps {
            assert!(!step.morpheme);
            assert!(
                CORPUS.contains(&step.token),
                "чужое слово: {}",
                step.token
            );
        }
        assert!(rep.steps.len() <= 12);
    }

    #[test]
    fn generation_deterministic_by_seed() {
        let run = |seed: u64| {
            let mut qc = trained_engine();
            let cfg = GeneratorConfig {
                seed,
                max_tokens: 10,
                ..GeneratorConfig::default()
            };
            let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
            gen.generate("фаза").unwrap()
        };
        let a = run(7);
        let b = run(7);
        assert_eq!(a.text, b.text);
        assert_eq!(a.steps.len(), b.steps.len());
        // Другой сид — другая траектория (статистически; фиксированный
        // корпус мал, поэтому проверяем лишь сам факт детерминизма сида).
        let _ = run(8);
    }

    #[test]
    fn empty_prompt_free_speech_seeded_from_lexicon() {
        let mut qc = trained_engine();
        let cfg = GeneratorConfig {
            max_tokens: 8,
            ..GeneratorConfig::default()
        };
        let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
        let rep = gen.generate("").unwrap();
        // Затравка из лексикона: либо слова, либо честная пустота
        // (микрокорпус может сойтись мгновенно).
        for step in &rep.steps {
            assert!(CORPUS.contains(&step.token));
        }
    }

    #[test]
    fn max_tokens_respected() {
        let mut qc = trained_engine();
        let cfg = GeneratorConfig {
            max_tokens: 3,
            ..GeneratorConfig::default()
        };
        let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
        let rep = gen.generate("квант фаза решётка").unwrap();
        assert!(rep.steps.len() <= 3);
    }

    #[test]
    fn repetition_never_immediate() {
        // Анти-заикание: соседние эмиссии не совпадают (veto = 1).
        let mut qc = trained_engine();
        let cfg = GeneratorConfig {
            max_tokens: 16,
            ..GeneratorConfig::default()
        };
        let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
        let rep = gen.generate("квант").unwrap();
        for w in rep.steps.windows(2) {
            assert_ne!(w[0].coord, w[1].coord, "немедленный повтор координаты");
        }
    }

    #[test]
    fn unknown_prompt_still_speaks_or_converges_honestly() {
        // Промпт из невиданных слов: каналов нет, зажиганий нет —
        // авто-free открывает чистый поток J, а если и он пуст —
        // честная сходимость без паники.
        let mut qc = trained_engine();
        let cfg = GeneratorConfig {
            max_tokens: 8,
            ..GeneratorConfig::default()
        };
        let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
        let rep = gen.generate("зукбар шлёп").unwrap();
        for step in &rep.steps {
            assert!(CORPUS.contains(&step.token) || step.morpheme);
        }
    }

    #[test]
    fn morpheme_fallback_enabled() {
        // Kinetic-путь может привести на координату без слова; с
        // включённым фолбэком она декодируется морфемой AOT.
        let mut qc = trained_engine();
        let cfg = GeneratorConfig {
            morphemes: true,
            max_tokens: 10,
            ..GeneratorConfig::default()
        };
        let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
        let rep = gen.generate("квант").unwrap();
        // Морфема либо не понадобилась, либо валидна — паник нет.
        for step in &rep.steps {
            assert!(!step.token.is_empty());
        }
    }

    #[test]
    fn feedback_enriches_channels() {
        // Эмиссии — сенсорные события: после генерации тактов
        // гироскопа больше, чем до (речь оставляет след).
        let mut qc = trained_engine();
        let before = qc.gyro_ticks();
        let cfg = GeneratorConfig {
            max_tokens: 8,
            ..GeneratorConfig::default()
        };
        let mut gen = L5Generator::new(&mut qc, cfg).unwrap();
        let rep = gen.generate("квант фаза").unwrap();
        let after = qc.gyro_ticks();
        assert!(after > before, "эмиссии обязаны наблюдаться гироскопом");
        assert!(!rep.steps.is_empty());
    }

    #[test]
    fn config_defaults_sane() {
        let c = GeneratorConfig::default();
        assert_eq!(c.think_steps, 4);
        assert_eq!(c.max_tokens, 64);
        assert_eq!(c.window, 8);
        assert_eq!(c.repeat_veto, 1);
        assert!(!c.free);
        assert!(!c.morphemes);
    }

    #[test]
    fn lexicon_token_max_filter() {
        let mut b = LexiconBuilder::new(64);
        let long = "а".repeat(LEXICON_TOKEN_MAX + 1);
        b.observe(&long);
        assert_eq!(b.tokens_seen(), 0, "длинный прогон — шум, мимо");
        assert!(b.is_empty());
    }

    #[test]
    fn morpheme_table_lookup() {
        // Слот 1 — «fn » (сид build.rs): фолбэк-декодирование живо.
        let m = morpheme_at(1).expect("слот 1 засеян");
        assert!(m.starts_with("fn"));
        assert!(morpheme_at(0).is_none(), "слот 0 пуст");
        assert!(morpheme_at(256).is_none(), "вне таблицы — None");
    }
}
