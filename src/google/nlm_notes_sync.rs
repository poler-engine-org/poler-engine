//! Двусторонняя синхронизация заметок poler-engine ↔ NotebookLM.
//!
//! Цель — «везде одинаковые заметки»: заметки ноутбука NLM видны и
//! редактируемы локально (SQLite `poler_notes`), а локальные заметки,
//! привязанные к ноутбуку, загружаются в облако NLM.
//!
//! Соответствие версий хранится тегом `nlm:<notebooklm-uuid>` в колонке
//! `tags` локальной заметки:
//!
//! * **Pull** (облако → локально): облачная заметка без тега-пары
//!   создаётся локально (`source = "nlm"`); с тегом и отличающимся
//!   телом — обновляется (облако — источник истины для nlm-заметок).
//! * **Push** (локально → облако): локальная заметка, привязанная к
//!   ноутбуку, без тега `nlm:*` и с `source != "nlm"` создаётся в NLM
//!   (CREATE_NOTE + UPDATE_NOTE), после чего получает тег `nlm:<id>`.
//! * Удалённые в облаке заметки локально НЕ удаляются (безопасность):
//!   их просто больше нет в списке NLM.
//!
//! Mind maps живут в том же сторе, что заметки, и отфильтровываются
//! на уровне парсера ([`crate::google::nlm::parse_nlm_notes`]).

use rusqlite::Connection;

use crate::google::nlm::{NlmNote, NlmSession};
use crate::notes::{self, Note, NoteSource};

/// Тег-маркер связи с облачной заметкой NLM.
pub fn nlm_tag(cloud_note_id: &str) -> String {
    format!("nlm:{cloud_note_id}")
}

/// Найти в тегах заметки маркер `nlm:<uuid>` (если есть).
fn cloud_id_of(note: &Note) -> Option<String> {
    note.tags
        .iter()
        .find(|t| t.starts_with("nlm:") && t.len() > "nlm:".len())
        .map(|t| t["nlm:".len()..].to_string())
}

/// План синхронизации: что создать/обновить, вычисляется чисто, без сети.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncPlan {
    /// Облачные заметки, которых нет локально → INSERT.
    pub pull_new: Vec<NlmNote>,
    /// Пары (локальная, облачная) с различающимся телом → UPDATE локальной.
    pub pull_update: Vec<(Note, NlmNote)>,
    /// Локальные заметки, которых нет в облаке → CREATE_NOTE в NLM.
    pub push_new: Vec<Note>,
}

impl SyncPlan {
    pub fn is_empty(&self) -> bool {
        self.pull_new.is_empty() && self.pull_update.is_empty() && self.push_new.is_empty()
    }
}

/// Сопоставить облачные и локальные заметки ноутбука.
///
/// Локальными считаются заметки с `notebook_id == Some(notebook_id)`.
pub fn plan_notes_sync(cloud: &[NlmNote], local_all: &[Note], notebook_id: &str) -> SyncPlan {
    let local: Vec<&Note> = local_all
        .iter()
        .filter(|n| n.notebook_id.as_deref() == Some(notebook_id))
        .collect();

    let mut plan = SyncPlan::default();
    let mut matched_local: Vec<&Note> = Vec::new();

    for cn in cloud {
        // пара по тегу nlm:<cloud.id>
        let pair = local.iter().find(|n| cloud_id_of(n).as_deref() == Some(cn.id.as_str()));
        match pair {
            Some(l) => {
                matched_local.push(l);
                if l.body != cn.text || l.title != cn.title {
                    plan.pull_update.push(((*l).clone(), cn.clone()));
                }
            }
            None => plan.pull_new.push(cn.clone()),
        }
    }

    // push: локальные этого ноутбука, не из облака и ещё не синхронизированные
    for l in local {
        let already_synced = cloud_id_of(l).is_some();
        let from_cloud = l.source == NoteSource::Nlm.as_str();
        if !already_synced && !from_cloud && !matched_local.iter().any(|m| m.id == l.id) {
            plan.push_new.push(l.clone());
        }
    }

    plan
}

/// Итог выполнения синхронизации.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub pulled_new: usize,
    pub pulled_updated: usize,
    pub pushed: usize,
    pub errors: Vec<String>,
}

impl SyncReport {
    pub fn is_empty(&self) -> bool {
        self.pulled_new == 0 && self.pulled_updated == 0 && self.pushed == 0 && self.errors.is_empty()
    }

    /// Однострочная сводка для чата/вывода.
    pub fn summary(&self) -> String {
        if self.is_empty() {
            return "синхронизации не потребовалось — заметки уже совпадают".to_string();
        }
        let mut parts = Vec::new();
        if self.pulled_new > 0 {
            parts.push(format!("{} скачано с NLM", self.pulled_new));
        }
        if self.pulled_updated > 0 {
            parts.push(format!("{} обновлено из облака", self.pulled_updated));
        }
        if self.pushed > 0 {
            parts.push(format!("{} отправлено в NLM", self.pushed));
        }
        if !self.errors.is_empty() {
            parts.push(format!("{} ошибок", self.errors.len()));
        }
        parts.join(", ")
    }
}

/// Выполнить синхронизацию заметок ноутбука: pull + push.
///
/// Сеть нужна только для `list_notes_structured` и `create_note`;
/// план вычисляется чистой функцией [`plan_notes_sync`].
pub fn sync_notebook_notes(
    sess: &mut NlmSession,
    conn: &Connection,
    notebook_id: &str,
) -> Result<SyncReport, String> {
    let cloud = sess.list_notes_structured(notebook_id)?;
    let local_all = notes::list_notes(conn, 100_000).map_err(|e| format!("list_notes: {e}"))?;
    let plan = plan_notes_sync(&cloud, &local_all, notebook_id);

    let mut report = SyncReport::default();

    // ---- pull: облако → локально ----
    for cn in &plan.pull_new {
        let tags = vec![nlm_tag(&cn.id)];
        match notes::add_note(conn, &cn.title, &cn.text, &tags, NoteSource::Nlm, Some(notebook_id)) {
            Ok(_) => report.pulled_new += 1,
            Err(e) => report.errors.push(format!("pull {}: {e}", cn.id)),
        }
    }
    for (l, cn) in &plan.pull_update {
        // облако — источник истины для nlm-заметок; прочие теги сохраняем
        let mut tags = l.tags.clone();
        let tag = nlm_tag(&cn.id);
        if !tags.contains(&tag) {
            tags.push(tag);
        }
        match notes::update_note(conn, l.id, &cn.title, &cn.text, &tags) {
            Ok(_) => report.pulled_updated += 1,
            Err(e) => report.errors.push(format!("pull-update {}: {e}", l.id)),
        }
    }

    // ---- push: локально → облако ----
    for l in &plan.push_new {
        match sess.create_note(notebook_id, &l.title, &l.body) {
            Ok(cloud_id) => {
                let mut tags = l.tags.clone();
                let tag = nlm_tag(&cloud_id);
                if !tags.contains(&tag) {
                    tags.push(tag);
                }
                if let Err(e) = notes::update_note(conn, l.id, &l.title, &l.body, &tags) {
                    report.errors.push(format!("push-тег {}: {e}", l.id));
                }
                report.pushed += 1;
            }
            Err(e) => report.errors.push(format!("push «{}»: {e}", l.title)),
        }
    }

    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cn(id: &str, title: &str, text: &str) -> NlmNote {
        NlmNote { id: id.into(), title: title.into(), text: text.into() }
    }

    fn ln(id: i64, title: &str, body: &str, tags: Vec<String>, source: NoteSource) -> Note {
        Note {
            id,
            title: title.into(),
            body: body.into(),
            tags,
            source: source.as_str().into(),
            notebook_id: Some("nb-1".into()),
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn plan_pull_new_and_push_new() {
        let cloud = vec![cn("c-1", "Облачная", "текст A")];
        let local = vec![ln(1, "Локальная", "текст B", vec![], NoteSource::Manual)];
        let plan = plan_notes_sync(&cloud, &local, "nb-1");
        assert_eq!(plan.pull_new.len(), 1);
        assert_eq!(plan.push_new.len(), 1);
        assert!(plan.pull_update.is_empty());
    }

    #[test]
    fn plan_matched_noop_when_identical() {
        let cloud = vec![cn("c-1", "Заметка", "текст")];
        let local = vec![ln(
            1,
            "Заметка",
            "текст",
            vec![nlm_tag("c-1")],
            NoteSource::Nlm,
        )];
        let plan = plan_notes_sync(&cloud, &local, "nb-1");
        assert!(plan.is_empty());
    }

    #[test]
    fn plan_update_when_body_differs() {
        let cloud = vec![cn("c-1", "Заметка", "новая версия")];
        let local = vec![ln(
            1,
            "Заметка",
            "старая версия",
            vec![nlm_tag("c-1")],
            NoteSource::Nlm,
        )];
        let plan = plan_notes_sync(&cloud, &local, "nb-1");
        assert_eq!(plan.pull_update.len(), 1);
        assert!(plan.pull_new.is_empty());
        assert!(plan.push_new.is_empty());
    }

    #[test]
    fn plan_ignores_other_notebooks_and_synced() {
        let cloud = vec![];
        let other_nb = Note {
            notebook_id: Some("nb-другой".into()),
            ..ln(9, "Чужая", "текст", vec![], NoteSource::Manual)
        };
        let already = ln(2, "Отсинкана", "текст", vec![nlm_tag("c-77")], NoteSource::Manual);
        let plan = plan_notes_sync(&cloud, &[other_nb, already], "nb-1");
        assert!(plan.push_new.is_empty());
    }

    #[test]
    fn nlm_source_not_pushed_twice() {
        // заметка source=nlm без тега (edge: тег потерян) — не пушим, а снова скачаем
        let cloud = vec![cn("c-2", "Облачная", "текст")];
        let local = vec![ln(3, "Облачная-копия", "текст", vec![], NoteSource::Nlm)];
        let plan = plan_notes_sync(&cloud, &local, "nb-1");
        assert_eq!(plan.pull_new.len(), 1);
        assert!(plan.push_new.is_empty());
    }

    #[test]
    fn sync_report_summary_formats() {
        let mut r = SyncReport::default();
        assert!(r.summary().contains("совпадают"));
        r.pulled_new = 2;
        r.pushed = 1;
        assert!(r.summary().contains("2 скачано"));
        assert!(r.summary().contains("1 отправлено"));
    }

    #[test]
    fn sync_notebook_notes_end_to_end_memory_sqlite() {
        // чистая логика pull без сети: имитируем облако через plan+вставки вручную,
        // т.к. NlmSession требует Chromium. Проверяем БД-часть пайплайна.
        let conn = Connection::open_in_memory().unwrap();
        notes::ensure_schema(&conn).unwrap();
        let plan = plan_notes_sync(
            &[cn("c-9", "Из облака", "тело")],
            &[ln(5, "Локал", "локал-тело", vec![], NoteSource::Manual)],
            "nb-1",
        );
        assert_eq!(plan.pull_new.len(), 1);
        // применяем pull_new как это делает sync_notebook_notes
        for cn in &plan.pull_new {
            notes::add_note(
                &conn,
                &cn.title,
                &cn.text,
                &[nlm_tag(&cn.id)],
                NoteSource::Nlm,
                Some("nb-1"),
            )
            .unwrap();
        }
        let all = notes::list_notes(&conn, 100).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].source, "nlm");
        assert_eq!(all[0].tags, vec![nlm_tag("c-9")]);
        assert_eq!(all[0].notebook_id.as_deref(), Some("nb-1"));

        // повторный план — пустой: всё совпадает
        let plan2 = plan_notes_sync(
            &[cn("c-9", "Из облака", "тело")],
            &notes::list_notes(&conn, 100).unwrap(),
            "nb-1",
        );
        assert!(plan2.is_empty());
    }
}
