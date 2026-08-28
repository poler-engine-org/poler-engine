//! # Help 2.0 + ? palette (v0.17.3 — Companion Bridge M2+M3+M4)
//!
//! Полная man-подобная справка по всем командам poler-shell + интерактивная
//! палитра из 10 готовых сценариев (вызывается `?` в TUI или REPL).
//!
//! ## Структура
//! - `help_overview()` — общий список команд по группам (как `help` без аргументов)
//! - `help_topic(name)` — детальная справка по одной команде/группе:
//!   `help nlm`, `help nlm ask`, `help gh`, `help sources add` и т.д.
//! - `palette_scenarios()` — 11 готових пресетів для `?` palette

use std::fmt::Write;

/// Группа команд (для фильтрации в `help`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpGroup {
    Search,
    Nlm,
    Crawl,
    Impact,
    Vcs,
    Gix,
    Sync,
    Set,
    Notes,
    Sources,
    Meta,
}

impl HelpGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            HelpGroup::Search => "Пошук",
            HelpGroup::Nlm => "NLM",
            HelpGroup::Crawl => "Crawl",
            HelpGroup::Impact => "AIDDE",
            HelpGroup::Vcs => "GitHub/GitLab/Gitea",
            HelpGroup::Gix => "gix (local)",
            HelpGroup::Sync => "Sync VCS",
            HelpGroup::Set => "Налаштування",
            HelpGroup::Notes => "Notes (CRUD)",
            HelpGroup::Sources => "Sources (CRUD)",
            HelpGroup::Meta => "Мета",
        }
    }
}

/// Одна строка в спрaвке.
#[derive(Debug, Clone)]
pub struct HelpEntry {
    pub group: HelpGroup,
    pub cmd: &'static str,
    pub short: &'static str,
}

/// Все команды poler-shell v0.17.3 (полный реестр для `help`).
pub fn all_entries() -> Vec<HelpEntry> {
    vec![
        HelpEntry { group: HelpGroup::Search, cmd: "search \"<query>\" [--top N]", short: "Пошук по web-index.db (NLM+веб+локал)" },
        HelpEntry { group: HelpGroup::Search, cmd: "web \"<query>\"", short: "Аліас для search" },
        HelpEntry { group: HelpGroup::Search, cmd: "stats", short: "Статистика web-index.db: сторінки, байти, PageRank" },

        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm list", short: "Список 87 ноутбуків акаунту (notebooklm.google.com)" },
        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm notes <NB_ID>", short: "Замітки ноутбука (без mind maps)" },
        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm notes-sync [<NB_ID>]", short: "Синк заміток двобічний: хмара ↔ poler_notes" },
        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm artifacts <NB_ID>", short: "Studio-артефакти: Audio/Slide/Report/Video/Quiz" },
        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm source <NB_ID> <SRC_ID>", short: "Контент джерела + URL слайдів" },
        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm account", short: "email/налаштування сесії NLM" },
        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm ask <NB_ID> \"питання\"", short: "Відповідь моделі ПО ДЖЕРЕЛАМ ноутбука" },
        HelpEntry { group: HelpGroup::Nlm, cmd: "nlm sync [<NB_ID>]", short: "Синк NLM → web-index.db (без аргумента = всі)" },

        HelpEntry { group: HelpGroup::Crawl, cmd: "crawl <URL> [--depth N] [--max M] [--cross] [--delay-ms N]", short: "Обхід URL → web-index.db (CDP+Chromium)" },
        HelpEntry { group: HelpGroup::Impact, cmd: "impact <PATH> <SYMBOL> [--depth N] [--cache <DB>]", short: "AIDDE impact-паспорт символу в кодовій базі" },

        HelpEntry { group: HelpGroup::Vcs, cmd: "gh search <Q> [--top N]", short: "Пошук по коду GitHub (треба $GITHUB_TOKEN)" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gh repos <USER>", short: "Список репозиторіїв користувача GitHub" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gh commits <OWNER/REPO>", short: "Останні 20 комітів репо" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gh issues <OWNER/REPO>", short: "issues+PR репозиторію" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gl search <Q>", short: "Пошук по GitLab (REST v4)" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gl commits <GROUP/PROJ>", short: "Коміти GitLab проєкту" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gl issues <GROUP/PROJ>", short: "issues+MR GitLab" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gt search <Q>", short: "Пошук по Gitea/Forgejo ($GITEA_HOST)" },
        HelpEntry { group: HelpGroup::Vcs, cmd: "gt commits <OWNER/REPO>", short: "Коміти Gitea" },

        HelpEntry { group: HelpGroup::Gix, cmd: "gix log <PATH> [--top N]", short: "git log локального репо через Pure-Rust gix" },
        HelpEntry { group: HelpGroup::Gix, cmd: "gix clone <URL> <PATH> [--depth N] [--branch B]", short: "Pure-Rust git clone через gix::clone::PrepareFetch" },
        HelpEntry { group: HelpGroup::Gix, cmd: "gix lfs list <PATH>", short: "Знайти LFS pointer-файли у worktree" },
        HelpEntry { group: HelpGroup::Gix, cmd: "gix lfs fetch <PATH>", short: "Завантажити LFS-об'єкти через batch API" },

        HelpEntry { group: HelpGroup::Sync, cmd: "sync vcs [gh|gl|gt] <OWNER>", short: "Синк VCS-сторінок у web-index.db" },

        HelpEntry { group: HelpGroup::Set, cmd: "set format md|json|simple", short: "Перемикнути формат виводу" },
        HelpEntry { group: HelpGroup::Set, cmd: "set top N", short: "Топ-K за замовчуванням для search" },

        HelpEntry { group: HelpGroup::Notes, cmd: "notes list", short: "Усі замітки (новіші зверху)" },
        HelpEntry { group: HelpGroup::Notes, cmd: "notes add <title>", short: "Створити порожню замітку (тіло введете у TUI редакторі)" },
        HelpEntry { group: HelpGroup::Notes, cmd: "notes show <id>", short: "Показати повну замітку" },
        HelpEntry { group: HelpGroup::Notes, cmd: "notes edit <id>", short: "Редагувати замітку (TUI редактор)" },
        HelpEntry { group: HelpGroup::Notes, cmd: "notes rm <id>", short: "Видалити замітку" },
        HelpEntry { group: HelpGroup::Notes, cmd: "notes save-from-ai", short: "Зберегти останню відповідь nlm ask як замітку (ті саме що Ctrl+S)" },
        HelpEntry { group: HelpGroup::Notes, cmd: "F3 (Transcript)", short: "Лента чату nlm ask в TUI: пари питання→відповідь, повна відповідь, копіювання" },

        HelpEntry { group: HelpGroup::Sources, cmd: "sources list", short: "Усі джерела" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources add <value> [--kind file|url|repo] [--label \"...\"]", short: "Додати джерело (kind авто-детектується)" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources rm <id>", short: "Видалити джерело" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources test <id>", short: "Перевірити доступність" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources open <id>", short: "Відкрити в системі через xdg-open" },

        HelpEntry { group: HelpGroup::Meta, cmd: "version | v", short: "Версія poler-engine + poler-shell" },
        HelpEntry { group: HelpGroup::Meta, cmd: "quit | exit | q", short: "Вийти з шелу" },
        HelpEntry { group: HelpGroup::Meta, cmd: "help | ?", short: "Ця справка" },
        HelpEntry { group: HelpGroup::Meta, cmd: "? (palette)", short: "Інтерактивна палітра 10 сценаріїв" },
    ]
}

/// Общая справка (без аргументов): список всех команд по группам.
pub fn help_overview() -> String {
    let mut s = String::new();
    let entries = all_entries();
    let groups = [
        HelpGroup::Search,
        HelpGroup::Nlm,
        HelpGroup::Crawl,
        HelpGroup::Impact,
        HelpGroup::Vcs,
        HelpGroup::Gix,
        HelpGroup::Sync,
        HelpGroup::Set,
        HelpGroup::Notes,
        HelpGroup::Sources,
        HelpGroup::Meta,
    ];
    let _ = writeln!(s, "poler-shell v0.17.3 — TUI Redesign + Companion Bridge M2+M3+M4 (MiMo Code-style 4-pane + mouse + CRUD + оф. NotebookLM API I/O + TUI Enter-handler)");
    let _ = writeln!(s, "Доступні команди ({}):\n", entries.len());
    for g in groups {
        let g_entries: Vec<&HelpEntry> = entries.iter().filter(|e| e.group == g).collect();
        if g_entries.is_empty() {
            continue;
        }
        let _ = writeln!(s, "[ {} ]", g.as_str());
        for e in g_entries {
            let _ = writeln!(s, "  {:<46} — {}", e.cmd, e.short);
        }
        s.push('\n');
    }
    s.push_str("Керування TUI:\n");
    s.push_str("  Tab/BackTab — зміна фокусу між 4 панелями\n");
    s.push_str("  ↑/↓ — навігація у списках / історія у вводі\n");
    s.push_str("  PgUp/PgDn — прокрутка Chat panel\n");
    s.push_str("  Enter — виконати команду\n");
    s.push_str("  клік/Enter на джерелі (Sources) — список документів джерела (Doc Browser)\n");
    s.push_str("  клік/Enter на документі — вікно з документом у тому ж терміналі (Doc Viewer)\n");
    s.push_str("  o — відкрити джерело/документ зовні ($EDITOR / браузер)\n");
    s.push_str("  Ctrl+N — нова замітка (вбудований редактор)\n");
    s.push_str("  Ctrl+S — зберегти відповідь AI як замітку\n");
    s.push_str("  F3 — Transcript: лента чату nlm ask (пари питання→відповідь,\n");
    s.push_str("       переживають перезапуски; Enter — повна відповідь, y — копіювати)\n");
    s.push_str("  Ctrl+Y — копіювати виділення мишею в буфер (OSC 52 + системний)\n");
    s.push_str("  Ctrl+Shift+V / Ctrl+V — вставка з буфера (bracketed paste)\n");
    s.push_str("  ? — палітра сценаріїв (11 пресетів)\n");
    s.push_str("  Esc / Ctrl+C — вихід\n");
    s.push('\n');
    s.push_str("Деталі: `help <topic>` (напр., `help nlm ask`, `help sources add`)\n");
    s
}

/// Детальная справка по одной команде/группе.
pub fn help_topic(name: &str) -> String {
    let entries = all_entries();
    let name_lower = name.trim().to_lowercase();
    // 1) Точное совпадение по полному cmd (например `?` или полный текст)
    for e in &entries {
        if e.cmd.to_lowercase() == name_lower {
            return format_entry_detail(e);
        }
    }
    // 2) Совпадение по группе (single-word name, например `nlm`, `gh`, `notes`)
    //    Если name — это одна "команда-родитель" (nlm/gh/gl/gt/gix/sources/notes/...),
    //    покажем все её подкоманды.
    let parent_cmds = [
        "search", "web", "stats", "nlm", "sync", "set", "crawl", "impact",
        "gh", "gl", "gt", "gix", "notes", "sources", "version", "quit", "help",
    ];
    if parent_cmds.contains(&name_lower.as_str()) {
        let matching: Vec<&HelpEntry> = entries
            .iter()
            .filter(|e| e.cmd.split_whitespace().next() == Some(name_lower.as_str()))
            .collect();
        if !matching.is_empty() {
            // Если ровно одна команда в группе → её детальный вид
            if matching.len() == 1 {
                return format_entry_detail(matching[0]);
            }
            // Иначе — групповой вид
            let g = matching[0].group;
            return format_group_detail(g, &entries);
        }
    }
    // 3) Совпадение по группе как строка
    for g in [
        HelpGroup::Search,
        HelpGroup::Nlm,
        HelpGroup::Crawl,
        HelpGroup::Impact,
        HelpGroup::Vcs,
        HelpGroup::Gix,
        HelpGroup::Sync,
        HelpGroup::Set,
        HelpGroup::Notes,
        HelpGroup::Sources,
        HelpGroup::Meta,
    ] {
        if name_lower == g.as_str().to_lowercase()
            || name_lower
                == g
                    .as_str()
                    .split(|c: char| c == '/' || c == ' ')
                    .next()
                    .unwrap_or("")
                    .to_lowercase()
        {
            return format_group_detail(g, &entries);
        }
    }
    // 4) "nlm ask", "sources add" и т.п. — частичное совпадение по prefix
    for e in &entries {
        if e.cmd.starts_with(&name_lower) {
            return format_entry_detail(e);
        }
    }
    format!("тема не знайдена: {name} (введіть `help` для повного списку)")
}

fn format_entry_detail(e: &HelpEntry) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "─── {} — {} ───\n", e.group.as_str(), e.cmd);
    let _ = writeln!(s, "{}\n", e.short);
    // Примеры для конкретных команд
    match e.cmd {
        "nlm ask <NB_ID> \"питання\"" => {
            s.push_str("ПРИКЛАДИ:\n");
            s.push_str("  nlm ask 704f2610 \"Параметри Планковської геодезичної\"\n");
            s.push_str("  nlm ask 704f2610-c02b-4ec1-9fc7-a3b72dde2af1 \"Що таке POLER cycle?\"\n\n");
            s.push_str("ВИВІД:\n");
            s.push_str("  Повний текст відповіді моделі (без more_horiz, без keep_pin,\n");
            s.push_str("  без артефактів UI). Текст можна виділити мишею → Ctrl+Y → буфер\n");
            s.push_str("  (OSC 52 — працює і через SSH/tmux). Вставка — Ctrl+Shift+V.\n");
            s.push_str("  Або Ctrl+S щоб зберегти як замітку (source=ai-reply).\n");
        }
        "sources add <value> [--kind file|url|repo] [--label \"...\"]" => {
            s.push_str("ПРИКЛАДИ:\n");
            s.push_str("  sources add https://doc.rust-lang.org/std/\n");
            s.push_str("  sources add /home/z/my-project/poler-engine/src --label \"poler src\"\n");
            s.push_str("  sources add rust-lang/rust --kind repo\n\n");
            s.push_str("АВТО-ДЕТЕКТ kind:\n");
            s.push_str("  http:// | https://         → url\n");
            s.push_str("  owner/repo (1 слеш, без шляху) → repo\n");
            s.push_str("  інше                       → file\n");
        }
        "notes add <title>" => {
            s.push_str("ПРИКЛАД:\n");
            s.push_str("  notes add \"Ідея архітектури v0.18\"\n");
            s.push_str("  → Відкриється TUI редактор для вводу тіла\n");
            s.push_str("  → Ctrl+S зберегти, Esc скасувати\n\n");
            s.push_str("Альтернатива: Ctrl+N прямо з TUI — те саме без команди.\n");
        }
        "gix clone <URL> <PATH> [--depth N] [--branch B]" => {
            s.push_str("ПРИКЛАДИ:\n");
            s.push_str("  gix clone https://github.com/rust-lang/rust ./rust\n");
            s.push_str("  gix clone https://github.com/user/repo ./r --depth 1   # shallow\n");
            s.push_str("  gix clone https://github.com/user/repo ./r --branch dev\n\n");
            s.push_str("ВАЖЛИВО:\n");
            s.push_str("  • Pure-Rust clone через gix::clone::PrepareFetch (без системного git).\n");
            s.push_str("  • PATH має бути порожнім каталогом (gix вимагає destination_must_be_empty).\n");
            s.push_str("  • Для приватних репо: встановіть POLER_GIT_TOKEN або ~/.git-credentials.\n");
            s.push_str("  • LFS-об'єкти НЕ завантажуються автоматично — використовуйте `gix lfs fetch <PATH>`.\n");
            s.push_str("  • Після clone: `gix log <PATH>` покаже коміти, `gix lfs list <PATH>` — LFS-файли.\n");
        }
        "gix lfs list <PATH>" => {
            s.push_str("ПРИКЛАД:\n");
            s.push_str("  gix lfs list ./my-repo\n\n");
            s.push_str("ВАЖЛИВО:\n");
            s.push_str("  • Обходить worktree (не .git/, не hidden-файли).\n");
            s.push_str("  • Шукає файли, що починаються з `version https://git-lfs.github.com/spec/v1`.\n");
            s.push_str("  • Виводить: шлях + oid sha256 (первые 12 хеша) + розмір у байтах.\n");
        }
        "gix lfs fetch <PATH>" => {
            s.push_str("ПРИКЛАД:\n");
            s.push_str("  gix lfs fetch ./my-repo\n\n");
            s.push_str("ЯК ПРАЦЮЄ:\n");
            s.push_str("  1. detect_pointers знаходить усі LFS pointer-файли у worktree.\n");
            s.push_str("  2. Визначає LFS-server URL з .git/config (remote.origin.url + /info/lfs).\n");
            s.push_str("  3. POST /objects/batch з JSON-тілом {operation: download, objects: [...]}\n");
            s.push_str("  4. Для кожного об'єкту завантажує blob у .git/lfs/objects/<oid[:2]>/<oid[2:]>/.\n");
            s.push_str("  5. Звіт: ✓/✗ на кожен об'єкт + підсумок кількості та байтів.\n\n");
            s.push_str("АВТОРИЗАЦІЯ:\n");
            s.push_str("  • Публічні репо: нічого не потрібно.\n");
            s.push_str("  • Приватні: POLER_GIT_TOKEN=<github_pat> передає як Bearer.\n");
            s.push_str("  • Альтернатива: ~/.git-credentials + GIT_TERMINAL_PROMPT=false.\n");
        }
        "? (palette)" => {
            s.push_str("Інтерактивна палітра 11 пресетів:\n");
            for (i, sc) in palette_scenarios().iter().enumerate() {
                s.push_str(&format!("  {}. {}\n", i + 1, sc.title));
            }
            s.push_str("\nENTER — виконати, Esc — закрити, ↑↓/миша — вибір.\n");
        }
        _ => {}
    }
    s
}

fn format_group_detail(g: HelpGroup, entries: &[HelpEntry]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "─── {} ───\n", g.as_str());
    let g_entries: Vec<&HelpEntry> = entries.iter().filter(|e| e.group == g).collect();
    for e in g_entries {
        let _ = writeln!(s, "  {:<46} — {}", e.cmd, e.short);
    }
    s
}

/// Готовый сценарий для `?` palette.
#[derive(Debug, Clone)]
pub struct Scenario {
    pub title: &'static str,
    pub cmd: &'static str,
    pub help: &'static str,
}

/// 10 готовых сценариев (как в mimocode от Xiaomi — палитра команд).
pub fn palette_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            title: "1. Список 87 NotebookLM ноутбуков",
            cmd: "nlm list",
            help: "Покаже всі ноутбуки акаунту NLM з їх UUID, назвами, джерелами.",
        },
        Scenario {
            title: "2. Відповісти на питання по ноутбуку 704f2610",
            cmd: "nlm ask 704f2610 \"Опиши основні тези Прологу\"",
            help: "Аналог \"задати питання\" на сайті NotebookLM — повний текст без more_horiz.",
        },
        Scenario {
            title: "3. Повний sync NLM → web-index.db",
            cmd: "nlm sync",
            help: "Синхронізує всі 87 ноутбуків у локальну БД poler-engine. Тривало.",
        },
        Scenario {
            title: "4. Пошук по corpus: \"POLER cycle\"",
            cmd: "search \"POLER cycle\" --top 10",
            help: "Гібридний пошук по NLM+веб+локал: PageRank + lexical + temporal.",
        },
        Scenario {
            title: "5. Статистика web-index.db",
            cmd: "stats",
            help: "JSON: pages, terms, postings, links, db_bytes, duplicates.",
        },
        Scenario {
            title: "6. Останні коміти GitHub: rust-lang/rust",
            cmd: "gh commits rust-lang/rust",
            help: "Покаже 20 останніх комітів (потрібен $GITHUB_TOKEN в .env).",
        },
        Scenario {
            title: "7. Пошук коду по GitHub: \"PageRank\"",
            cmd: "gh search PageRank --top 20",
            help: "Знайде 20 кодових хітів по всьому GitHub. Метадані: file, repo, score.",
        },
        Scenario {
            title: "8. gix log локального репо poler-engine",
            cmd: "gix log /home/z/my-project/poler-engine --top 10",
            help: "Pure-Rust gix: git log без зовнішнього git. Топ-10 коммітів.",
        },
        Scenario {
            title: "9. Створити нову замітку",
            cmd: "notes add \"Нова ідея\"",
            help: "Відкриє вбудований TUI редактор (tui-textarea). Ctrl+S зберегти.",
        },
        Scenario {
            title: "10. Додати джерело: посилання на Rust docs",
            cmd: "sources add https://doc.rust-lang.org/std/ --label \"Rust std\"",
            help: "Авто-детект url; з'явиться в правій панелі Sources TUI.",
        },
        Scenario {
            title: "11. Pure-Rust git clone + LFS",
            cmd: "gix clone https://github.com/user/large-repo ./lr",
            help: "Без системного git! Shallow + branch: --depth 1 --branch main. Потім: gix lfs fetch ./lr.",
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_entries_nonempty() {
        let e = all_entries();
        assert!(!e.is_empty());
        assert!(e.iter().any(|x| x.cmd.starts_with("nlm")));
        assert!(e.iter().any(|x| x.cmd.starts_with("gh")));
        assert!(e.iter().any(|x| x.cmd.starts_with("notes")));
        assert!(e.iter().any(|x| x.cmd.starts_with("sources")));
    }

    #[test]
    fn overview_lists_every_group() {
        let s = help_overview();
        for g in [
            HelpGroup::Search,
            HelpGroup::Nlm,
            HelpGroup::Crawl,
            HelpGroup::Impact,
            HelpGroup::Vcs,
            HelpGroup::Gix,
            HelpGroup::Sync,
            HelpGroup::Set,
            HelpGroup::Notes,
            HelpGroup::Sources,
            HelpGroup::Meta,
        ] {
            assert!(s.contains(g.as_str()), "overview must contain group {:?}", g);
        }
    }

    #[test]
    fn topic_nlm_ask_has_examples() {
        let s = help_topic("nlm ask");
        assert!(s.contains("ПРИКЛАДИ"));
        assert!(s.contains("704f2610"));
        assert!(s.contains("Ctrl+S"));
    }

    #[test]
    fn topic_sources_add_has_detect_rules() {
        let s = help_topic("sources add");
        assert!(s.contains("АВТО-ДЕТЕКТ"));
        assert!(s.contains("rust-lang/rust"));
    }

    #[test]
    fn topic_notes_add_mentions_editor() {
        let s = help_topic("notes add");
        assert!(s.contains("TUI редактор"));
        assert!(s.contains("Ctrl+S"));
    }

    #[test]
    fn palette_has_11_scenarios() {
        let v = palette_scenarios();
        assert_eq!(v.len(), 11);
        for sc in &v {
            assert!(!sc.title.is_empty());
            assert!(!sc.cmd.is_empty());
        }
    }

    #[test]
    fn palette_topic_lists_all_11() {
        let s = help_topic("?");
        assert!(s.contains("11 пресетів"));
        for sc in palette_scenarios() {
            assert!(s.contains(sc.title), "missing {}", sc.title);
        }
    }

    #[test]
    fn topic_unknown_returns_help_message() {
        let s = help_topic("totally-unknown-xyz");
        assert!(s.contains("не знайдена"));
    }

    #[test]
    fn group_nlm_lists_all_subcommands() {
        let s = help_topic("nlm");
        assert!(s.contains("nlm list"));
        assert!(s.contains("nlm ask"));
        assert!(s.contains("nlm sync"));
        assert!(s.contains("nlm notes"));
        assert!(s.contains("nlm notes-sync"));
        assert!(s.contains("nlm artifacts"));
        assert!(s.contains("nlm source"));
        assert!(s.contains("nlm account"));
    }
}
