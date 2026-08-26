//! NLM Corpus Ingestion: NotebookLM → `web-index.db` для унифицированного
//! кросс-юниверсального поиска (v0.14.0).
//!
//! Заметки/источники/артефакты NotebookLM становятся страницами в общем
//! индексе `poler-engine` — тот же `--web-search` пробивает одновременно
//! приватный корпус NLM, локальный код и проползенный веб. Рёбра `links`
//! связывают заметки ↔ источники ↔ ноутбуки ↔ внешние URL — PageRank
//! работает сквозь все юниверсумы (веб-страница → NLM-источник → заметка
//! → другой ноутбук → …).
//!
//! Доноры (из предыдущих витков):
//! * **Percolator-lite** (v0.9): сравнение `content_hash` — повторный синк
//!   пропускает неизменившиеся заметки за O(1) lookup;
//! * **Positional Inverted Index** (v0.11): фразы «"..."» ищутся по
//!   смежности delta-varint позиций и в заметках NLM;
//! * **PageRank** (v0.8): итерации по `links` — авторитет заметки
//!   зависит от того, сколько источников/артефактов на неё ссылаются.
//!
//! URL-схема NLM-страниц:
//! ```text
//! nlm://notebook/{nb_id}                          — паспорт ноутбука
//! nlm://notebook/{nb_id}/source/{src_id}          — контент источника
//! nlm://notebook/{nb_id}/note/{note_id}           — текст заметки/чата
//! nlm://notebook/{nb_id}/artifact/{art_id}        — Studio-объект
//! ```
//! Внешние URL источников (Google Docs, YouTube, …) попадают в `links`
//! как обычные строки — они совпадают с URL проползенных веб-страниц,
//! образуя сквозной граф.

use serde_json::Value;

use crate::web::index::{WebDoc, WebIndex};

use super::nlm::{NlmSession, Notebook};

// ---------------------------------------------------------------------------
// URL-схема
// ---------------------------------------------------------------------------

/// URL паспорта ноутбука в web-index.
pub fn notebook_url(nb_id: &str) -> String {
    format!("nlm://notebook/{nb_id}")
}

/// URL контента источника.
pub fn source_url(nb_id: &str, src_id: &str) -> String {
    format!("nlm://notebook/{nb_id}/source/{src_id}")
}

/// URL заметки (chat/notes).
pub fn note_url(nb_id: &str, note_id: &str) -> String {
    format!("nlm://notebook/{nb_id}/note/{note_id}")
}

/// URL Studio-артефакта.
pub fn artifact_url(nb_id: &str, art_id: &str) -> String {
    format!("nlm://notebook/{nb_id}/artifact/{art_id}")
}

// ---------------------------------------------------------------------------
// content_hash (Percolator-lite): FNV-1a 64-bit hex, без новых зависимостей
// ---------------------------------------------------------------------------

/// Детерминированный хеш контента для Percolator-lite (пропуск
/// неизменившихся страниц). FNV-1a 64-bit → hex 16 символов:
/// достаточно различающей способности для текстовых документов NLM
/// (коллизия ~1 на 2^32 при 4 млрд документов — за пределами корпуса
/// персонального NLM-аккаунта).
pub fn content_hash(text: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in text.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{h:016x}")
}

// ---------------------------------------------------------------------------
// Парсер cFji9: заметки ноутбука (raw JSON → Vec<ParsedNote>)
// ---------------------------------------------------------------------------

/// Распароченная заметка: id + текст + опциональный заголовок.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedNote {
    pub id: String,
    pub text: String,
    pub title: Option<String>,
}

/// Хождение по массиву Google (толерантно к null).
fn at(v: &Value, i: usize) -> &Value {
    v.get(i).unwrap_or(&Value::Null)
}

/// Парсинг ответа `GET_NOTES` (cFji9) → заметки.
///
/// Формат payload (из реального продакшен-дампа v0.13.0):
/// ```text
/// [ items_array, metadata_array ]
/// items_array = [ item, item, … ]
/// item = [ id, [ id, text, ?, ?, title?, ...optional_metadata? ] ]
/// ```
/// Внутренний массив имеет 5 или 6 элементов (Google вариативен), текст
/// всегда на `inner[1]`, заголовок — на `inner[4]` (опционально).
///
/// Эвристика wrapper-detection: `data[0]` это items-массив (а не
/// сам item) ⇔ `data[0][0]` это массив (item = `[id, inner]`).
/// Если `data[0][0]` строка — значит data уже bare items, не обёрнут.
pub fn parse_notes(data: &Value) -> Vec<ParsedNote> {
    // bare vs wrapped: data = [items_array, metadata] vs data = items_array
    let items = if at(data, 0).is_array() && at(at(data, 0), 0).is_array() {
        at(data, 0) // wrapped: data[0] = items
    } else {
        data // bare: data = items
    };
    let arr = match items.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let id = match at(item, 0).as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let inner = at(item, 1);
        if !inner.is_array() {
            continue;
        }
        let text = match at(inner, 1).as_str() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let title = at(inner, 4)
            .as_str()
            .filter(|s| !s.is_empty())
            .map(String::from);
        out.push(ParsedNote { id, text, title });
    }
    out
}

// ---------------------------------------------------------------------------
// IngestStats — ход синкронизации
// ---------------------------------------------------------------------------

/// Статистика инжеста NLM-корпуса в web-index.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct IngestStats {
    /// Число ноутбуков в аккаунте (из list_notebooks).
    pub notebooks: usize,
    /// Ноутбуков-паспортов: новый или изменённый (переиндексирован).
    pub notebooks_reindexed: usize,
    pub notebooks_unchanged: usize,
    /// Источников: контент SourceContent влит в индекс.
    pub sources_reindexed: usize,
    pub sources_unchanged: usize,
    /// Заметок: текст заметок NLM влит в индекс.
    pub notes_reindexed: usize,
    pub notes_unchanged: usize,
    /// Studio-артефактов: метаданные влиты.
    pub artifacts_reindexed: usize,
    pub artifacts_unchanged: usize,
    /// Накопленные ошибки (не фатальные — синк продолжается).
    pub errors: Vec<String>,
}

impl IngestStats {
    pub fn total_reindexed(&self) -> usize {
        self.notebooks_reindexed
            + self.sources_reindexed
            + self.notes_reindexed
            + self.artifacts_reindexed
    }
    pub fn total_unchanged(&self) -> usize {
        self.notebooks_unchanged
            + self.sources_unchanged
            + self.notes_unchanged
            + self.artifacts_unchanged
    }
    pub fn total_pages(&self) -> usize {
        self.total_reindexed() + self.total_unchanged()
    }
}

// ---------------------------------------------------------------------------
// Главный API: sync_all + ingest_notebook
// ---------------------------------------------------------------------------

/// Полный синк всех ноутбуков аккаунта в web-index.
///
/// Шаги:
/// 1. `list_notebooks()` — все ноутбуки аккаунта (RPC `wXbhsf`);
/// 2. для каждого ноутбука — `ingest_notebook` (паспорт + источники +
///    заметки + артефакты);
/// 3. `recompute_pagerank(20)` — сквозной граф по всем юниверсам.
///
/// Возвращает `IngestStats` с детальным счётом. Ошибки отдельных
/// ноутбуков/источников НЕ фатальны — копятся в `stats.errors`, синк
/// идёт дальше.
pub fn sync_all(ix: &mut WebIndex, sess: &mut NlmSession) -> Result<IngestStats, String> {
    let nbs = sess.list_notebooks()?;
    let mut stats = IngestStats {
        notebooks: nbs.len(),
        ..Default::default()
    };
    for nb in nbs {
        if let Err(e) = ingest_notebook(ix, sess, &nb, &mut stats) {
            stats
                .errors
                .push(format!("notebook {} ({}): {e}", nb.id, nb.title));
        }
    }
    // PageRank по сквозному графу (веб-страницы + NLM-страницы)
    let _ = ix.recompute_pagerank(20);
    Ok(stats)
}

/// Синк одного ноутбука в web-index.
///
/// Создаёт 4 типа страниц:
/// 1. `nlm://notebook/{nb_id}` — паспорт (title + список источников);
/// 2. `nlm://notebook/{nb_id}/source/{src_id}` — контент каждого источника
///    (через `load_source`), включая URL картинок слайдов в `links`;
/// 3. `nlm://notebook/{nb_id}/note/{note_id}` — каждая заметка/чат NLM;
/// 4. `nlm://notebook/{nb_id}/artifact/{art_id}` — каждый Studio-объект
///    (title + kind + status + source_ids → ссылки на источники).
///
/// Все рёбра `links` замыкают граф: заметка → ноутбук, артефакт →
/// источник, источник → внешний URL (Google Docs/YouTube) — PageRank
/// распространяет авторитет сквозь NLM и веб.
pub fn ingest_notebook(
    ix: &mut WebIndex,
    sess: &mut NlmSession,
    nb: &Notebook,
    stats: &mut IngestStats,
) -> Result<(), String> {
    // 1) Паспорт ноутбука — title + source list (короткий, для обнаружения
    //    по имени ноутбука и навигации к источникам).
    let mut nb_text = format!("# {} {}\n\n", nb.emoji, nb.title);
    nb_text.push_str("Источники:\n");
    for s in &nb.sources {
        nb_text.push_str(&format!("- [{}] {} — id={}\n", s.kind, s.title, s.id));
        if let Some(u) = &s.url {
            nb_text.push_str(&format!("  url: {u}\n"));
        }
    }
    let nb_links: Vec<String> = nb.sources.iter().map(|s| source_url(&nb.id, &s.id)).collect();
    let doc = WebDoc {
        url: notebook_url(&nb.id),
        title: nb.title.clone(),
        lang: "uk".into(),
        meta_description: format!("NotebookLM: {}", nb.title),
        text: nb_text.clone(),
        links: nb_links,
        content_hash: content_hash(&nb_text),
    };
    match ix.upsert_page(&doc) {
        Ok((_, true)) => stats.notebooks_reindexed += 1,
        Ok((_, false)) => stats.notebooks_unchanged += 1,
        Err(e) => stats.errors.push(format!("notebook {}: {e}", nb.id)),
    }

    // 2) Источники — load_source per src (медленно, но честно: RPC на источник).
    for src in &nb.sources {
        let sc = match sess.load_source(&nb.id, &src.id) {
            Ok(sc) => sc,
            Err(e) => {
                stats
                    .errors
                    .push(format!("load_source {}-{}: {e}", nb.id, src.id));
                continue;
            }
        };
        // Текст: либо контент source, либо (для слайдов) описание картинок.
        let mut src_text = format!("# {}\n\nТип: {}\n", sc.title, sc.kind);
        if let Some(c) = &sc.content {
            src_text.push('\n');
            src_text.push_str(c);
        }
        if !sc.images.is_empty() {
            src_text.push_str("\n\nКартинки слайдов:\n");
            for img in &sc.images {
                src_text.push_str(&format!("- {} (id={})\n", img.url, img.id.as_deref().unwrap_or("?")));
            }
        }
        // Ссылки: паспорт ноутбука + внешние URL источников (для сквозного графа)
        let mut src_links = vec![notebook_url(&nb.id)];
        if let Some(u) = &src.url {
            src_links.push(u.clone());
        }
        for img in &sc.images {
            src_links.push(img.url.clone());
        }
        let doc = WebDoc {
            url: source_url(&nb.id, &src.id),
            title: format!("{} — {}", src.title, sc.kind),
            lang: "uk".into(),
            meta_description: src.kind.clone(),
            text: src_text.clone(),
            links: src_links,
            content_hash: content_hash(&src_text),
        };
        match ix.upsert_page(&doc) {
            Ok((_, true)) => stats.sources_reindexed += 1,
            Ok((_, false)) => stats.sources_unchanged += 1,
            Err(e) => stats
                .errors
                .push(format!("source {}-{}: {e}", nb.id, src.id)),
        }
    }

    // 3) Заметки — raw cFji9 → parse_notes → по одной странице на заметку.
    match sess.notes(&nb.id) {
        Ok(raw) => {
            let notes = parse_notes(&raw);
            for note in notes {
                let note_links = vec![notebook_url(&nb.id)];
                let title = note.title.clone().unwrap_or_else(|| nb.title.clone());
                let doc = WebDoc {
                    url: note_url(&nb.id, &note.id),
                    title,
                    lang: "uk".into(),
                    meta_description: format!("NotebookLM note in {}", nb.title),
                    text: note.text.clone(),
                    links: note_links,
                    content_hash: content_hash(&note.text),
                };
                match ix.upsert_page(&doc) {
                    Ok((_, true)) => stats.notes_reindexed += 1,
                    Ok((_, false)) => stats.notes_unchanged += 1,
                    Err(e) => stats
                        .errors
                        .push(format!("note {}-{}: {e}", nb.id, note.id)),
                }
            }
        }
        Err(e) => stats
            .errors
            .push(format!("notes {}: {e}", nb.id)),
    }

    // 4) Studio-артефакты — title + kind + status + source_ids → ссылки.
    match sess.artifacts(&nb.id) {
        Ok(arts) => {
            for art in arts {
                let art_text = format!(
                    "# {}\n\nТип: {}\nСтатус: {}\nИсточники: {}\n",
                    art.title,
                    art.kind,
                    art.status,
                    art.source_ids.join(", ")
                );
                let mut art_links = vec![notebook_url(&nb.id)];
                for sid in &art.source_ids {
                    art_links.push(source_url(&nb.id, sid));
                }
                let doc = WebDoc {
                    url: artifact_url(&nb.id, &art.id),
                    title: art.title.clone(),
                    lang: "uk".into(),
                    meta_description: format!("NLM Studio: {}", art.kind),
                    text: art_text.clone(),
                    links: art_links,
                    content_hash: content_hash(&art_text),
                };
                match ix.upsert_page(&doc) {
                    Ok((_, true)) => stats.artifacts_reindexed += 1,
                    Ok((_, false)) => stats.artifacts_unchanged += 1,
                    Err(e) => stats
                        .errors
                        .push(format!("artifact {}-{}: {e}", nb.id, art.id)),
                }
            }
        }
        Err(e) => stats
            .errors
            .push(format!("artifacts {}: {e}", nb.id)),
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Тесты
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- URL helpers ----

    #[test]
    fn url_scheme_stable_and_unique() {
        assert_eq!(notebook_url("nb1"), "nlm://notebook/nb1");
        assert_eq!(source_url("nb1", "src-a"), "nlm://notebook/nb1/source/src-a");
        assert_eq!(note_url("nb1", "n1"), "nlm://notebook/nb1/note/n1");
        assert_eq!(artifact_url("nb1", "art-1"), "nlm://notebook/nb1/artifact/art-1");
        assert_ne!(note_url("nb1", "n1"), note_url("nb1", "n2"));
        assert_ne!(note_url("nb1", "n1"), note_url("nb2", "n1"));
    }

    // ---- content_hash ----

    #[test]
    fn content_hash_deterministic_and_unique() {
        let a = content_hash("hello world");
        let b = content_hash("hello world");
        assert_eq!(a, b, "FNV-1a 64-hex стабилен");
        assert_eq!(a.len(), 16, "16 hex-символов");
        assert_ne!(a, content_hash("hello World"), "чувствителен к регистру");
        assert_ne!(a, content_hash("hello world "), "чувствителен к пробелу");
        assert_eq!(content_hash("").len(), 16);
    }

    // ---- parse_notes ----

    #[test]
    fn parse_notes_six_item_inner() {
        let data = json!([[
            ["n1", ["n1", "Текст першої заметки.",
                    ["meta1", "meta2", "meta3"],
                    ["extra"],
                    "Заголовок 1",
                    ["m1", "m2", "m3", "m4", "m5"]]],
            ["n2", ["n2", "Друга заметка без метаданных.",
                    ["x"],
                    null,
                    "Заголовок 2"]],
        ], [42, null]]);
        let notes = parse_notes(&data);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].id, "n1");
        assert_eq!(notes[0].text, "Текст першої заметки.");
        assert_eq!(notes[0].title.as_deref(), Some("Заголовок 1"));
        assert_eq!(notes[1].id, "n2");
        assert_eq!(notes[1].text, "Друга заметка без метаданных.");
        assert_eq!(notes[1].title.as_deref(), Some("Заголовок 2"));
    }

    #[test]
    fn parse_notes_five_item_inner() {
        let data = json!([[
            ["x1", ["x1", "Коротка заметка", ["a"], null, "Заголовок X"]],
        ]]);
        let notes = parse_notes(&data);
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, "x1");
        assert_eq!(notes[0].text, "Коротка заметка");
        assert_eq!(notes[0].title.as_deref(), Some("Заголовок X"));
    }

    #[test]
    fn parse_notes_bare_array_no_wrapper() {
        let data = json!([
            ["a1", ["a1", "текст 1", [], null, "T1"]],
            ["a2", ["a2", "текст 2", [], null, "T2"]]
        ]);
        let notes = parse_notes(&data);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].id, "a1");
        assert_eq!(notes[1].id, "a2");
    }

    #[test]
    fn parse_notes_skips_empty_or_invalid() {
        let data = json!([[
            ["", ["", "пустой id"]],
            ["ok", ["ok", "валидная"]],
            ["bad1", "не массив"],
            ["bad2", ["bad2", ""]],
            ["bad3", ["bad3", 42]],
            ["good", ["good", "текст", [], null, "Заголовок"]],
        ]]);
        let notes = parse_notes(&data);
        assert_eq!(notes.len(), 2);
        assert_eq!(notes[0].id, "ok");
        assert_eq!(notes[1].id, "good");
    }

    #[test]
    fn parse_notes_empty_input() {
        assert!(parse_notes(&json!(null)).is_empty());
        assert!(parse_notes(&json!([])).is_empty());
        assert!(parse_notes(&json!({})).is_empty());
    }

    #[test]
    fn parse_notes_real_production_layout() {
        let data = json!([[
            ["1549a611-f31b-4717-be1c-a0555c58cc25",
             ["1549a611-f31b-4717-be1c-a0555c58cc25",
              "Ваше уточнення архітектури персонажів...",
              ["kind", "inst", "audit"],
              ["extra1"],
              "Канон Уламка: Юридична Логіка",
              ["m1", "m2", "m3", "m4", "m5"]]],
            ["2026828e-2de8-4477-aef4-1503e5d65ce6",
             ["2026828e-2de8-4477-aef4-1503e5d65ce6",
              "Ниже представлен аудит ошибок и новая биохимическая модель...",
              ["kind2", "inst2"],
              null,
              "Біохімічна модель Кассіопеї Astra-Nic Complex"]],
        ], [42, null]]);
        let notes = parse_notes(&data);
        assert_eq!(notes.len(), 2);
        assert!(notes[0].text.starts_with("Ваше уточнення"));
        assert_eq!(notes[0].title.as_deref(), Some("Канон Уламка: Юридична Логіка"));
        assert!(notes[1].text.starts_with("Ниже представлен"));
        assert_eq!(notes[1].title.as_deref(), Some("Біохімічна модель Кассіопеї Astra-Nic Complex"));
    }

    // ---- IngestStats arithmetic ----

    #[test]
    fn stats_totals() {
        let s = IngestStats {
            notebooks: 3,
            notebooks_reindexed: 2,
            notebooks_unchanged: 1,
            sources_reindexed: 10,
            sources_unchanged: 5,
            notes_reindexed: 24,
            notes_unchanged: 0,
            artifacts_reindexed: 2,
            artifacts_unchanged: 8,
            errors: vec![],
        };
        assert_eq!(s.total_reindexed(), 2 + 10 + 24 + 2);
        assert_eq!(s.total_unchanged(), 1 + 5 + 0 + 8);
        assert_eq!(s.total_pages(), s.total_reindexed() + s.total_unchanged());
    }

    // ---- Интеграционный тест: WebIndex + parse_notes + ingest ----

    #[test]
    fn ingest_then_search_finds_note() {
        let mut ix = WebIndex::open_memory().unwrap();
        let nb_id = "nb-test";
        let note_text = "Cassiopeia Astra-Nic Complex biochemistry deep space model";
        let doc = WebDoc {
            url: note_url(nb_id, "n1"),
            title: "Note about Cassiopeia".into(),
            lang: "en".into(),
            meta_description: "test note".into(),
            text: note_text.to_string(),
            links: vec![notebook_url(nb_id)],
            content_hash: content_hash(note_text),
        };
        let (id, reindexed) = ix.upsert_page(&doc).unwrap();
        assert!(id > 0);
        assert!(reindexed);

        let hits = ix.search("cassiopeia", 5).unwrap();
        assert!(!hits.is_empty(), "поиск находит инжестнутую заметку");
        assert_eq!(hits[0].url, note_url(nb_id, "n1"));

        let (_, reindexed2) = ix.upsert_page(&doc).unwrap();
        assert!(!reindexed2, "Percolator-lite: неизменившийся документ не переиндексируется");
    }

    #[test]
    fn ingest_creates_link_graph() {
        let mut ix = WebIndex::open_memory().unwrap();
        let nb_id = "nb-graph";
        let note_doc = WebDoc {
            url: note_url(nb_id, "n1"),
            title: "Note".into(),
            lang: "uk".into(),
            meta_description: "".into(),
            text: "some text here".into(),
            links: vec![notebook_url(nb_id)],
            content_hash: content_hash("some text here"),
        };
        let nb_doc = WebDoc {
            url: notebook_url(nb_id),
            title: "Notebook".into(),
            lang: "uk".into(),
            meta_description: "".into(),
            text: "notebook passport".into(),
            links: vec![],
            content_hash: content_hash("notebook passport"),
        };
        ix.upsert_page(&note_doc).unwrap();
        ix.upsert_page(&nb_doc).unwrap();
        ix.recompute_pagerank(5).unwrap();

        let rank: f64 = ix
            .conn()
            .query_row(
                "SELECT rank FROM pages WHERE url = ?1",
                rusqlite::params![notebook_url(nb_id)],
                |r| r.get(0),
            )
            .unwrap();
        assert!(rank > 0.0, "паспорт ноутбука получил PageRank от заметки");
    }

    #[test]
    fn ingestion_is_idempotent() {
        let mut ix = WebIndex::open_memory().unwrap();
        let text = "idempotent content";
        let doc = WebDoc {
            url: note_url("nb-i", "n1"),
            title: "T".into(),
            lang: "uk".into(),
            meta_description: "".into(),
            text: text.into(),
            links: vec![],
            content_hash: content_hash(text),
        };
        let (_, r1) = ix.upsert_page(&doc).unwrap();
        let (_, r2) = ix.upsert_page(&doc).unwrap();
        assert!(r1, "первый upsert — переиндексация");
        assert!(!r2, "второй — skip (Percolator-lite)");
    }
}
