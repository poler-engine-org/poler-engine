//! # feed — контекст-шлюз под действие (манифест «Работа с текстом», п.11)
//!
//! «Не подавай весь текст. Подавай релевантный контекст по энергии:
//! для понимания — высокоэнергетичные узлы и их связи; для правки —
//! низкоэнергетичные участки и то, что на них влияет. Это даёт скорость
//! и уменьшает ошибки.»
//!
//! `feed` — последний слой удобства для ИИ: он НЕ ищет (это делает
//! `Engine::scan` с ε/R/сценами/K-hop), а **собирает пакет контекста
//! под конкретное действие** и укладывает его в токен-бюджет:
//!
//! * `--for understand` — СКЕЛЕТ: якоря, ранжированные по ε (высокая
//!   ε = плотное ядро темы), + K-hop связи. Карта «что это и на
//!   чём держится».
//! * `--for edit` — ПРАВОЧНЫЙ ПАКЕТ: якоря цели, ранжированные по ε
//!   ВОЗРАСТАНИЕМ (разреженные зоны — хирургические цели — первыми),
//!   с вердиктом «плотная зона / середина / разреженная зона» по
//!   перцентилю ε, + точка определения символа + AIDDE impact-паспорт
//!   (доказанный call graph: кто сломается, если править) + связи.
//!
//! Философия вердиктов (честность метрик): ε меряет ПЛОТНОСТЬ токенов
//! вокруг совпадения — не «важность» и не нагрузку на проект. Нагрузку
//! доказывает только символьный граф: центральная функция может быть
//! определена один раз (разреженная точка) и при этом держать
//! пол-проекта.
//!
//! Оценка токенов — письменность-осознанная (через
//! [`crate::universal_letters::Script`]): CJK/Хангыль/Кана ≈ 1 токен на
//! символ, кириллица ≈ 3 символа на токен, прочее ≈ 4 символа на токен.
//! Без словарей — чистая аналитика, в духе движка.
//!
//! Синтаксис (poler-shell / `--exec`):
//! ```text
//! feed <PATH> --for understand --query "<тема|сущность|символ>" [--budget N] [--relations N] [--json]
//! feed <PATH> --for edit     --query "<символ|фрагмент>"       [--budget N] [--relations N] [--json]
//! ```
//!
//! Пример для агента:
//! ```text
//! poler-engine --exec 'feed ./src --for edit --query cmd_search --budget 4000 --json'
//! ```

use std::collections::HashSet;
use std::path::PathBuf;

use serde_json::json;

use crate::universal_letters::Script;
use crate::{Engine, EngineConfig};

use super::state::ShellState;
use super::CmdResult;

/// Режим подачи: под какое действие собирается пакет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedMode {
    Understand,
    Edit,
}

impl FeedMode {
    fn as_str(self) -> &'static str {
        match self {
            FeedMode::Understand => "understand",
            FeedMode::Edit => "edit",
        }
    }
}

/// Пул якорей, который движок даёт на один запрос (шире top-N поиска:
/// feed сам решает, кого взять в бюджет).
const POOL_TOP_N: usize = 200;
/// Глубина K-hop для пакета (2 = соседи + соседи соседей).
const FEED_K_HOP: usize = 2;
/// Токен-стоимость метаданных одного якоря в пакете (файл/глава/ε/R/субъекты).
const ITEM_META_TOKENS: usize = 24;
/// Токен-стоимость конверта пакета (заголовок, режим, бюджет, статистика).
const ENVELOPE_TOKENS: usize = 48;
/// Надбавка на одну K-hop тройку сверх её текста.
const RELATION_EXTRA_TOKENS: usize = 4;
/// Дефолтный токен-бюджет пакета.
const DEFAULT_BUDGET: usize = 8000;
/// Дефолтный потолок K-hop троек в пакете.
const DEFAULT_RELATIONS: usize = 40;
/// Максимум upstream/downstream строк в человекочитаемой карте.
const IMPACT_DISPLAY_ROWS: usize = 12;

// ---------------------------------------------------------------------------
// Оценка токенов (письменность-осознанная, без словарей)
// ---------------------------------------------------------------------------

/// Оценка числа LLM-токенов в тексте по письменности символов.
///
/// CJK/Хангыль/Кана ≈ 1 токен на символ; кириллица ≈ 1 токен на 3 символа
/// (BPE режет кириллицу на ~2–3-символьные куски); прочее (латиница,
/// цифры, пунктуация) ≈ 1 токен на 4 символа. Пробелы не считаются —
/// BPE обычно склеивает их со следующим куском.
pub fn estimate_tokens(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut cyr = 0usize;
    let mut other = 0usize;
    for c in text.chars() {
        if c.is_whitespace() {
            continue;
        }
        match Script::from_char(c) {
            Script::Cjk | Script::Hangul | Script::Hiragana | Script::Katakana => cjk += 1,
            Script::Cyrillic => cyr += 1,
            _ => other += 1,
        }
    }
    cjk + (cyr + 2) / 3 + (other + 3) / 4
}

// ---------------------------------------------------------------------------
// Статистика энергии пула
// ---------------------------------------------------------------------------

/// Перцентиль значения в выборке (линейная интерполяция, детерминированно).
fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let idx = p * (sorted.len() - 1) as f64;
    let lo = idx.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let frac = idx - lo as f64;
    sorted[lo] * (1.0 - frac) + sorted[hi] * frac
}

/// Вердикт ПЛОТНОСТИ якоря по его перцентилю ε в пуле.
///
/// Важно (философия движка): ε измеряет плотность токенов вокруг
/// совпадения — НЕ «важность» и НЕ нагрузку на проект. Единственное
/// упоминание центрального символа — разреженная точка (игла в стоге
/// сена), а его нагрузка определяется ТОЛЬКО графом вызовов (AIDDE
/// impact), не энергией. Поэтому вердикты здесь — про плотность
/// контекста: плотная зона = сложное окружение (много редких токенов
/// рядом), разреженная = хирургическая цель для точечной правки.
fn energy_verdict(p: f64) -> &'static str {
    if p >= 0.75 {
        "плотная зона"
    } else if p <= 0.25 {
        "разреженная зона"
    } else {
        "середина"
    }
}

// ---------------------------------------------------------------------------
// Ядро: сборка пакета
// ---------------------------------------------------------------------------

/// Один элемент пакета (якорь + вердикт + стоимость).
struct FeedItem<'a> {
    anchor: &'a crate::ContextAnchor,
    tokens: usize,
    /// Только для edit-режима: вердикт по перцентилю энергии.
    verdict: Option<&'static str>,
}

/// Внутренний прогон движка: пул якорей под запрос.
fn scan_pool(target: &PathBuf, query: &str) -> Option<crate::SearchResult> {
    let config = EngineConfig {
        top_n: POOL_TOP_N,
        k_hop_depth: FEED_K_HOP,
        ..EngineConfig::default()
    };
    let mut engine = Engine::new(config, false);
    let (res, _stats) = engine.scan(target, query);
    if res.anchors.is_empty() {
        return None;
    }
    Some(res)
}

/// AIDDE impact-паспорт символа по кодовым файлам пути (если они есть).
/// Второй элемент — сколько точек определения нашёл символьный граф
/// (0 = extern/макро: определения нет, паспорт собран по вызовам).
fn impact_for(path: &PathBuf, symbol: &str) -> Option<(crate::aidde::ImpactReport, usize)> {
    let config = EngineConfig::default();
    let files: Vec<PathBuf> = crate::collect_files(path, &config)
        .into_iter()
        .filter(|p| crate::detect_lang(p) != crate::CodeLang::Plain)
        .collect();
    if files.is_empty() {
        return None;
    }
    let table = crate::aidde::SymbolTable::build(&files, config.max_file_bytes);
    let def_count = table.resolve(symbol).len();
    crate::aidde::impact_analysis(&table, symbol, 3, 200).map(|r| (r, def_count))
}

/// Честная формулировка точки определения: сколько мест объявляет символ.
/// Точное число — в числовом поле `definition_sites`; текст грамматически
/// устойчив для любого количества.
fn definition_note(def_count: usize) -> String {
    match def_count {
        0 => "внешний/макро-символ: точки определения нет — паспорт по вызовам".into(),
        1 => "единственная точка определения (символьный граф)".into(),
        _ => "несколько точек определения — показана первая (символьный граф)".into(),
    }
}

/// Связи K-hop: движок кладёт их в каждый якорь (одни и те же для корня
/// запроса) — берём из первого и обрезаем до потолка.
fn relations_of(res: &crate::SearchResult, limit: usize) -> Vec<(String, String, String)> {
    res.anchors
        .first()
        .map(|a| a.k_hop_relations.iter().take(limit).cloned().collect())
        .unwrap_or_default()
}

fn relations_tokens(rels: &[(String, String, String)]) -> usize {
    rels.iter()
        .map(|(s, p, o)| estimate_tokens(&format!("{s} {p} {o}")) + RELATION_EXTRA_TOKENS)
        .sum()
}

fn round2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// Сборка и исполнение feed-пакета. Возвращает текст карты (человек/LLM)
/// или JSON-пакет при `json = true`.
pub fn run_feed(
    path: &PathBuf,
    mode: FeedMode,
    query: &str,
    budget: usize,
    relations_limit: usize,
    json: bool,
) -> String {
    let Some(res) = scan_pool(path, query) else {
        return format!(
            "feed: 0 якорей по «{query}» в {}\n\
             Подсказки:\n  - проверь путь/запрос (кавычки для фраз)\n  \
             - скрытые/сюжетные связи: poler-engine {} -q \"{query}\" --resonance-mode psi\n  \
             - точный подсчёт: --grep \"{query}\" {}",
            path.display(),
            path.display(),
            path.display()
        );
    };

    let mut pool: Vec<crate::ContextAnchor> = res.anchors.clone();
    // Дедупликация сцен: разные хиты внутри одной сцены дают одинаковый
    // materialized scope — в пакет достаточно одного представителя
    // (в understand — с наибольшей ε, т.к. сортировка уже по убыванию).
    let mut seen_scenes: HashSet<String> = HashSet::new();
    pool.retain(|a| {
        let key = format!(
            "{}\x00{}",
            a.file,
            a.scene.enclosing_scope.chars().take(120).collect::<String>()
        );
        seen_scenes.insert(key)
    });
    // Детерминированный полный tiebreaker: ε → R → файл → глава.
    pool.sort_by(|a, b| {
        b.epsilon
            .partial_cmp(&a.epsilon)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                b.resonance
                    .partial_cmp(&a.resonance)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.scene.chapter.cmp(&b.scene.chapter))
    });
    let eps_sorted: Vec<f64> = {
        let mut v: Vec<f64> = pool.iter().map(|a| a.epsilon).collect();
        v.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
        v
    };
    let n = pool.len();
    let eps_median = percentile(&eps_sorted, 0.5);
    let eps_q3 = percentile(&eps_sorted, 0.75);
    let eps_max = eps_sorted[n - 1];

    // -------- edit-режим: разреженные зоны первыми (ε по возрастанию) --------
    let (items_order, verdicts): (Vec<usize>, Option<Vec<&'static str>>) = match mode {
        FeedMode::Understand => ((0..n).collect(), None),
        FeedMode::Edit => {
            let mut idx: Vec<usize> = (0..n).collect();
            idx.sort_by(|&i, &j| {
                pool[i]
                    .epsilon
                    .partial_cmp(&pool[j].epsilon)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| pool[i].file.cmp(&pool[j].file))
            });
            // Перцентиль каждого якоря в пуле: rank = числу значений ε
            // строго меньших (partition_point — стабильная нижняя граница;
            // равные энергии получают одинаковый ранг).
            let all_equal = eps_max == eps_sorted[0];
            let v: Vec<&'static str> = idx
                .iter()
                .map(|&i| {
                    let p = if n == 1 || all_equal {
                        0.5
                    } else {
                        let rank = eps_sorted.partition_point(|x| x < &pool[i].epsilon);
                        rank as f64 / (n - 1) as f64
                    };
                    energy_verdict(p)
                })
                .collect();
            (idx, Some(v))
        }
    };

    // -------- связи и impact: резервируются до заполнения --------
    let rels = relations_of(&res, relations_limit);
    let rel_cost = relations_tokens(&rels);

    let impact_pack = if mode == FeedMode::Edit {
        impact_for(path, query.trim())
    } else {
        None
    };
    let impact = impact_pack.as_ref().map(|(r, _)| r);
    let def_count = impact_pack.as_ref().map(|(_, c)| *c).unwrap_or(0);
    let impact_cost = impact
        .map(|r| {
            // 34 = конверт impact-паспорта + точка определения
            let mut t = 34usize;
            for d in r.structural_relations.upstream_dependents.iter() {
                t += estimate_tokens(&format!("{} {}", d.caller, d.file)) + 4;
            }
            for d in r.structural_relations.downstream_dependencies.iter() {
                t += estimate_tokens(&format!("{} {}", d.callee, d.file)) + 4;
            }
            t
        })
        .unwrap_or(0);

    // -------- жадное заполнение бюджета --------
    let mut spent = ENVELOPE_TOKENS + rel_cost + impact_cost;
    let mut selected: Vec<FeedItem> = Vec::new();
    for (pos, &i) in items_order.iter().enumerate() {
        let cost = estimate_tokens(&pool[i].scene.enclosing_scope) + ITEM_META_TOKENS;
        if !selected.is_empty() && spent + cost > budget {
            continue; // не влезает — пробуем следующие (они могут быть короче)
        }
        spent += cost;
        selected.push(FeedItem {
            anchor: &pool[i],
            tokens: cost,
            verdict: verdicts.as_ref().map(|v| v[pos]),
        });
    }

    let coverage = if res.total_hits > 0 {
        100.0 * selected.len().min(res.total_hits) as f64 / res.total_hits as f64
    } else {
        0.0
    };
    // Честная пометка: первый якорь подаётся полностью даже сквозь бюджет
    // (в edit-режиме резать цель правки нельзя) — если он один его переполняет.
    let over_budget = spent > budget;

    if json {
        let items_json: Vec<serde_json::Value> = selected
            .iter()
            .map(|it| {
                let a = it.anchor;
                let mut v = json!({
                    "file": a.file,
                    "chapter": a.scene.chapter,
                    "epsilon": round2(a.epsilon),
                    "resonance": round2(a.resonance),
                    "subjects": a.scene.subjects,
                    "tokens": it.tokens,
                    "scope": a.scene.enclosing_scope,
                });
                if let Some(l) = &a.scene.location {
                    v["location"] = json!(l);
                }
                if let Some(m) = &a.scene.temporal_metric {
                    v["temporal_metric"] = json!(m);
                }
                if let Some(verdict) = it.verdict {
                    v["density_verdict"] = json!(verdict);
                }
                v
            })
            .collect();
        let rels_json: Vec<serde_json::Value> = rels
            .iter()
            .map(|(s, p, o)| json!([s, p, o]))
            .collect();
        let mut packet = json!({
            "feed": mode.as_str(),
            "query": query,
            "path": path.display().to_string(),
            "budget_tokens": budget,
            "spent_tokens": spent,
            "pool_anchors": n,
            "total_hits": res.total_hits,
            "selected": selected.len(),
            "coverage_pct": round2(coverage),
            "over_budget": over_budget,
            "energy": {
                "median": round2(eps_median),
                "q3": round2(eps_q3),
                "max": round2(eps_max),
            },
            "items": items_json,
            "relations": rels_json,
        });
        if let Some(r) = impact {
            // Точка определения — «игла в стоге сена»: символьный граф доказал,
            // ГДЕ и СКОЛЬКО раз символ объявлен. Это точка истины правки —
            // она НЕ выводится из ε (ε меряет плотность, не нагрузку).
            packet["definition"] = json!({
                "symbol": r.target_function,
                "file": r.file,
                "lines": r.lines,
                "definition_sites": def_count,
                "note": definition_note(def_count),
            });
            packet["impact"] = json!({
                "target": r.target_function,
                "file": r.file,
                "lines": r.lines,
                "danger_level_if_modified": r.danger_level_if_modified,
                "upstream_dependents": r.structural_relations.upstream_dependents
                    .iter()
                    .map(|d| json!({"caller": d.caller, "file": d.file}))
                    .collect::<Vec<_>>(),
                "downstream_dependencies": r.structural_relations.downstream_dependencies
                    .iter()
                    .map(|d| json!({"callee": d.callee, "file": d.file}))
                    .collect::<Vec<_>>(),
                "triage_alerts": r.heuristic_triage_alerts
                    .iter()
                    .map(|a| json!({"marker": a.marker, "description": a.description}))
                    .collect::<Vec<_>>(),
            });
        }
        packet["advice"] = json!(advice_text(
            mode,
            query,
            &selected,
            impact,
            def_count,
            n,
            eps_median
        ));
        serde_json::to_string_pretty(&packet).unwrap_or_else(|_| "{}".into())
    } else {
        render_card(
            mode,
            query,
            path,
            budget,
            spent,
            over_budget,
            n,
            res.total_hits,
            coverage,
            eps_median,
            eps_q3,
            eps_max,
            &selected,
            &rels,
            impact,
            def_count,
        )
    }
}

/// Текстовый совет: как агенту использовать пакет.
fn advice_text(
    mode: FeedMode,
    query: &str,
    selected: &[FeedItem],
    impact: Option<&crate::aidde::ImpactReport>,
    def_count: usize,
    pool: usize,
    eps_median: f64,
) -> String {
    match mode {
        FeedMode::Understand => {
            let top = selected
                .first()
                .map(|it| round2(it.anchor.epsilon))
                .unwrap_or(0.0);
            format!(
                "Подай модели этот пакет как ядро темы «{query}»: {} якорей из пула {} \
                 (медиана плотности пула ε={:.1}, топ ε={:.1}). Плотные узлы — \
                 тематическое ядро: держи их в рабочей памяти, остальное догружай \
                 точечными запросами.",
                selected.len(),
                pool,
                eps_median,
                top
            )
        }
        FeedMode::Edit => {
            let mut s = String::new();
            let verdicts: Vec<&str> = selected
                .iter()
                .filter_map(|it| it.verdict)
                .collect();
            let sparse = verdicts
                .iter()
                .filter(|v| **v == "разреженная зона")
                .count();
            let dense = verdicts.iter().filter(|v| **v == "плотная зона").count();
            // Точка истины — честно о числе определений: символьный граф ЗНАЕТ,
            // где символ объявлен; ε этого не знает (плотность ≠ нагрузка).
            match (impact, def_count) {
                (Some(r), 1) => s.push_str(&format!(
                    "Точка истины: «{query}» определён ровно один раз: {} строки {} \
                     (найдено символьным графом, не угадано). ",
                    r.file, r.lines
                )),
                (Some(r), n) if n > 1 => s.push_str(&format!(
                    "«{query}» объявлен в {n} местах (первая: {} строки {}): \
                     проверь, та ли точка попала в пакет. ",
                    r.file, r.lines
                )),
                (Some(_), _) => s.push_str(
                    "Точка определения не найдена (внешний/макро-символ): \
                     паспорт собран по вызовам. ",
                ),
                (None, _) => s.push_str(&format!(
                    "Символ «{query}» не найден в кодовом call graph (или корпус текстовый): \
                     опирайся на связи сущности и плотность зон. ",
                )),
            }
            if let Some(r) = impact {
                s.push_str(&format!(
                    "Правка заденет доказанный call graph: {} upstream-потребителей, \
                     {} downstream-зависимостей, danger: {}. ",
                    r.structural_relations.upstream_dependents.len(),
                    r.structural_relations.downstream_dependencies.len(),
                    r.danger_level_if_modified
                ));
            }
            // Нагрузка на проект — только от графа; плотность — только от ε.
            // Эти вещи НЕ синонимы: центральная функция может быть определена
            // один раз (разреженная точка) и одновременно держать пол-проекта.
            let graph_load = impact
                .map(|r| r.structural_relations.upstream_dependents.len())
                .unwrap_or(0);
            if graph_load >= 10 {
                s.push_str(&format!(
                    "Нагрузка на граф ВЫСОКАЯ ({} потребителей): меняй контракт \
                     только осознанно. ",
                    graph_load
                ));
            }
            s.push_str(&format!(
                "Сами правки делай в разреженных зонах (низкая ε — локальные, \
                 изолированные контексты): их {} из {}; плотных зон (сложное \
                 окружение) — {}. Низкая ε — не шум, а точечная цель.",
                sparse,
                verdicts.len(),
                dense
            ));
            s
        }
    }
}

/// Человекочитаемая карта пакета (LLM читает её одинаково хорошо).
#[allow(clippy::too_many_arguments)]
fn render_card(
    mode: FeedMode,
    query: &str,
    path: &PathBuf,
    budget: usize,
    spent: usize,
    over_budget: bool,
    pool: usize,
    total_hits: usize,
    coverage: f64,
    eps_median: f64,
    eps_q3: f64,
    eps_max: f64,
    selected: &[FeedItem],
    rels: &[(String, String, String)],
    impact: Option<&crate::aidde::ImpactReport>,
    def_count: usize,
) -> String {
    let mut out = String::new();
    let icon = if mode == FeedMode::Understand {
        "🧠"
    } else {
        "🛠"
    };
    out.push_str(&format!(
        "{icon} FEED · {} · «{query}»\n",
        mode.as_str()
    ));
    out.push_str(&format!(
        "Путь: {} | Пул: {} якорей (всего хитов: {}) | Выбрано: {} | Бюджет: {}/{} ток (охват ≈{:.1}%)\n",
        path.display(),
        pool,
        total_hits,
        selected.len(),
        spent,
        budget,
        coverage
    ));
    if over_budget {
        out.push_str(
            "⚠ Первый якорь превышает бюджет и подан полностью (цель правки не режется):\n\
             подними --budget либо уточни запрос для меньших скоупов.\n",
        );
    }
    out.push_str(&format!(
        "Энергия пула: медиана ε={:.1} · Q3 ε={:.1} · max ε={:.1}\n",
        eps_median, eps_q3, eps_max
    ));

    out.push('\n');
    match mode {
        FeedMode::Understand => {
            out.push_str("── СКЕЛЕТ (высокоэнергетичные узлы темы) ──\n");
            for (i, it) in selected.iter().enumerate() {
                let a = it.anchor;
                out.push_str(&format!(
                    "\n{}. ε={:.1} R={:.1} | {} | {}\n",
                    i + 1,
                    a.epsilon,
                    a.resonance,
                    a.file,
                    a.scene.chapter
                ));
                if !a.scene.subjects.is_empty() {
                    out.push_str(&format!(
                        "   субъекты: {}\n",
                        a.scene.subjects.join("; ")
                    ));
                }
                if let Some(l) = &a.scene.location {
                    out.push_str(&format!("   {l}\n"));
                }
                out.push_str(&format!(
                    "\n```text\n{}\n```\n",
                    a.scene.enclosing_scope
                ));
            }
        }
        FeedMode::Edit => {
            out.push_str("── ЦЕЛЬ ПРАВКИ (разреженные зоны первыми) ──\n");
            for (i, it) in selected.iter().enumerate() {
                let a = it.anchor;
                let verdict = it.verdict.unwrap_or("середина");
                out.push_str(&format!(
                    "\n{}. ε={:.1} [{verdict}] | {} | {}\n",
                    i + 1,
                    a.epsilon,
                    a.file,
                    a.scene.chapter
                ));
                if let Some(l) = &a.scene.location {
                    out.push_str(&format!("   {l}\n"));
                }
                out.push_str(&format!(
                    "\n```text\n{}\n```\n",
                    a.scene.enclosing_scope
                ));
            }
        }
    }

    if let Some(r) = impact {
        out.push_str("\n── ВЛИЯНИЕ (AIDDE, доказано call graph) ──\n");
        out.push_str(&format!(
            "target: {} | {} | строки {} | danger: {}\n",
            r.target_function, r.file, r.lines, r.danger_level_if_modified
        ));
        out.push_str(&format!(
            "точка определения: {}\n",
            match def_count {
                0 => "нет — внешний/макро-символ (паспорт по вызовам)".to_string(),
                1 => "единственная (символьный граф)".to_string(),
                n => format!("в {n} местах — показана первая (символьный граф)"),
            }
        ));
        out.push_str(&format!(
            "⬆ upstream ({}):\n",
            r.structural_relations.upstream_dependents.len()
        ));
        for d in r.structural_relations.upstream_dependents.iter().take(IMPACT_DISPLAY_ROWS) {
            out.push_str(&format!("   • {} ({})\n", d.caller, d.file));
        }
        out.push_str(&format!(
            "⬇ downstream ({}):\n",
            r.structural_relations.downstream_dependencies.len()
        ));
        for d in r
            .structural_relations
            .downstream_dependencies
            .iter()
            .take(IMPACT_DISPLAY_ROWS)
        {
            out.push_str(&format!("   • {} ({})\n", d.callee, d.file));
        }
    }

    if !rels.is_empty() {
        out.push_str("\n── СВЯЗИ (K-hop графа сущностей) ──\n");
        for (s, p, o) in rels {
            out.push_str(&format!("• {s} —{p}→ {o}\n"));
        }
    }

    out.push_str("\n── СОВЕТ ──\n");
    out.push_str(&advice_text(
        mode,
        query,
        selected,
        impact,
        def_count,
        pool,
        eps_median,
    ));
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Команда шелла
// ---------------------------------------------------------------------------

/// Справка по команде.
pub fn feed_usage() -> String {
    "feed <PATH> --for <understand|edit> --query \"<тема|символ>\" [опции]\n\
     Контекст-шлюз под действие (манифест п.11): пакет контекста по энергии,\n\
     уложенный в токен-бюджет — то, что подавать модели вместо всего текста.\n\n\
     Режимы:\n\
       --for understand  СКЕЛЕТ: плотное ядро темы по ε + K-hop связи\n\
                         (для понимания: «что это и на чём держится»)\n\
       --for edit        ПРАВОЧНЫЙ ПАКЕТ: цели по ε ВОЗРАСТАНИЮ (разреженные\n\
                         зоны первыми), вердикты «плотная/середина/разреженная\n\
                         зона», точка определения + AIDDE impact (кто\n\
                         сломается) + связи сущности\n\n\
     Опции:\n\
       --query, -q \"...\"   запрос: сущность/фраза/символ (обязателен)\n\
       --budget, -b N       токен-бюджет пакета (дефолт 8000)\n\
       --relations, -r N    потолок K-hop троек (дефолт 40)\n\
       --json               машиночитаемый пакет вместо карты\n\n\
     Примеры:\n\
       feed ./src --for edit --query cmd_search --budget 4000 --json\n\
       feed ./book --for understand --query \"Нокс\" --budget 6000\n\
       poler-engine --exec 'feed . --for understand -q \"архитектура\" --json'"
        .into()
}

/// Точка входа команды шелла: `feed <PATH> ...`.
pub fn cmd_feed(state: &mut ShellState, args: &[String]) -> CmdResult {
    let mut path: Option<PathBuf> = None;
    let mut mode = FeedMode::Understand;
    let mut query = String::new();
    let mut budget = DEFAULT_BUDGET;
    let mut relations = DEFAULT_RELATIONS;
    let mut json = false;

    let mut i = 0usize;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--for" | "--mode" | "-f" => {
                if let Some(v) = args.get(i + 1) {
                    match v.as_str() {
                        "understand" | "u" | "понять" => mode = FeedMode::Understand,
                        "edit" | "e" | "правка" => mode = FeedMode::Edit,
                        other => {
                            return CmdResult::Done(format!(
                                "feed: неизвестный режим {other} (understand | edit)"
                            ))
                        }
                    }
                    i += 2;
                    continue;
                }
            }
            "--query" | "-q" => {
                if let Some(v) = args.get(i + 1) {
                    query = v.clone();
                    i += 2;
                    continue;
                }
            }
            "--budget" | "-b" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<usize>() {
                        budget = n.max(64);
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("feed --budget: ожидается число токенов (≥64)".into());
            }
            "--relations" | "-r" => {
                if let Some(v) = args.get(i + 1) {
                    if let Ok(n) = v.parse::<usize>() {
                        relations = n;
                        i += 2;
                        continue;
                    }
                }
                return CmdResult::Done("feed --relations: ожидается число".into());
            }
            "--json" => {
                json = true;
                i += 1;
                continue;
            }
            "--help" | "-h" => return CmdResult::Done(feed_usage()),
            other if other.starts_with("--") => {
                return CmdResult::Done(format!("feed: неизвестный флаг {other}"));
            }
            other if path.is_none() && !other.is_empty() => {
                path = Some(PathBuf::from(other));
            }
            _ => {}
        }
        i += 1;
    }

    let Some(path) = path else {
        return CmdResult::Done(feed_usage());
    };
    if !path.exists() {
        return CmdResult::Done(format!("feed: путь не найден: {}", path.display()));
    }
    if query.trim().is_empty() {
        return CmdResult::Done(
            "feed: укажите --query \"<тема|символ>\" (пример: feed ./src --for edit -q cmd_search)"
                .into(),
        );
    }

    let out = run_feed(&path, mode, &query, budget, relations, json);
    state.set_output(out.clone());
    CmdResult::Done(out)
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Временный каталог с уникальным именем (best-effort очистка вручную).
    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("poler-feed-test-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    #[test]
    fn estimate_tokens_scripts() {
        // CJK: 1 токен на символ.
        assert_eq!(estimate_tokens("你好世界"), 4);
        // Кириллица: 3 символа на токен (5 символов → 2).
        assert_eq!(estimate_tokens("этапа"), 2);
        // Латиница: 4 символа на токен (7 символов → 2).
        assert_eq!(estimate_tokens("network"), 2);
        // Пробелы не считаются.
        assert_eq!(estimate_tokens("   "), 0);
        // Смешанный текст: 4 CJK + 7 латиницы = 4 + 2 = 6.
        assert_eq!(estimate_tokens("dragons 你好世界"), 6);
    }

    #[test]
    fn feed_understand_respects_budget_and_energy_order() {
        let d = temp_dir("und");
        std::fs::write(
            d.join("a.md"),
            "# Глава 1\n\nНокс вошла в Разлом Каньона. Кристалл буфера гудел, сектор\n\
             захлопнулся за спиной. Этерия слушала море, и море отвечало ей\n\
             тишиной. Каждый мускул держал напряжение буферного поля.\n",
        )
        .unwrap();
        std::fs::write(
            d.join("b.md"),
            "# Глава 2\n\nНокс. Просто упоминание.\n",
        )
        .unwrap();
        let out = run_feed(&d, FeedMode::Understand, "Нокс", 10_000, 40, false);
        assert!(out.contains("FEED · understand"), "карта режима: {out}");
        assert!(out.contains("СКЕЛЕТ"), "секция скелета: {out}");
        assert!(out.contains("Бюджет:"), "строка бюджета: {out}");
        // Первый выбранный якорь — из плотного файла (ε выше, чем у b.md).
        let first_scope_pos = out.find("```text").expect("скоуп в карте");
        assert!(
            out[..first_scope_pos].contains("a.md"),
            "первым должен идти плотный a.md: {}",
            &out[..first_scope_pos]
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn feed_understand_json_packet_shape() {
        let d = temp_dir("json");
        std::fs::write(
            d.join("a.md"),
            "# Глава 1\n\nСлепой Пастырь Экхарт шёл по Архисфере. Сеть Юниций\n\
             молчала. Протока светилась в темноте, и Одесса спала.\n",
        )
        .unwrap();
        let out = run_feed(&d, FeedMode::Understand, "Экхарт", 8000, 40, true);
        let v: serde_json::Value = serde_json::from_str(&out).expect("валидный JSON");
        assert_eq!(v["feed"], "understand");
        assert_eq!(v["query"], "Экхарт");
        assert!(v["spent_tokens"].as_u64().unwrap() > 0);
        assert!(v["items"].as_array().unwrap().len() >= 1);
        assert!(v["energy"]["median"].is_f64());
        assert!(v["advice"].as_str().unwrap().contains("тематическое ядро"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn feed_edit_low_energy_first_with_verdicts() {
        let d = temp_dir("edit");
        std::fs::write(
            d.join("dense.md"),
            "# Глава 1\n\nНокс вонзила когти в буферное поле. Разлом Каньона\n\
             держал удар: кристалл, сектор, Архисфера, Сеть Юниций — всё\n\
             гудело от напряжения. Этерия и Протока, Одесса и Пастырь.\n",
        )
        .unwrap();
        std::fs::write(d.join("sparse.md"), "# Глава 2\n\nНокс мимоходом.\n").unwrap();
        let out = run_feed(&d, FeedMode::Edit, "Нокс", 10_000, 40, false);
        assert!(out.contains("FEED · edit"), "карта edit: {out}");
        assert!(out.contains("ЦЕЛЬ ПРАВКИ"), "секция цели: {out}");
        assert!(
            out.contains("разреженная зона")
                || out.contains("плотная зона")
                || out.contains("середина"),
            "вердикты плотности: {out}"
        );
        assert!(out.contains("СОВЕТ"), "совет: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn feed_edit_code_impact_block() {
        // Вендор-устойчивость: собственный исходник движка всегда рядом.
        let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let src = project.join("src/shell");
        if !src.exists() {
            return;
        }
        let out = run_feed(&src, FeedMode::Edit, "cmd_search", 60_000, 40, false);
        assert!(out.contains("FEED · edit"), "режим: {out}");
        assert!(out.contains("ВЛИЯНИЕ"), "AIDDE-блок: {out}");
        assert!(
            out.contains("точка определения"),
            "точка определения в карте: {out}"
        );
        assert!(
            out.contains("upstream") || out.contains("downstream"),
            "call graph в карте: {out}"
        );
    }

    #[test]
    fn feed_no_hits_honest_message() {
        let d = temp_dir("empty");
        std::fs::write(d.join("x.md"), "# Глава 1\n\nСовершенно посторонний текст.\n").unwrap();
        let out = run_feed(&d, FeedMode::Understand, "квадроберинг", 8000, 40, false);
        assert!(out.contains("0 якорей"), "честный ноль: {out}");
        assert!(out.contains("psi"), "подсказка про psi: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn feed_dedups_same_scene() {
        let d = temp_dir("dedup");
        // Два упоминания запроса внутри ОДНОЙ сцены → один элемент пакета.
        std::fs::write(
            d.join("a.md"),
            "# Глава 1\n\nНокс прошла по коридору. Нокс остановилась у окна.\n\n# Глава 2\n\nНокс снова вышла ночью, и фонари дрожали в тумане,\n\nа город спал под низким небом, и снег шёл медленно,\n\nпока не закончилась страница и глава.\n",
        )
        .unwrap();
        let out = run_feed(&d, FeedMode::Understand, "Нокс", 8000, 40, true);
        let v: serde_json::Value = serde_json::from_str(&out).expect("JSON");
        let items = v["items"].as_array().unwrap();
        // Сцена одна (маркдаун-сцена = секция) — повторение внутри неё
        // не должно раздувать пакет.
        let mut keys = HashSet::new();
        for it in items {
            let scope = it["scope"].as_str().unwrap().to_string();
            keys.insert(scope.chars().take(40).collect::<String>());
        }
        assert_eq!(keys.len(), items.len(), "дубликаты сцен в пакете: {out}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn feed_over_budget_marker_is_honest() {
        let d = temp_dir("over");
        std::fs::write(
            d.join("big.md"),
            "# Глава 1\n\nНокс шла долго. И текст был длинным, гораздо длиннее\n\
             крошечного бюджета, который задаст тест: предложение за\n\
             предложением, строка за строкой, чтобы скоуп гарантированно\n\
             не влез в бюджет и движок честно об этом сказал.\n",
        )
        .unwrap();
        // Карта: пометка о превышении.
        let card = run_feed(&d, FeedMode::Edit, "Нокс", 64, 40, false);
        assert!(
            card.contains("превышает бюджет"),
            "пометка о превышении: {card}"
        );
        // JSON: булев флаг.
        let js = run_feed(&d, FeedMode::Edit, "Нокс", 64, 40, true);
        let v: serde_json::Value = serde_json::from_str(&js).expect("JSON");
        assert_eq!(v["over_budget"], true, "флаг over_budget: {js}");
        assert!(
            v["spent_tokens"].as_u64().unwrap() > v["budget_tokens"].as_u64().unwrap(),
            "потрачено больше бюджета: {js}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cmd_feed_requires_query_and_path() {
        let mut s = ShellState::new(std::path::PathBuf::from("/tmp/x.db"));
        // без аргументов → справка
        match super::super::commands::dispatch(&mut s, "feed") {
            CmdResult::Done(out) => assert!(out.contains("--for"), "справка: {out}"),
            _ => panic!("expected Done"),
        }
        // путь есть, запроса нет → подсказка
        match super::super::commands::dispatch(&mut s, "feed /tmp") {
            CmdResult::Done(out) => assert!(out.contains("--query"), "подсказка query: {out}"),
            _ => panic!("expected Done"),
        }
        // несуществующий путь → ошибка
        match super::super::commands::dispatch(
            &mut s,
            "feed /nonexistent-dir-xyz --for edit -q foo",
        ) {
            CmdResult::Done(out) => assert!(out.contains("не найден"), "ошибка пути: {out}"),
            _ => panic!("expected Done"),
        }
    }

    #[test]
    fn percentile_and_verdict_boundaries() {
        let sorted = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(percentile(&sorted, 0.5), 2.5);
        assert_eq!(percentile(&sorted, 0.0), 1.0);
        assert_eq!(percentile(&sorted, 1.0), 4.0);
        assert_eq!(energy_verdict(0.75), "плотная зона");
        assert_eq!(energy_verdict(0.25), "разреженная зона");
        assert_eq!(energy_verdict(0.5), "середина");
    }

    #[test]
    fn definition_note_is_honest_about_count() {
        assert!(definition_note(0).contains("нет"), "extern: честный ноль");
        assert!(
            definition_note(1).contains("единственная"),
            "одна точка: {:?}",
            definition_note(1)
        );
        assert!(
            definition_note(5).contains("несколько"),
            "много точек: {:?}",
            definition_note(5)
        );
    }
}
