//! # Help + ? palette (v2.0 sovereign stack)
//!
//! Полная man-подобная справка по всем командам poler-shell + интерактивная
//! палитра готовых сценариев (вызывается `?` в TUI или REPL).
//!
//! ## Структура
//! - `help_overview()` — общий список команд по группам (как `help` без аргументов)
//! - `help_topic(name)` — детальная справка по одной команде/группе:
//!   `help gh`, `help sources add` и т.д.
//! - `palette_scenarios()` — готовые пресеты для `?` palette
//!
//! v2.0: группа NLM удалена вместе с Google/NotebookLM-интеграциями.

use std::fmt::Write;

/// Группа команд (для фильтрации в `help`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpGroup {
    Search,
    Crawl,
    Impact,
    Vcs,
    Gix,
    Sync,
    Set,
    Notes,
    Sources,
    // v0.47.0
    System,
    Agent,
    Meta,
    // v0.48.0
    Calc,
}

impl HelpGroup {
    pub fn as_str(self) -> &'static str {
        match self {
            HelpGroup::Search => "Пошук",
            HelpGroup::Crawl => "Crawl",
            HelpGroup::Impact => "AIDDE",
            HelpGroup::Vcs => "GitHub/GitLab/Gitea",
            HelpGroup::Gix => "gix (local)",
            HelpGroup::Sync => "Sync VCS",
            HelpGroup::Set => "Налаштування",
            HelpGroup::Notes => "Notes (CRUD)",
            HelpGroup::Sources => "Sources (CRUD)",
            // v0.47.0
            HelpGroup::System => "Система/Win+Linux (v0.47.0)",
            HelpGroup::Agent => "Середа агента (v0.47.0)",
            HelpGroup::Meta => "Мета",
            HelpGroup::Calc => "Калькулятор Всего (v0.48.0)",
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

/// Все команды poler-shell (полный реестр для `help`). v2.0: NLM удалены.
pub fn all_entries() -> Vec<HelpEntry> {
    vec![
        HelpEntry { group: HelpGroup::Search, cmd: "search \"<query>\" [--top N]", short: "Пошук по web-index.db (веб+локал)" },
        HelpEntry { group: HelpGroup::Search, cmd: "web \"<query>\"", short: "Аліас для search" },
        HelpEntry { group: HelpGroup::Search, cmd: "stats", short: "Статистика web-index.db: сторінки, байти, PageRank" },

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
        HelpEntry { group: HelpGroup::Notes, cmd: "F3 (Transcript)", short: "Лента локальної історії питань у TUI: пари питання→відповідь, копіювання" },

        HelpEntry { group: HelpGroup::Sources, cmd: "sources list", short: "Усі джерела" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources add <value> [--kind file|url|repo] [--label \"...\"]", short: "Додати джерело (kind авто-детектується)" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources rm <id>", short: "Видалити джерело" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources test <id>", short: "Перевірити доступність" },
        HelpEntry { group: HelpGroup::Sources, cmd: "sources open <id>", short: "Відкрити в системі через xdg-open" },

        // v0.47.0: система — Win+Linux словарь + прямой шелл
        HelpEntry { group: HelpGroup::System, cmd: "cd <path> | cd", short: "Змінити каталог (без аргументів — показати поточний, як у cmd.exe)" },
        HelpEntry { group: HelpGroup::System, cmd: "pwd", short: "Показати поточний каталог" },
        HelpEntry { group: HelpGroup::System, cmd: "clear | cls", short: "Очистити екран (ANSI; cls — Windows-словарь)" },
        HelpEntry { group: HelpGroup::System, cmd: "engine <args...>", short: "Виклик CLI самого движка (self-exec): engine --benchmark, engine --poler-box ..." },
        HelpEntry { group: HelpGroup::System, cmd: "! <command>", short: "Прямий виклик системного шелла (sh -c)" },
        HelpEntry { group: HelpGroup::System, cmd: "sh | bash | exec <command>", short: "Виконати команду в системному шеллі" },
        HelpEntry { group: HelpGroup::System, cmd: "<path>.poler [args]", short: "Автозапуск .poler-контейнера в ізольованому poler-box" },
        HelpEntry { group: HelpGroup::System, cmd: "<системна команда>", short: "Transparent PATH passthrough: ls, cargo, python3, git..." },
        HelpEntry { group: HelpGroup::System, cmd: "dir | type | copy | del | md | ren | move", short: "Windows-словарь → ls/cat/cp/rm/mkdir/mv (деталі: `win`)" },
        HelpEntry { group: HelpGroup::System, cmd: "findstr | tasklist | taskkill | ipconfig | ping", short: "Windows-словарь → grep/ps/kill/ip addr (флаги переводяться)" },
        HelpEntry { group: HelpGroup::System, cmd: "win", short: "Повний каталог Windows-команд і їх трансляцій" },

        // v0.48.0: Калькулятор Всего
        HelpEntry { group: HelpGroup::Calc, cmd: "calc <expr> | = <expr>", short: "Вычислить всё: арифметика, единицы (to), матрицы expm/eigen, триты, законы" },
        HelpEntry { group: HelpGroup::Calc, cmd: "calc solve <eq>", short: "Корни уравнения: полиномы (Дюран–Кернер) и трансцендентные (Ньютон+бисекция)" },
        HelpEntry { group: HelpGroup::Calc, cmd: "calc constants|units|funcs|laws", short: "Каталоги: константы CODATA/IAU, единицы, функции, законы физики" },
        HelpEntry { group: HelpGroup::Calc, cmd: "calc script <law> [k=v]", short: "Сгенерировать скрипт/.poler-правило по закону физики (Kepler, Циолковский…)" },
        HelpEntry { group: HelpGroup::Calc, cmd: "hw [--json]", short: "Скрытые параметры ПК: кеши L1-L3, ISA-флаги, NUMA, GPU, топология" },

        // v0.47.0: середа для ІІ-агентів (Antigravity)
        HelpEntry { group: HelpGroup::Agent, cmd: "sysinfo | systeminfo", short: "Карта середовища: CPU/RAM/GPU/диск/тулчейни/локаль/TTY — звіт для агента" },
        HelpEntry { group: HelpGroup::Agent, cmd: "env [PREFIX]", short: "Знімок змінних сесії (токени маскуються)" },
        HelpEntry { group: HelpGroup::Agent, cmd: "set NAME=VALUE", short: "Змінна сесії (Windows-стиль); set NAME — показати" },
        HelpEntry { group: HelpGroup::Agent, cmd: "pty <command>", short: "PTY-міст: запуск інтерактивних утиліт (top, gdb) через pseudo-tty" },
        HelpEntry { group: HelpGroup::Agent, cmd: "agent", short: "Статус і поради агенту: --exec, --json, MCP, таймінги" },
        HelpEntry { group: HelpGroup::Agent, cmd: "--exec '<cmd>' [--json]", short: "CLI: одноразове виконання без банера; --json — машиночитаний конверт" },

        // v0.47.0: POLER Reader — приложение живого голоса
        HelpEntry { group: HelpGroup::Agent, cmd: "read <книга> [--out x.wav] [--voice V] [--seed N]", short: "ЖИВОЙ ГОЛОС книги: роторный резонатор + коартикуляция (txt/md/fb2/poler-book)" },

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
        HelpGroup::Crawl,
        HelpGroup::Impact,
        HelpGroup::Vcs,
        HelpGroup::Gix,
        HelpGroup::Sync,
        HelpGroup::Set,
        HelpGroup::Notes,
        HelpGroup::Sources,
        HelpGroup::System,
        HelpGroup::Agent,
        HelpGroup::Meta,
    ];
    let _ = writeln!(s, "poler-shell v2.0 — sovereign stack: без Google/NotebookLM, локальный поиск без лимитов");
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
    s.push_str("  Tab/BackTab — зміна фокусу між панелями\n");
    s.push_str("  ↑/↓ — навігація у списках / історія у вводі\n");
    s.push_str("  PgUp/PgDn — прокрутка Chat panel\n");
    s.push_str("  Enter — виконати команду\n");
    s.push_str("  клік/Enter на джерелі (Sources) — список документів джерела (Doc Browser)\n");
    s.push_str("  клік/Enter на документі — вікно з документом у тому ж терміналі (Doc Viewer)\n");
    s.push_str("  o — відкрити джерело/документ зовні ($EDITOR / браузер)\n");
    s.push_str("  Ctrl+N — нова замітка (вбудований редактор)\n");
    s.push_str("  Ctrl+E — редагувати вибрану замітку\n");
    s.push_str("  F3 — Transcript: лента локальної історії питань\n");
    s.push_str("       переживають перезапуски; Enter — повна відповідь, y — копіювати)\n");
    s.push_str("  Ctrl+Y — копіювати виділення мишею в буфер (OSC 52 + системний)\n");
    s.push_str("  Ctrl+Shift+V / Ctrl+V — вставка з буфера (bracketed paste)\n");
    s.push_str("  ? — палітра сценаріїв\n");
    s.push_str("  Esc / Ctrl+C — вихід\n");
    s.push('\n');
    s.push_str("Деталі: `help <topic>` (напр., `help sources add`, `help gix clone`)\n");
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
    // 2) Совпадение по группе (single-word name, например `gh`, `notes`)
    //    Если name — это одна "команда-родитель" (gh/gl/gt/gix/sources/notes/...),
    //    покажем все её подкоманды.
    let parent_cmds = [
        "search", "web", "stats", "sync", "set", "crawl", "impact",
        "gh", "gl", "gt", "gix", "notes", "sources", "version", "quit", "help",
        "calc", "hw",
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
        HelpGroup::Crawl,
        HelpGroup::Impact,
        HelpGroup::Vcs,
        HelpGroup::Gix,
        HelpGroup::Sync,
        HelpGroup::Set,
        HelpGroup::Notes,
        HelpGroup::Sources,
        HelpGroup::Meta,
        HelpGroup::Calc,
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
    // 4) "sources add", "gix clone" и т.п. — частичное совпадение по prefix
    for e in &entries {
        if e.cmd.starts_with(&name_lower) {
            return format_entry_detail(e);
        }
    }
    // 5) v0.48.0: развёрнутая тема калькулятора — полный каталог возможностей
    if name_lower == "calc" || name_lower == "калькулятор" || name_lower == "=" {
        return calc_help_topic();
    }
    if name_lower == "hw" || name_lower == "hardware" {
        return format!(
            "─── {} — hw [—json] ───\n\n{}\n\nСкрытые от глаз параметры ПК: кеши L1d/L1i/L2/L3 \
по индексам sysfs, ISA-флаги (AVX/AVX2/AVX-512/AES…), топология сокетов/ядер/потоков, \
NUMA-узлы, bogomips, размеры страниц, диски (HDD/SSD), GPU (nvidia-smi или PCI IDs sysfs), \
гипервизор.\n\nПримеры:\n  poler> hw\n  poler> hw --json\n  poler-engine --exec \"hw --json\" --json\n",
            HelpGroup::Calc.as_str(),
            "Зонд железа: то, что не видно в htop"
        );
    }
    format!("тема не знайдена: {name} (введіть `help` для повного списку)")
}

/// v0.48.0: полная справка «Калькулятора Всего» с примерами.
fn calc_help_topic() -> String {
    let mut s = String::new();
    let _ = writeln!(s, "─── Калькулятор Всего (v0.48.0) ───");
    let _ = writeln!(s);
    let _ = writeln!(s, "Три входа: poler> calc <expr> · префикс = <expr> · --exec \"calc …\" --json");
    let _ = writeln!(s, "В TUI: клавиша = в Normal-режиме открывает виджет с живым preview.");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Арифметика и алгебра ──");
    let _ = writeln!(s, "  calc (1538 * 485) / 1024        = 728.447265625");
    let _ = writeln!(s, "  calc 2^3^2                      = 512 (правоассоц.), -2^2 = -4, 5! = 120");
    let _ = writeln!(s, "  calc 0.5!                       = 0.8862… (Γ(1.5) = √π/2)");
    let _ = writeln!(s, "  calc x = 5; calc x^2 + 1        переменные; ans — последний результат");
    let _ = writeln!(s, "  calc solve x^2 - 4 = 0          x ∈ 2.0, -2.0 (Дюран–Кернер)");
    let _ = writeln!(s, "  calc solve x^2 + 1 = 0          комплексные: x ∈ i, -i");
    let _ = writeln!(s, "  calc solve sin(x) = 0.5         численно: Ньютон + бисекция (±100)");
    let _ = writeln!(s, "  calc gcd(1071, 462) | next_prime(1e6) | factorize(360) | fib(70)");
    let _ = writeln!(s, "  calc binomial(52, 5)            = 2598960 (покерные руки)");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Единицы измерения (to) ──");
    let _ = writeln!(s, "  calc 5 km + 300 m               = 5.3 km (размерности строго)");
    let _ = writeln!(s, "  calc 100 km/h to m/s            = 27.777…");
    let _ = writeln!(s, "  calc degC(100) to degF          = 212.0 (аффинные температуры)");
    let _ = writeln!(s, "  calc asin(0.5) to deg           = 30.0 (радианы по умолчанию)");
    let _ = writeln!(s, "  calc c to km/h                  = 1.079…e9 (c — и константа, и единица)");
    let _ = writeln!(s, "  calc 1 TiB to byte              = 1099511627776.0 (IEC префиксы)");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Специальная математика ──");
    let _ = writeln!(s, "  calc gamma(0.5)                 = 1.7724… (√π), Ланцрош g=7");
    let _ = writeln!(s, "  calc erf(1)                     = 0.8427008 (A&S 7.1.26)");
    let _ = writeln!(s, "  calc zeta(2)                    = 1.6449… (π²/6, Эйлер–Маклорен)");
    let _ = writeln!(s, "  calc zeta(-1)                   = -0.08333… (−1/12)");
    let _ = writeln!(s);
    let _ = writeln!(s, "── POLER Matrix Calc (квант) ──");
    let _ = writeln!(s, "  calc [1,2;3,4] * [5;6]          матрицы: det inv trace transpose");
    let _ = writeln!(s, "  calc expm([0,-1;1,0] * psi)     вращение Ли: expm(J·Ψ), Паде [6/6]");
    let _ = writeln!(s, "  calc eigen(rot2(pi/2))          = [i, -i] — чисто мнимые");
    let _ = writeln!(s, "  calc charpoly([1,2;3,4])        = [1, -5, -2] (Фаддеев–Леврерье)");
    let _ = writeln!(s, "  calc so_gen(3,0,2) * 1.0        генератор so(3); A^(-1) — обратная");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Триты POLER (сбалансированная троичная) ──");
    let _ = writeln!(s, "  calc trits(5)                   = \"1TT\"; trit_val(\"1TT\") = 5");
    let _ = writeln!(s, "  calc trit_and(\"1TT\", \"10T\")   вентили Клини: min/max/инверсия");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Астрономия (Шлhyter + NOAA; якоря — реальные затмения) ──");
    let _ = writeln!(s, "  calc moon_illum(2024,4,8,18.35) ≈ 0 (солнечное затмение 08.04.2024)");
    let _ = writeln!(s, "  calc moon_phase(2025,3,14,6.9)  ≈ 180 (полнолуние-затмение)");
    let _ = writeln!(s, "  calc sun_lon(2024,3,20,3.1)     ≈ 0 (равноденствие)");
    let _ = writeln!(s, "  calc planet_lon(\"mars\",2024,6,1)  геоцентрическая долгота");
    let _ = writeln!(s, "  calc sunrise(50.45,30.52,2024,6,21)  восход в Киеве, часы UTC");
    let _ = writeln!(s, "  calc moon_dist(2024,1,1,12)     расстояние до Луны, км");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Геодезия и навигация ──");
    let _ = writeln!(s, "  calc dist(50.45,30.52,49.84,24.03)   Киев—Львов, км (большой круг)");
    let _ = writeln!(s, "  calc bearing(50.45,30.52,49.84,24.03) азимут, град");
    let _ = writeln!(s, "  calc dest(50.45,30.52,45,500)   точка в 500 км на северо-восток");
    let _ = writeln!(s, "  calc earth_radius(55.75)        радиус кривизны WGS84, км");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Генератор скриптов по законам ──");
    let _ = writeln!(s, "  calc laws                      19+ законов: Кеплер, Циолковский, Шварцшильд…");
    let _ = writeln!(s, "  calc script kepler3             готовая команда calc + .poler-правило");
    let _ = writeln!(s, "  calc script emc2 m=2 kg         со своими значениями");
    let _ = writeln!(s);
    let _ = writeln!(s, "── Каталоги ──");
    let _ = writeln!(s, "  calc constants [фильтр]         CODATA 2022 / IAU / СИ-2019 (с источниками)");
    let _ = writeln!(s, "  calc units | calc funcs         единицы и функции");
    let _ = writeln!(s, "  calc vars | calc hist           переменные и история");
    let _ = writeln!(s, "  hw | hw --json                 скрытые параметры ПК");
    s
}

fn format_entry_detail(e: &HelpEntry) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "─── {} — {} ───\n", e.group.as_str(), e.cmd);
    let _ = writeln!(s, "{}\n", e.short);
    // Примеры для конкретных команд
    match e.cmd {
        "search \"<query>\" [--top N]" => {
            s.push_str("ПРИКЛАДИ:\n");
            s.push_str("  search \"Касіопея Astra-Nic\" --top 5\n");
            s.push_str("  search \"POLER cycle\"\n\n");
            s.push_str("ВИВІД:\n");
            s.push_str("  Гібридний пошук по веб+локал: PageRank + lexical + temporal.\n");
            s.push_str("  Текст можна виділити мишею → Ctrl+Y → буфер\n");
            s.push_str("  (OSC 52 — працює і через SSH/tmux). Вставка — Ctrl+Shift+V.\n");
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
            s.push_str("Інтерактивна палітра сценаріїв:\n");
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

/// Готовые сценарии для `?` palette (v2.0: без NLM).
pub fn palette_scenarios() -> Vec<Scenario> {
    vec![
        Scenario {
            title: "1. Пошук по corpus: \"POLER cycle\"",
            cmd: "search \"POLER cycle\" --top 10",
            help: "Гібридний пошук по веб+локал: PageRank + lexical + temporal.",
        },
        Scenario {
            title: "2. Статистика web-index.db",
            cmd: "stats",
            help: "JSON: pages, terms, postings, links, db_bytes, duplicates.",
        },
        Scenario {
            title: "3. AIDDE impact-аналіз символу",
            cmd: "impact ./src run",
            help: "Impact-паспорт символу: call graph + upstream/downstream.",
        },
        Scenario {
            title: "4. Останні коміти GitHub: rust-lang/rust",
            cmd: "gh commits rust-lang/rust",
            help: "Покаже 20 останніх комітів (потрібен $GITHUB_TOKEN в .env).",
        },
        Scenario {
            title: "5. Пошук коду по GitHub: \"PageRank\"",
            cmd: "gh search PageRank --top 20",
            help: "Знайде 20 кодових хітів по всьому GitHub. Метадані: file, repo, score.",
        },
        Scenario {
            title: "6. gix log локального репо poler-engine",
            cmd: "gix log /home/z/my-project/poler-engine --top 10",
            help: "Pure-Rust gix: git log без зовнішнього git. Топ-10 коммітів.",
        },
        Scenario {
            title: "7. Створити нову замітку",
            cmd: "notes add \"Нова ідея\"",
            help: "Відкриє вбудований TUI редактор (tui-textarea). Ctrl+S зберегти.",
        },
        Scenario {
            title: "8. Додати джерело: посилання на Rust docs",
            cmd: "sources add https://doc.rust-lang.org/std/ --label \"Rust std\"",
            help: "Авто-детект url; з'явиться в правій панелі Sources TUI.",
        },
        Scenario {
            title: "9. Pure-Rust git clone + LFS",
            cmd: "gix clone https://github.com/user/large-repo ./lr",
            help: "Без системного git! Shallow + branch: --depth 1 --branch main. Потім: gix lfs fetch ./lr.",
        },
        Scenario {
            title: "10. Синк VCS → web-index.db",
            cmd: "sync vcs gh",
            help: "Індексує коміти/issues відслідковуваних GitHub-репо у web-index.",
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
        assert!(!e.iter().any(|x| x.cmd.starts_with("nlm")), "v2.0: nlm удалён");
        assert!(e.iter().any(|x| x.cmd.starts_with("gh")));
        assert!(e.iter().any(|x| x.cmd.starts_with("notes")));
        assert!(e.iter().any(|x| x.cmd.starts_with("sources")));
    }

    #[test]
    fn overview_lists_every_group() {
        let s = help_overview();
        for g in [
            HelpGroup::Search,
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
    fn topic_search_has_examples() {
        let s = help_topic("search");
        assert!(s.contains("ПРИКЛАДИ"));
        assert!(s.contains("POLER cycle"));
        assert!(s.contains("Ctrl+Y"));
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
    fn palette_has_10_scenarios() {
        let v = palette_scenarios();
        assert_eq!(v.len(), 10);
        for sc in &v {
            assert!(!sc.title.is_empty());
            assert!(!sc.cmd.is_empty());
            assert!(!sc.cmd.starts_with("nlm"), "v2.0: nlm удалён");
        }
    }

    #[test]
    fn palette_topic_lists_all() {
        let s = help_topic("?");
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
    fn topic_nlm_is_gone_in_v2() {
        let s = help_topic("nlm");
        assert!(s.contains("не знайдена"), "v2.0: темы nlm больше нет: {s}");
    }
}
