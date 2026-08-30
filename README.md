# POLER-Engine

[![CI](https://github.com/poler-engine-org/poler-engine/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/poler-engine-org/poler-engine/actions/workflows/ci.yml)
[![License: POLER Source-Available v1.0](https://img.shields.io/badge/license-POLER%20Source--Available%20v1.0-9b59b6.svg)](LICENSE.md)
[![Rust 1.98](https://img.shields.io/badge/rust-1.98%2B-orange.svg)](Cargo.toml)

**AI-Native Topographical, Resonant and Graph Search Engine** — поисково-аналитический
движок на Rust, спроектированный для вытеснения `grep`/`ripgrep` и слепого векторного
RAG из архитектуры LLM-агентов.

```
poler-engine ~/book -q "нокс" --format ai-json | jq '.anchors[0].k_hop_relations'
```

---

## v0.22.0: Terminal Gateway + Source-Available EULA

Два взаимосвязанных изменения: **верхний уровень управления** (нативный
терминальный шлюз поверх веб-GUI/Auth Companion/WebLens — «нижнего
сервисного слоя») и **новая лицензионная модель** по прецеденту Unreal
Engine EULA.

### Terminal Gateway (`poler-engine --gateway`)

Единое окно терминала (Linux/macOS) с **двойным контуром исполнения**:

1. **Engine Native (приоритет)** — команды движка (`search`, `grep`,
   `chunk`, `crawl`, `impact`, `nlm`, `notes`, `weblens`, `benchmark`…)
   перехватываются и исполняются нативно внутри процесса, без спавна
   внешних шеллов. `grep` здесь — POLER Native Grep (не `/bin/grep`;
   системный — через `!grep` / `host grep`);
2. **Controlled Host OS Proxy** — всё прочее исполняется в хостовой ОС
   через **Sandboxed OS Subshell**: блок деструктивного (`rm -rf /`,
   форк-бомбы, `dd of=/dev/*`, shutdown-семейство, `curl | sh`, запись
   в `/dev/sd*` и `/etc/*`), подтверждение эскалаций (sudo/su, `rm -r`,
   dd), кап вывода 16 МБ, таймаут 120 с, фильтр секретов из env.

**Конвейеры смешивают контуры**: `ls -la | chunk --size 200`,
`cat main.rs | impact main`, `grep "fn " --stdin | wc -l`,
`ls | poler chunk` (префикс опционален). Без `/bin/sh` вообще —
токенизацию делает движок, спавн прямой (класс shell-инъекций устранён).

**Сервисный слой из шлюза**: `service start|stop|status|restart|attach`
(mcp / weblens / companion), `attach mcp` — интерактивный JSON-RPC-клиент
поверх живого MCP-сервера (`tools`, `call <tool> {json}`). Токен сервиса
передаётся через env (не argv — не светится в `/proc/<pid>/cmdline`),
живёт в 0600-файле.

ANSI/VT100, SIGINT — SIGINT группе процессов с grace, SIGWINCH —
перерисовка, история 5000 команд. Архитектура:
`docs/terminal-gateway-architecture.md`.

### Лицензия: Source-Available с раскрытием модификаций

См. раздел «Лицензия» ниже, `LICENSE.md`, `TERMS.md`. `poler-engine
--license` и баннер gateway показывают модель, тир и адрес раскрытия
модификаций. `Cargo.toml` — `license-file = "LICENSE.md"`.

### Ретроспектива v0.21.x (не вошла в README ранее)

- **v0.21.0 Hardening & Precision**: CodeSymbolIdentity (Foo ≠ foo ≠ FOO,
  module::name), Triage Layer в AIDDE (proof vs heuristic), Semantic
  Bridge (офлайн ru↔en, WHY, §8.1 закрыта), Benchmark Suite (POLER grep
  3.3 мс vs ripgrep 6.3 мс при parity 195=195), фикс чанкера; 783 теста;
- **v0.21.1 Security Hardening**: white-box аудит v0.21.0 — 21 позиция
  (0 Critical, 2 HIGH), 12 патчей P1–P12 одним коммитом: guard_path +
  анти-SSRF в MCP (закрыт arbitrary file read и эксфильтрация
  refresh_token), fail-closed пустой токен, REQUEST_DEADLINE 120 с,
  атомарные 0600, CDP-капы; security-гейты `audit_patch_verify.py` 10/10
  + `audit_stress.py --hardened` 46/46.

---

## v0.20.0: Native Retrieval — grep-режим и RAG-чанки в одном бинарнике

Движок — инструмент ИИ-агента, у которого всё под капотом. До v0.20.0
агенту не хватало двух внешних инструментов: grep (полнота, без индекса)
и RAG-конвейера нарезки (passage-уровень вместо целых документов).
Оба вошли в бинарник — анализ эталонов (GNU grep, benbrandt/text-splitter,
LangChain) и gap-таблица: `docs/native-retrieval-analysis.md`.
**Ни одной новой зависимости** — `aho-corasick`, `regex`, `ignore`,
`memchr`, `rayon` уже были в дереве.

### Слой 0: точный поиск (`--grep`, семантика GNU grep)

```bash
poler-engine --grep "weblens_token" src/ --grep-before 1 --grep-after 1
poler-engine --grep "fn [a-z_]+" src/ --grep-regex          # grep -E
poler-engine --grep "токен" . --grep-i                       # Unicode-fold
poler-engine --grep TODO . --grep-count                      # grep -c
poler-engine --grep panic . --grep-list                      # grep -l
poler-engine --grep secret . --grep-json | jq                # машинный отчёт
```

- **Полнота как гарантия**: все совпадения, «ноль значит ноль» —
  живой прогон против GNU grep на `src/`: 1669 = 1669 строк, включая
  кириллицу; скорость release-сборки — 23 мс против 18 мс у GNU grep
  (разница — цена rayon-пула и полного отчёта в памяти).
- Обход ripgrep-класса: .gitignore/.ignore уважаются, скрытые — по
  `--grep-hidden`, симлинки не преследуются.
- Контекст `-A/-B` с групповым разделителем `--` и слиянием слипшихся
  групп — как у GNU grep.
- Exit-коды для скриптов: 0 найдено / 1 пусто / 2 ошибка.
- `--grep-json`: byte_offset строк, байтовые диапазоны вхождений,
  статистика — агент верифицирует совпадения по диапазонам.

### Слой B: RAG-чанки (`--chunk`, passage-уровень)

```bash
poler-engine --chunk FUTURE_ROADMAP.md                    # 25 чанков, breadcrumbs
poler-engine --chunk book.md --chunk-size 512 --chunk-overlap 64
poler-engine --chunk lib.rs --chunk-json | jq '.chunks[0]'
```

- Иерархия уровней (выше = целостнее): секция заголовка → абзац →
  предложение → слово; для кода — блок между пустыми строками → строка
  (никогда внутри строки); code-fence в markdown не режется.
- Вместимость в токенах POLER (целевые 384, перекрытие 48), слияние
  соседей до capacity, хвост меньше минимума приклеивается.
- **Якоря для агента**: `text == original[byte_start..byte_end]` — точный
  срез исходника; номера строк; breadcrumb заголовков; число токенов.

### MCP-инструменты (9 теперь)

`poler_grep` (полнота + JSON-отчёт) и `poler_chunk` (нарезка с якорями)
добавлены к `poler_search`/`poler_web_search`/`poler_crawl`/`poler_fetch`/
`poler_gmail`/`poler_drive`/`poler_nlm`. Рабочий цикл агента: нарежь
документ `poler_chunk` → найди релевантные куски `poler_search`/
`poler_grep` → процитируй по byte range.

### Артефакты

- `src/retrieval/{mod,grep,chunk}.rs` — 3 файла, ~1700 строк
  (+43 unit-теста: контекст-группировка, exit-коды, бинарность,
  gitignore, unicode-офсеты, breadcrumbs, инвариант точного среза,
  code-fence, перекрытие).
- `docs/native-retrieval-analysis.md` — gap-таблицы «GNU grep × RAG ×
  poler-engine» с решениями по каждой функции.

---

## v0.19.0: Browser Surface — 4 фикса краулера, --browser-index и WebLens (MV3)

Фаза Dogfooding вскрыла UX-болячки веб-конвейера (живой аудит в
FUTURE_ROADMAP §8). v0.19.0 закрывает их и ставит поиск движка прямо
в браузер.

### Фиксы краулера (по материалам живого аудита)

1. **Автодетект Chromium**: кеш playwright
   (`~/.cache/ms-playwright/chromium-*`, `chromium_headless_shell-*`)
   сканируется автоматически, свежая версия побеждает — больше не нужно
   `POLER_CHROME_BIN` на машинах с playwright.
2. **Пер-страничный таймаут** (`--crawl-page-timeout-ms`, по умолчанию
   45 с): JS-тяжёлые сайты с бесконечными XHR/стримингом не вешают обход —
   страница отметится ошибкой с человеческим текстом и подсказкой, обход
   продолжится. Плюс жёсткий бюджет 8 с на выгрузку тел перехваченных
   JSON-API (только завершившиеся ответы — `Network.loadingFinished`).
3. **Robots больше не молчит**: каждый запрет печатается с хостом и путём
   («robots.txt хоста datatracker.ietf.org запрещает /doc/html/rfc8032 —
   страница пропущена, RFC 9309») и попадает в `stats.notes` — не только
   в счётчик.
4. **Самовосстановление CDP**: полумёртвый/осиротевший браузер (kill -9
   родителя, мёртвый рендерер) детектится проверкой `/json/version`
   вместо голого TCP; сессия не открылась → браузер перезапускается,
   обход продолжается.

### --browser-index: «прочитал — индексируй» одной командой

```
poler-engine --browser-index https://example.com/article
# → рендер через CDP → upsert в web-index.db → подсказка про --web-search
```

Явная команда пользователя: robots.txt не блокирует (но честно
фиксируется в notes). Повторный запуск покажет «уже в индексе, контент
не менялся» (Percolator-lite); near-дубликат ловит SimHash.

### WebLens: поиск движка в браузере (Manifest V3)

```
poler-engine --web-lens              # браузерный режим: расширение +
                                     # оконный Chromium c WebLens +
                                     # MCP-демон на 127.0.0.1:8765
poler-engine --web-lens-install     # в свой браузер: файлы + инструкция
```

Расширение **вшито в бинарник** (`include_bytes!`) и материализуется
само — отдельного дистрибутива нет. `--web-lens` поднимает оконный
Chromium с уже загруженным WebLens (`--load-extension` — автоустановка,
ноль кликов в chrome://extensions) и держит тот же MCP over HTTP
(Bearer-токен в `~/.config/poler-engine/weblens-token`, 0600, вписан
в config.json расширения).

Что умеет панель (Alt+P): поиск по **общему** индексу (веб + NotebookLM
+ код + VCS — единый `--web-search`-инвариант), клик по результату,
подсветка термов запроса прямо на странице (TreeWalker + `<mark>`,
без порчи DOM), кнопка «Индексировать эту страницу» (один клик →
страница в индексе; `respect_robots: false` — явная команда человека).
Иконка `icons/` — резонансная дуга движка.

Честные границы: подсветка — совпавшие термы (не «тепловая карта
смысла»), кросс-язычного моста нет (замер в §8.1 роадмапа: 0 результатов).
Киллер-ценность — capture: одна кнопка → страница в общем индексе →
один поиск по всему накопленному.

### MCP-инструмент poler_crawl: новые аргументы

`respect_robots` (bool, по умолчанию true) и `page_timeout_ms`
(по умолчанию 45000) — те же ручки, что и в CLI; расширение передаёт
`respect_robots: false` для кнопки «Индексировать эту страницу».
CORS-preflight MCP-HTTP теперь отдаёт `Access-Control-Allow-Headers:
Authorization, Content-Type, X-Poler-Token` (раньше браузерный fetch
с Bearer падал на preflight).

### Тесты

703 зелёных (+16 к v0.18.0): playwright-скан (свежая версия побеждает,
мусор игнорирует), cdp_healthy против фейковых DevTools-серверов
(валидный JSON/мусор), дедлайн-математика, finished-only интерсепт,
robots-notes, respect_robots=false, материализация WebLens байт-в-байт
+ строгая MV3-валидность манифеста + запрет remote code.

## v0.18.0: License Gate — офлайн-лицензии ed25519 (PO1)

Коммерческая основа движка: платные интеграции (Gmail / Drive / NotebookLM)
закрываются лицензионным гейтом с **офлайн-проверкой ed25519-подписей**.
Без серверов, без телеметрии, без «звонков домой» — математика вместо сети.

### Философия гейта: три принципа

1. **Локальное — свято.** Поиск, резонанс Ψ, AIDDE impact, граф, shell/TUI
   работают ВСЕГДА и БЕЗ лицензии. «Кирпич» невозможен по построению.
2. **Офлайн-честность.** Лицензия = JSON {product, name, email, tier,
   issued, expires, features} + подпись ed25519 мастер-ключом POLER.
   Проверка — микросекунды локально. Подделка без приватного ключа —
   задача дискретного логарифма, а не «поменять байтик в файле».
3. **Мягкие пределы.** Community: интеграции — 50 операций за скользящие
   24 ч (локальное — без лимитов). Trial: 14 дней всех функций с первого
   запуска. Истёкшая лицензия: 7 дней grace с предупреждением, затем
   тихий откат на Community — ничего не блокируется и не удаляется.

### CLI

```
poler-engine --license                        # статус: тир, срок, квоты
poler-engine --license-import PO1.….….       # активация (ключ или путь к файлу)
poler-engine --license-import ./my.key       # проверка подписи ДО сохранения
```

Файл лицензии: `~/.config/poler-engine/license.key` (0600). Для автоматизации:
`POLER_LICENSE_KEY` (ключ строкой) или `POLER_LICENSE_FILE` (путь).
Точки гейта: `--google-gmail`, `--google-drive`, все `--nlm-*`, shell/TUI
`nlm …`, MCP-инструменты `poler_gmail` / `poler_drive` — везде единая
скользящая квота Community.

### Формат ключа PO1

```
PO1.<base64url(payload JSON)>.<base64url(подпись ed25519, 64 байта)>
```

Подпись считается по сырым байтам payload мастер-ключом POLER; в бинарник
вшит только ПУБЛИЧНЫЙ ключ (`src/license/mod.rs::POLER_LICENSE_PUBLIC_KEY_HEX`).
Приватный ключ — офлайн у владельца: выпуск лицензий — `license-tool/`
(отдельный крейт, в поставку не входит):

```
cd license-tool && cargo build --release
./target/release/poler-license-tool keygen --out ~/.config/poler-engine/license-signing-key
./target/release/poler-license-tool issue --key ~/.config/poler-engine/license-signing-key \
    --name "Имя Покупателя" --email buyer@example.com --tier pro --days 365
```

Тиры: `community` (бесплатный), `pro` (годовая), `enterprise`
(бессрочная разрешена). 20 unit-тестов: base64url (все 256 байт, все
выравнивания, Reject стандартного алфавита и паддинга), чужая подпись,
подмена payload после подписи, будущая дата выдачи, бессрочный pro,
grace-семантика, скользящее окно квот, независимость фич, civil-даты.

### Прозрачность Google-авторизации

`--google-auth` теперь печатает человеческим языком, ЧТО получает движок:
Gmail (readonly), Drive (readonly) через OAuth; NotebookLM — НЕ отдельный
OAuth-скоуп, синк идёт через профиль браузера движка (`--auth-ui`),
логин и 2FA остаются между пользователем и Google.

---

## v0.17.6: Auth Companion — интерактивное окно авторизации с изоляцией

Локальный легковесный мост между владельцем и движком: `poler-engine --auth-ui`
поднимает **отдельное окно Chromium с изолированным профилем движка**, в котором
владелец сам вводит логин/пароль и проходит 2FA. Движок (и тем более ИИ-агент)
не видит ни форм ввода, ни хост-браузера — только итоговые куки сессии через
защищённый интерфейс.

```
poler-engine --auth-ui
  └─ spawn: node scripts/auth-companion.js     (zero-dependency, Node ≥ 18)
       ├─ Chromium: userDataDir = ~/.cache/poler-engine/google-profile
       │            (НЕ головной браузер; логин и 2FA — руками владельца)
       ├─ CDP поллинг (127.0.0.1:случайный порт): Storage.getCookies
       │            до полного ядра сессии: SID HSID SSID APISID SAPISID
       ├─ снапшот → ~/.config/poler-engine/google_session.json (0600)
       ├─ статус-сервер 127.0.0.1: GET /status, POST /shutdown
       └─ автозакрытие окна (CDP Browser.close) — куки флэшатся на диск
```

### Гарантии безопасности

* **No Host Snooping** — `~/.config/chromium`, `~/.config/google-chrome` и
  другие браузерные профили хоста не читаются и не пишутся никогда; companion
  отказывается стартовать, если `POLER_GOOGLE_PROFILE` указывает туда.
* **Localhost Only** — статус-сервер и DevTools-порт слушают строго
  `127.0.0.1` (`--remote-debugging-address=127.0.0.1`); окно логина
  запускается БЕЗ `--disable-web-security` и по умолчанию БЕЗ `--no-sandbox`
  (opt-in `POLER_CHROME_NO_SANDBOX=1` — только для контейнеров).
* **Auto-termination** — после подтверждения входа окно закрывается сам
  (`Browser.close` → SIGTERM → SIGKILL по эскалации); Ctrl+C тоже прибирает
  браузер. Висячих процессов и открытых CDP-портов не остаётся.
* **Audit-trail** — `security.auth_companion` в `~/.config/poler-engine/audit.log`
  (только коды/счётчики, без значений кук).

### Файлы и exit-коды

| артефакт | назначение |
|---|---|
| `~/.cache/poler-engine/google-profile/` | изолированный профиль (куки живут тут) |
| `~/.config/poler-engine/google_session.json` | снапшот сессии, 0600 (значения кук — только тут) |
| `~/.config/poler-engine/auth-companion.state.json` | transient-состояние (state/port/счётчик) |
| `scripts/auth-companion.js` | сам companion (self-test: `node scripts/auth-companion.js --self-test`) |
| `dev-stand/` | headless-стенд для контейнеров/песочниц: CDP-релей превью + супервизор + security-audit (`dev-stand/README.md`) |

| код | смысл |
|---|---|
| 0 | авторизация зафиксирована |
| 2 | окно закрыто до завершения входа |
| 3 | таймаут (`POLER_AUTH_TIMEOUT_SECS`, default 600) |
| 4 | preflight: нет Node≥18/Chromium, профиль занят, запрещённый путь |
| 130 | прервано сигналом |

### Контейнеры без дисплея: dev-stand

В песочнице/headless-контейнере окно показать некуда — `dev-stand/` поднимает
companion на Xvfb и транслирует его экран владельцу через CDP-релей в превью
платформы (скринкаст + мышь/клавиатура + кнопки навигации «Назад/Вперёд»).
Модель безопасности и 29 проверок аудита — в `dev-stand/README.md`.

### Почему сессия пишется в google_session.json, а не в google_tokens.json

`google_tokens.json` — строго типизированное OAuth-хранилище
(`access_token`/`refresh_token` для Gmail/Drive, выдаются consent-флоу
`--google-auth`). Браузерная сессия — другой класс креденшелов: компаньон
пишет снапшот в отдельный `google_session.json`, а «синхронизация хранилища
профиля» происходит сама собой — куки уже лежат в изолированном профиле,
который читают `--google-fetch` / `--nlm-*`. OAuth-токены companion выдать не
может (нужен consent-экран Google) — они по-прежнему только через
`poler-engine --google-auth`.

```bash
# окно входа (можно сразу целевой сервис):
poler-engine --auth-ui
POLER_AUTH_TIMEOUT_SECS=900 poler-engine --auth-ui

# после успешного входа:
poler-engine --nlm-account          # проверка сессии NotebookLM
poler-engine --nlm-notebooks        # ноутбуки уже доступны
poler-engine --google-status        # OAuth-токены — отдельная история

# отладка без запуска браузера:
node scripts/auth-companion.js --print-plan   # JSON-план запуска
node scripts/auth-companion.js --self-test    # 19 встроенных тестов
```

Диагностика: если профиль уже занят открытым окном `--google-browse`,
companion откажется стартовать (код 4) — закройте то окно: профиль один.

---

## v0.17.5: Security Hardening — ручной контроль над аккаунтными операциями

Аудит безопасности выявил четыре слабых места в работе движка с аккаунтом
владельца (Google/NotebookLM): молчаливый перенос куков из основного
браузера, отсутствие подтверждений перед write-операциями, «вечно живой»
headless-браузер с открытым CDP-портом и отсутствие журнала действий.
v0.17.5 закрывает все четыре.

### 1. Cookie-import — только по явному согласию

`sync_host_chromium_profile()` больше НЕ тянет куки из `~/.config/chromium`
автоматически при каждом запуске google-браузера. Перенос сессии внешнего
браузера — осознанное действие:

```bash
poler-engine --import-browser-session   # [y/N] + предупреждение + audit-запись
POLER_IMPORT_BROWSER_SESSION=1 …        # скрипты (тоже логируется)
```

Логин своими руками через `--google-browse <URL>` — по-прежнему основной
и рекомендуемый путь (пароль между вами и Google).

### 2. Confirmation Gate для write-операций

- `nlm notes-sync <NB>` — теперь **только pull** (облако → локально).
  Отправка локальных заметок в облако — отдельно, с подтверждением:
  `--dry-run` — план изменений без выполнения; `--yes` — выполнить push.
- TUI: открытие ноутбука синхронизирует только pull; ожидающие отправки
  заметки показываются с подсказкой команды.
- `notes rm <id>` — показывает заголовок заметки и требует `notes rm <id> --yes`.
- Скрипты: env `POLER_YES=1` снимает вопросы (каждое действие всё равно
  попадает в audit-лог).
- Новый модуль `google::confirm`: интерактивный `[y/N]` для CLI,
  двухшаговый паттерн plan→apply для shell/TUI, безопасный отказ по умолчанию.

### 3. Shutdown headless-браузера при выходе

До v0.17.5 `--google-gmail`/`--nlm-*`/OAuth-обмены оставляли headless
Chromium жить неограниченно — CDP-порт 9223 без аутентификации торчал в
системе, и любой локальный процесс мог управлять авторизованной сессией.
Теперь CLI закрывает поднятый им браузер через `Browser.close` (headed-окно
`--google-browse` не трогается — его закрывает владелец).

### 4. JSONL audit-лог

Все обращения к аккаунту фиксируются в `~/.config/poler-engine/audit.log`
(права 0600, best-effort — ошибка лога никогда не ломает операцию):

```json
{"ts":"2026-08-29T13:04:00Z","action":"nlm.create_note","details":"nb=abc note=n-1 title=\"Мысль\""}
{"ts":"2026-08-29T13:05:11Z","action":"gmail.search","details":"q=from:me hits=8"}
```

Логируемые действия: `oauth.auth`, `oauth.gcp_auth`, `gmail.search`,
`drive.list`, `nlm.create_note`, `nlm.chat`, `nlm.notes_sync`,
`security.import_browser_session`. Отключение: `POLER_AUDIT_LOG=off`,
свой путь: `POLER_AUDIT_LOG=/path/to.log`. В details — только
идентификаторы и счётчики, без тел писем/заметок.

```
количество новых тестов: 15 (gate-логика, audit JSONL/0600/env,
  import-гейт по умолчанию OFF, shutdown no-op, dispatch notes rm)
```

---

## v0.17.4: Transcript / Response View — лента чата в окне TUI

Восстановление ключевой функции Ask-вкладки Web GUI (удалён в v0.17.0 —
Next.js весил 1.2 ГБ, а функция нужна): **«Історія чату» + «Відповідь»**
теперь живут в самом TUI как окно-оверлей.

- **Персистентность**: каждая пара `nlm ask` (вопрос, ответ, notebook_id,
  время) автоматически пишется в таблицу `poler_chat` той же SQLite-БД
  (`web-index.db`) — лента переживает перезапуски движка.
- **F3 — Transcript**: лента пар в feed-порядке (новые снизу):
  `#id [дата время] NB вопрос → N символов`. ↑↓/PgUp/PgDn/Home/End —
  навигация, Enter — полный ответ, y — копировать ответ в буфер,
  d — удалить пару, r — обновить, Esc — закрыть.
- **Response View**: вопрос в шапке + полный ответ с прокруткой
  (↑↓/PgUp/PgDn), y — копировать, Esc — назад к ленте.
- **Мышь**: клик по строке ленты открывает ответ.
- Модуль `shell::transcript`: схема `poler_chat`, CRUD, форматирование
  времени без внешних зависимостей (алгоритм Хиннанта), 6 юнит-тестов;
  рендер-смоуки на ratatui TestBackend.

```
poler-engine --tui
F3                    # лента чата: все пары nlm ask
  ↑↓ Enter            # выбрать пару → полный ответ
  y                   # скопировать ответ в буфер обмена
poler> nlm ask <NB_ID> "новый вопрос"   # пара попадёт в ленту автоматически
```


---

## Проблема: почему grep и RAG больше не достаточны

| # | Проблема | Симптом | Механизм POLER |
|---|----------|---------|----------------|
| 1 | **Graph Blindness** | grep находит изолированную строку, модель не понимает, в каком скоупе она находится | Возвращается **полный логический скоуп** (функция/класс целиком, законченная сцена) + **K-hop подграф связей** |
| 2 | **Cosine Collapse / Negation Blindness** | «Система ОБЯЗАНА отключиться» ≈ «Система НЕ ДОЛЖНА отключаться» при cosine > 0.94 | **Exact Lexical Anchors**: фразовый поиск по токенам + маркеры отрицаний с весом 2.0 в ε |
| 3 | **Chunk Fragmentation** | Нарезка по 500 токенов рвёт причинно-следственные связи | **Semantic Boundary Chunking**: границы окон = заголовки сцен / границы функций |
| 4 | **BM25 / TF-IDF Fail** | Редкий токен считается «важным», а суть выражена базовыми словами | Формула информационной плотности **ε** на локальной энтропии |
| 5 | **Temporal Blindness** | Устаревший код смешивается с актуальным, эпохи T-24 и T-0 в одной куче | **Temporal Metric Tagging**: теги `Т-23` на сценах, узлах графа и фильтр `--metric` |

## v0.17.4: MCP over HTTP — удалённый агент в блокнотах владельца без передачи пароля

`--mcp` работал только поверх stdio — то есть для агента, сидящего на той же
машине, что и движок. v0.17.4 добавляет второй транспорт: тот же набор из
семи инструментов (`poler_nlm` в том числе), но по HTTP с Bearer-токеном —
**удалённый агент получает доступ к блокнотам NotebookLM владельца, не
получая ни пароль, ни куки Google**. Движок на машине владельца ходит в
NotebookLM своим персистентным профилем; наружу (через туннель) уходит
только JSON-RPC-ответ по предъявленному токену.

```bash
# 1. на машине владельца (токен напечатается при старте; или задай сам):
poler-engine --mcp-http 127.0.0.1:8765 --mcp-token <секрет>

# 2. публичный туннель без аккаунта (напечатает https://….trycloudflare.com):
cloudflared tunnel --url http://127.0.0.1:8765

# 3. удалённый агент подключается обычным HTTP:
curl -X POST https://<туннель>/mcp \
  -H "Authorization: Bearer <секрет>" \
  -d '{"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"poler_nlm","arguments":{"action":"notebooks"}}}'
```

| Параметр | Значение |
|---|---|
| CLI | `--mcp-http [BIND]` (по умолчанию `127.0.0.1:8765`; можно просто порт `8765`), `--mcp-token <T>` |
| Токен | `--mcp-token` → env `POLER_MCP_TOKEN` → автогенерация (32 hex из `/dev/urandom`) |
| Транспорт | Streamable HTTP: `POST /` и `POST /mcp`, одно сообщение или batch-массив; ответ `application/json` |
| Auth | `Authorization: Bearer <T>` или `X-Poler-Token: <T>`; сравнение за постоянное время |
| Эндпоинты | `GET /health` — smoke-проба туннеля без токена; `GET /mcp` → 405; `OPTIONS` → 204 (CORS-preflight) |
| Реализация | Ручной HTTP/1.1 поверх `std::net` — ноль новых зависимостей; keep-alive, `Expect: 100-continue`, поток на соединение, лимит 16 соединений |

**Модель безопасности**: токен — единственный секрет, который покидает машину
владельца (и то — по приватному каналу в чате/мессенджере). Куки Google
остаются в `~/.cache/poler-engine/google-profile/`, наружу отдаются только
результаты вызовов инструментов. NLM-чат занимает до 90 с — акцептор не
блокируется (каждое соединение — свой поток). Утечка токена = доступ к
инструментам движка (чтение блокнотов, чат), но НЕ к аккаунту Google;
отзыв = Ctrl+C и рестарт с новым токеном.

**Реализация** (`src/mcp_http.rs`, ~700 строк, 20 тестов): рудиментарный
HTTP/1.1-парсер (CRLF/LF-заголовки, Content-Length, лимиты 16 КБ заголовков /
8 МБ тела, slow-loris-защита через idle-таймаут), маршрутизатор запросов,
JSON-RPC-слой поверх общего `McpServer::dispatch()` (выделен из stdio-цикла
`mcp.rs` — поведение `--mcp` не изменено ни на бит), генератор токена с
fallback-PRNG splitmix64, если `/dev/urandom` недоступен. E2E-прогон curl-ом:
401 без токена / с неверным, 200 initialize/tools/list/tools/call, batch
с уведомлением, 202 на чистое уведомление, -32700/-32600/-32601, keep-alive
из двух запросов в одном соединении.

## v0.17.3: Companion Bridge — официальный NotebookLM API рядом с batchexecute

v0.17.1–v0.17.3 соединяют poler-engine с **официальным Pre-GA NotebookLM
Enterprise API** (Discovery Engine `v1alpha`) — не заменяя
реверс-инжиниренный batchexecute-клиент v0.13.0, а достроив **сменный мост**
поверх обоих. Разведка API подтвердила исходную гипотезу: официальный API силён
там, где batchexecute слаб (пакетное создание источников, upload файлов,
аудио-обзоры, удаление), и слаб там, где batchexecute силён (чтение контента,
заметки, артефакты, чат — endpoints отсутствуют или возвращают пустые данные).
Мост маршрутизирует каждую операцию к сильнейшему провайдеру и молча падает
назад при отказе.

### Архитектура HybridProvider (`src/google/companion.rs`, ~2000 строк)

| Компонент | Роль |
|---|---|
| `SourceContentProvider` trait | единый контракт: 13 операций (`Op` enum) для всех провайдеров |
| `GcpEnterpriseProvider` | официальный Pre-GA API: Discovery Engine `v1alpha`, ureq + Bearer (scope `cloud-platform`), 9 операций — M2 ✓ |
| `CdpBatchexecuteProvider` | потребительский протокол v0.13.0: чтение контента, заметки, артефакты, чат |
| `HybridProvider` | routing: primary по `supports(op)`, fallback на `NotSupported`/`NotConfigured` — M3 ✓ |

Routing policy: режим `Auto` (по умолчанию) ведёт GCP-first для 9
enterprise-операций и CDP-first для 4 операций чтения; `GcpOnly`/`CdpOnly`
принудительно фиксируют провайдер (fallback off). Серверные ошибки
(`Http`/`Transport`/`Parse`) **не** переключают провайдера — это разные
данные, а не сбой транспорта.

```
$POLER_GCP_PROJECT_NUMBER   # GCP-проект с включённым Discovery Engine API
$POLER_GCP_REGION           # us | eu | global (default: us)
$POLER_GCP_LOCATION         # (default: global)
$POLER_COMPANION_MODE       # auto | gcp | cdp (default: auto)
```

Milestone-разбивка: **v0.17.1** — M1 skeleton (trait-контракт, 10 URL-билдеров
с `sources:uploadFile` media-конвенцией `/upload/v1alpha/...`, 24 теста);
**v0.17.3** — M2 реальные вызовы (9 операций, refresh-токены из
`oauth::ensure_gcp_fresh`), M3 HybridProvider routing + fallback (8 тестов
routing-политики), M4 TUI Enter-handler. v0.17.2 намеренно пропущен
(reserved).

### M4: Enter на источнике в TUI

Клавиша Enter в панели Sources больше не «ничего не делает» — источник
маппится в `SourceKind` → `EnterAction`:

| Тип источника | Enter-действие |
|---|---|
| `File` | `$EDITOR` на локальном файле (fallback `nano`) |
| `Url` | открыть в браузере пользователя (`xdg-open`) |
| `Repo` | открыть `https://github.com/{value}` в браузере |
| NLM-контент | `FallbackFetch` → `get_source_content` через HybridProvider |

### Горизонт: Zero-Storage Streaming Archives (SA1–SA7)

`docs/future-streaming-archives.md` фиксирует следующий рывок — потоковое
чтение петабайтных архивов (Common Crawl `.tar.zst`, Hugging Face `.zip`)
**через HTTP Range без скачивания на диск**: топологическая адресация
zip central-directory (O(δ) ≈ 64 КБ для 50 ГБ архива), streaming ε + IIR
резонанс, SimHash-дедуп с Bloom-фильтром (m=2²⁰, k=7), importance sampling
батчей `P(d→batch) ∝ exp(λ₁·ψ + λ₂·H − λ₃·Redundancy)` — десятки МБ RAM на
корпуса интернета. Четыре потребителя: обучение локальных LLM, RAG-батчи для
готовых моделей, TUI discovery, параллельный поиск по N архивам (rayon).

---

## v0.17.0: TUI Redesign + Pure-Rust Git Clone & LFS — без системного git

Два релиза в одном: полный редизайн терминального интерфейса в стиле MiMo
Code и закрытие последних заглушек v0.16.0 — `gix clone` и Git LFS теперь
работают на чистом Rust, без системного `git` и `git-lfs` в `$PATH`.

### TUI Redesign (M1–M4, M6)

- **4-панельный дашборд**: Output (главный поток), Notes, Sources, Help —
  переключение фокуса, resize, scroll в каждой панели.
- **Мышь**: клики по панелям, drag-select текста, clipboard через `arboard`
  (копирование выделенного в системный буфер).
- **Notes/Sources CRUD**: заметки и источники живут в `poler-shell.db`
  (`src/notes/mod.rs`, `src/sources/mod.rs`) — создаются, редактируются,
  удаляются прямо из TUI.
- **Help 2.0**: `src/shell/help.rs` — палитра `?` с 11 пресетами сценариев
  (от «первый запрос» до «Pure-Rust git clone + LFS»), детальная справка по
  каждой команде.

### M5: Pure-Rust Git Clone & LFS

`gix clone <URL> <PATH> [--depth N] [--branch B]` — настоящий clone через
`gix::clone::PrepareFetch` (shallow-depth, checkout в worktree), без
вызова системного git. `gix lfs list|fetch <PATH>` — Pure-Rust LFS-клиент:
детект pointer-файлов (`version https://git-lfs/...`), batch-запрос
`POST /objects/batch`, скачивание блобов в `.git/lfs/objects/<oid[:2]>/...`,
авторизация `Bearer $POLER_GIT_TOKEN`.

### Метрики релиза

| Метрика | v0.16.0 | v0.17.0 |
|---|---|---|
| Тесты | 413 | 486 (+24: clone/lfs, notes/sources, help) |
| Бинарник | 8.4 МБ | 12 МБ (+3 МБ: `blocking-network-client` gix) |
| Rust-файлов | 51 | 57 (+notes, sources, help, mouse, clone, lfs) |
| Web GUI (Next.js) | ~1.2 ГБ | удалён (M6) |

---

## v0.16.0: Unified VCS & Data Mesh — нативные адаптеры GitHub/GitLab/Gitea + Pure-Rust git (gix)

Превращение poler-engine из локального инструмента в **Универсальную Сеть Кода и
Данных** — единый пульт, нативно работающий с любыми репозиториями. Каждый VCS
(GitHub, GitLab, Gitea/Forgejo, локальный git через gix) становится
source-адаптером, вливающим коммиты/issues/PR в `web-index.db` как страницы по
своим URL-схемам (`gh://`, `gl://`, `gt://`, `gix://`).

### Архитектурные инварианты v0.16.0 (см. FUTURE_ROADMAP.md §6.4)

**Ноль новых зависимостей в схеме `web-index.db`** — VCS-страницы используют те
же `WebDoc` + `links` + `content_hash` + `positions` + PageRank, что веб и NLM.
URL-схема — единственное отличие. Это сохраняет инвариант v0.14.0: один
`--web-search` пробивает ВСЕ юниверсы (NLM + веб + локальный код + GitHub +
GitLab + Gitea + gix-local) с единой PageRank топологией.

### Структура нового модуля `src/vcs/`

| Файл | Назначение | LOC |
|---|---|---|
| `mod.rs` | VcsAdapter trait, VcsScheme enum, RepoId/VcsCommit/VcsIssue типы, sync_vcs() | ~370 |
| `github.rs` | REST API GitHub v3 (search/repos/commits/issues/PRs) через ureq | ~470 |
| `gitlab.rs` | REST API GitLab v4 (search/projects/commits/issues/MRs) | ~420 |
| `gitea.rs` | REST API Gitea/Forgejo (commits/issues/PRs) | ~370 |
| `local.rs` | Pure-Rust git через `gix` crate: discover/rev_walk/decode | ~440 |
| `ingest.rs` | Helper: VcsCommit/VcsIssue → WebDoc (URL-схема + content_hash + links) | ~220 |

### Новые команды poler-shell

```
poler> gh search <Q>                # GitHub code search (требует $GITHUB_TOKEN)
poler> gh repos <USER>              # список репозиториев пользователя
poler> gh commits <OWNER/REPO>      # последние 20 коммитов
poler> gh issues <OWNER/REPO>       # issues + PRs (REST, не GraphQL)
poler> gl search <Q>                # GitLab REST v4 search
poler> gl commits <GROUP/PROJ>      # коммиты GitLab проекта
poler> gl issues <GROUP/PROJ>        # issues + MR (два endpoint'а слиты)
poler> gt search <Q>                 # Gitea/Forgejo (требует $GITEA_HOST)
poler> gt commits <OWNER/REPO>       # коммиты Gitea
poler> gix log <PATH> [--top N]      # Pure-Rust git log локального репо
poler> gix clone <URL> <PATH>        # заглушка v0.16 (используйте git clone)
poler> sync vcs [gh|gl|gt] <OWNER>   # синк VCS в web-index.db + recompute_pagerank
poler> sync vcs all <OWNER>           # все 4 адаптера сразу
```

Tab-completion для всех новых команд: `gh<Tab>` → search/repos/commits/issues;
`sync vcs <Tab>` → gh/gl/gt/gix/all; `gix <Tab>` → log/clone.

### Переменные окружения

- `$GITHUB_TOKEN` или `$GH_TOKEN` — для `gh search` (анонимно нельзя).
  Опционально для list_repos/commits/issues (rate-limit 60 req/h без токена).
- `$GITLAB_TOKEN` или `$GL_TOKEN` — для GitLab.
- `$GITEA_TOKEN` / `$GT_TOKEN` — для Gitea. `$GITEA_HOST` — обязательный
  (например `gitea.com`, `codeberg.org`, `git.example.com`).
- `$GITHUB_API_HOST` — для GitHub Enterprise (например `github.corp.com/api/v3`).
- `$GITLAB_HOST` — для self-hosted GitLab (`gitlab.corp.org`).
- `$POLER_USER_AGENT` — User-Agent для HTTP-запросов (по умолчанию `poler-engine/0.16`).

### Донорские технологии

| Донор | Что берём | Куда легло |
|---|---|---|
| `gh` CLI (GitHub) | REST+GraphQL API, commits/issues/PR/codeowners | `src/vcs/github.rs` |
| `glab` CLI (GitLab) | REST API v4, merge_requests/pipelines | `src/vcs/gitlab.rs` |
| `tea` CLI (Gitea/Forgejo) | REST API (forgejo-compatible) | `src/vcs/gitea.rs` |
| `gix` crate (gitoxide) | Pure-Rust git: discover/rev_walk/commit decode | `src/vcs/local.rs` |
| `ureq` crate | Синхронный HTTP без tokio-runtime (минимум deps) | `src/vcs/{github,gitlab,gitea}.rs` |

### Аттестация

- 413 unit-тестов зелёные (+119 к v0.15.1: 6 vcs::mod, 21 github, 13 gitlab,
  11 gitea, 22 ingest, 16 local, 15 completer, 15 commands).
- clippy — 0 warning'ов.
- Бинарник `poler-engine` — 8.4 МБ stripped (рост с 5.9 МБ за счёт gix+ureq).
- Smoke-тест: `poler> gix log /home/z/my-project --top 3` → 3 коммита
  прочитаны через Pure-Rust gix (без `git` CLI), индексированы в web-index.db
  как `gix://` страницы, PageRank переcчитан.
- Smoke-тест: `poler> gh search rust` без токена → корректная подсказка
  `$GITHUB_TOKEN`. `poler> sync vcs` без owner → graceful fallback к NLM sync.

### Архитектурный итог

VCS-страницы в `web-index.db` — это обычные `WebDoc` со своим URL-space:
`gh://user/repo/commit/<sha>`, `gl://group/proj/issues/<iid>`,
`gt://owner/repo/pulls/<n>`, `gix:///path/to/repo/commit/<sha>`.
Каждая страница получает `content_hash` (Percolator-lite идемпотентность),
индексируется BM25, ссылается через `links` на свой `web_url`
(`https://github.com/...`), и участвует в общем PageRank графе вместе с
вебом, NLM и локальным кодом. `poler> search "Планковська геодезична"`
пробивает всё сразу.

### Известные ограничения (перенесены в v0.17.0)

- `gix clone` — заглушка v0.16.0; для синхронного clone требуются
  feature-флаги `blocking-network-client` (добавлены в v0.17.0).
  Пока: `git clone URL path` в соседнем окне, затем `poler> gix log path`.
- Git LFS pointer-resolve (`.gitattributes` + `version https://git-lfs/...`)
  — v0.17.0 (см. FUTURE_ROADMAP.md §6.3, шаг 3).
- Hugging Face Hub (model cards + datasets) — v0.21.0 (`hf://` URL-схема).
- DVC + Oxen.ai (data-versioning pointer files) — v0.22.0.
- HugeSCM/Lit/ParamLake — v0.23.0+.

### Артефакты

- `src/vcs/{mod,github,gitlab,gitea,local,ingest}.rs` — 6 файлов, ~2290 строк
  (+119 unit-тестов, ~370 строк тестового кода).
- `src/shell/commands.rs` — +290 строк cmd_gh/cmd_gl/cmd_gt/cmd_gix/cmd_sync +
  16 unit-тестов.
- `src/shell/completer.rs` — +60 строк complete_gh/gl/gt/gix/sync_vcs + 14 тестов.
- `src/shell/state.rs` — +20 строк vcs_subcommands()/gix_subcommands()/vcs_schemes().
- `Cargo.toml` — `ureq 2.10` + `gix 0.66` (default-features=false, features
  `blocking-http-transport-reqwest` + `worktree-mutation` + `revision` + `comfort`).
- `README.md` — секция v0.16.0 (~150 строк).
- `FUTURE_ROADMAP.md` — §6.2 обновлён (v0.16.0 = shipped, v0.17.0 → gix-clone + LFS).

---

## v0.15.1: poler-shell финализация — Tab-completion + нативные crawl/impact в REPL

**Шлифовка полиринга полер-шелла** — подключены Tab-completion, подсказки Hinter
и нативные команды `crawl`/`impact` прямо внутри REPL. Шелл становится монолитным:
все 15+ режимов движка теперь доступны из `poler>` без переключения окон.

### Что починено в v0.15.1

**1. Tab-completion в REPL (rustyline `Helper`):**

`PolerCompleter` теперь зарегистрирован в `Editor::<PolerCompleter, DefaultHistory>::new()`
через `rl.set_helper(Some(PolerCompleter))`. Tab-completion работает для:

* Первого слова команды: `sear` → `search`, `imp` → `impact`, `cra` → `crawl`.
* Подкоманд `nlm`/`set`: `nlm l` → `list`, `set fo` → `format`.
* Флагов `crawl`/`impact`: после `crawl https://example.com ` Tab предлагает
  `--depth/--max/--cross/--delay-ms/--wait-ms/--cdp-port/--help`. После
  value-флага (`--depth`, `--max`, `--delay-ms`, `--wait-ms`, `--cdp-port`)
  completion выключается — ждётся числовое значение, а не другой флаг.

**Hinter** показывает inline-подсказку по набранной команде в серой подсветке
(`search ` → `# search "<query>" [--top N]`), не дожидаясь Tab. **History**
хранит до 2000 команд в `~/.cache/poler-engine/shell-history.txt` с dedup
последовательных дубликатов.

**2. Нативная команда `crawl` в шелле:**

```text
poler> crawl https://rust-lang.org --depth 2 --max 25
poler> crawl https://rust-lang.org --depth 3 --max 50 --cross --delay-ms 500
poler> crawl https://example.com --cdp-port 9223 --wait-ms 1200
```

Полный синтаксис: `crawl <URL> [--depth N] [--max M] [--cross] [--delay-ms N]
[--wait-ms N] [--cdp-port P]`. Делегирует в `poler_engine::web::cdp_fetcher`
+ `poler_engine::web::crawl::crawl` — те же функции, что и в standalone-режиме
`poler-engine --crawl URL`. В шелле есть преимущество: `WebIndex` уже открыт
(если был `search`/`stats`/`nlm sync` ранее), так что crawl сразу льёт страницы
в ту же БД без повторного открытия. Вывод: `fetched`, `indexed`, `unchanged`
(Percolator-lite skip), `duplicates`, `errors`, `sitemap_urls`, `elapsed_ms`.

**3. Нативная команда `impact` в шелле:**

```text
poler> impact ./src crawl --depth 2
poler> impact /home/z/myproject main --depth 3 --cache /tmp/aidde.db
poler> impact /path/to/repo parse_file --depth 2 --max-file-bytes 128MB
```

Делегирует в `poler_engine::collect_files` + `aidde::SymbolTable::build` +
`aidde::impact_analysis` (in-memory по умолчанию) или в `aidde::SymbolStore` +
`aidde::impact_analysis_sqlite` (с `--cache <DB>` для кодовых баз 65K+ файлов).
Выводит target_function, file, lines, danger_level_if_modified, upstream
dependents (кто вызывает этот символ), downstream dependencies (кого вызывает),
side-effects (маркеры unsafe/mutex/static/IO/socket/panic/...).

### Аттестация v0.15.1

* **Unit-тесты**: 294 passed, 0 failed (270 v0.15.0 + 24 новых в v0.15.1:
  14 completion-tests для crawl/impact флагов, 10 cmd_crawl/cmd_impact
  edge-case tests).
* **Clippy**: 0 warnings (`useless_format` и `default_constructed_unit_structs`
  починены автоматически).
* **Бинарь**: 5.9 МБ stripped ELF x86-64 (рост с 5.8 МБ за счёт явного `Helper`
  impl + доп. completion-логики).
* **Smoke-тест**: `echo -e "version\nhelp\nquit" | poler-engine --shell`
  → "poler-shell 0.15.1 — интерактивный режим" + help со списком всех 11 команд
  (search/web/stats/nlm list/notes/artifacts/source/account/ask/sync/crawl/
  impact/set/version/quit/help).
* **Боевой smoke-test**: `poler> impact ./src crawl --depth 2` → построил
  SymbolTable на 45 кодовых файлах движка за <1 с, нашёл `mod::crawl` в
  `src/web/mod.rs`, рассчитал danger_level `HIGH (затронет 6 файлов)`,
  26 upstream dependents (кто вызывает crawl: commands/main/mcp/completer/
  state/crawl_tests), 118 downstream dependencies (кого вызывает crawl:
  derive/parse/insert/clone/discover_sitemaps/...).

### Архитектурные инварианты v0.15.1

* **Ноль изменений в ядре `poler_engine::*`** — shell только заимствует
  `WebIndex`, `cdp_fetcher`, `crawl::crawl`, `collect_files`, `aidde::*`
  и форматирует вывод.
* **PolerCompleter — теперь полноценный `Helper`** (Completer + Hinter +
  Highlighter + Validator) с явным `impl Helper for PolerCompleter {}`.
  В v0.15.0 был `Editor::<(), DefaultHistory>` без completion — это была
  единственная регрессия, теперь закрыта.
* **Ленивое открытие ресурсов сохранено** — `WebIndex` и `NlmSession`
  открываются только при первом `search`/`stats`/`nlm`/`crawl`. Команды
  `help`/`version`/`set` не трогают БД и RPC.

### Артефакты v0.15.1

* `src/shell/commands.rs` (+~340 строк): `cmd_crawl`, `cmd_impact` —
  нативные команды с парсером флагов (`--depth/--max/--cross/--delay-ms/
  --wait-ms/--cdp-port` для crawl; `--depth/--cache/--max-file-bytes`
  для impact) и форматированным выводом.
* `src/shell/completer.rs` (+~110 строк): `complete_crawl_flags`,
  `complete_impact_flags` — Tab-completion флагов с различением
  value-флагов (после них ждём значение, не флаг); явный
  `impl Helper for PolerCompleter {}`; CMD_HINTS расширены crawl/impact.
* `src/shell/commands.rs` `run_shell` обновлён: `Editor` теперь
  типизирован как `Editor<PolerCompleter, DefaultHistory>`, через
  `Configurer` trait выставлены `max_history_size=2000`,
  `history_ignore_dups=true`, `completion_type=List`,
  `auto_add_history=true`.
* `Cargo.toml`: bump 0.15.0 → 0.15.1.

---

## v0.15.0: poler-shell — интерактивный TUI/REPL терминал поверх движка

Когда у движка 15+ режимов (поиск, AIDDE, веб-краулинг, NotebookLM, Google
Drive, фразы, графы, синк), человеку неудобно каждый раз вбивать длинные
флаги `--format md --nlm-chat --top 5` или вспоминать UUID ноутбуков. v0.15.0
добавляет **две поверхности для человека** поверх существующих режимов —
ядро `poler_engine::*` не трогается, только UI-слой.

| Поверхность | Команда | Технология | Что даёт |
|---|---|---|---|
| **REPL** | `poler-engine --shell` | `rustyline` | Быстрый командный режим без перезапуска процесса: `poler> search "..."` / `poler> nlm ask <id> "..."` / `poler> nlm sync`. История `↑/↓` сохраняется в `~/.cache/poler-engine/shell-history.txt`. |
| **TUI Dashboard** | `poler-engine --tui` | `ratatui` + `crossterm` | 3-панельный layout: слева — список 87 ноутбуков (обновление по `r`); справа сверху — поле ввода; справа снизу — выдача с прокруткой `PgUp/PgDn`. `Tab` — смена фокуса, `Esc` — выход. |

**Архитектурные инварианты v0.15.0**:

- **Ноль изменений в ядре** — `poler_engine::*` остаётся как в v0.14.0. Shell
  только заимствует `WebIndex`/`NlmSession`/`nlm_ingest`/`nlm::*` и форматирует
  вывод для человека.
- **Ленивое открытие ресурсов** — `WebIndex` и `NlmSession` открываются
  только при первом использовании (первый `search`/`stats` открывает БД,
  первый `nlm list/notes/...` открывает RPC-сессию). После этого
  переиспользуются до выхода из шелла — экономит ~1 s на каждой команде
  по сравнению с автономным запуском CLI.
- **История команд** — до 2000 записей в `~/.cache/poler-engine/shell-history.txt`.
- **MCP `poler_nlm` 9 actions** из v0.14.0 не тронуты.

**Команды REPL** (палитра для человека):

```bash
poler-engine --shell
poler> help
poler> version
poler> search "Касіопея Astra-Nic Complex" --top 5    # поиск по web-index.db
poler> web "..."                                       # алиас для search
poler> stats                                            # статистика web-index
poler> nlm list                                         # список 87 ноутбуков
poler> nlm notes 704f2610-...                          # заметки/чат (JSON)
poler> nlm artifacts 704f2610-...                       # Studio-артефакты
poler> nlm source 704f2610-... <SRC_ID>                # контент источника
poler> nlm account                                       # email сессии
poler> nlm ask 704f2610-... "Параметры Планковской геодезической"
poler> nlm sync                                          # синк ВСЕХ ноутбуков
poler> nlm sync 704f2610-...                            # синк одного
poler> set format md|json|simple                        # формат вывода
poler> set top 20                                       # топ-K по умолчанию
poler> quit
```

**TUI keybinds** (`poler-engine --tui`):

```text
Tab / BackTab    смена фокуса: notebooks → input → output → notebooks
↑ / ↓           в input: история команд; в notebooks: навигация
'r'             в notebooks: обновить список (nlm list)
PgUp / PgDn     в output: скроллинг результата
Enter           в input: выполнить команду
Esc / Ctrl+C    выход
```

**Аттестация v0.15.0**:

- 270 unit-тестов зелёные (239 из v0.14.0 + 31 новый для `shell::`
  `{state, commands, completer, tui, integration_tests}`):
  - `state::tests`: ленивое открытие `WebIndex`, парсинг `set format`,
    `commands()` и `nlm_subcommands()` стабильные списки;
  - `commands::tests`: `tokenize` (кавычки/пробелы/unclosed), `cmd_set_format`,
    `cmd_unknown`, `cmd_quit`, `cmd_empty`, `cmd_version`, `cmd_help`;
  - `completer::tests`: `complete_prefix("sear")` → search,
    `complete_prefix("nlm l")` → list (но не account, не начинается с 'l'),
    `complete_prefix("set fo")` → format, пустая строка → все команды;
  - `tui::tests`: `Focus::next/prev` цикл (notebooks↔input↔output↔notebooks),
    `Focus::as_str` корректен;
  - `integration_tests`: `tokenize_handles_quoted_args`,
    `state_default_format_is_md_for_humans`,
    `commands_dispatch_unknown_returns_message`.
- `cargo clippy --lib --bin poler-engine` — 0 warning'ов (8 auto-fixed:
  `push_str("\n")` → `push('\n')`, `&[s].to_vec()` → `&[s]`,
  неиспользуемые импорты `RlBuilder`/`KeyEvent`/`Rect`/`tokenize`).
- `cargo build --release --bin poler-engine` — бинарник 5.8 МБ
  (+0.4 МБ к v0.14 за счёт ratatui+crossterm+rustyline; binary stripped),
  `--version` → `0.15.0`.
- Smoke-тест REPL: `echo "version\nhelp\nquit" | poler-engine --shell`
  — приветствие + `version` + `help` (полный список команд) + `quit`,
  всё работает.
- **НЕ подключён** в v0.15.0: Tab-completion в rustyline Editor
  (`PolerCompleter` реализован и покрыт тестами, но rustyline 14 Helper
  trait bound регрессия не даёт подключить его к Editor — v0.15.1 исправит
  через производный Helper derive).
- **НЕ подключены** в v0.15.0: команды `crawl`/`impact` в шелле (заглушки
  с подсказкой использовать `poler-engine --crawl/--impact` в соседнем
  окне) — v0.15.1 добавит нативную интеграцию.

**Что влито в продакшене (по данным v0.14.0)**: после `poler-engine --shell`
владелец может интерактивно: `nlm list` → стрелочкой выбрать UUID →
`nlm sync <id>` → `search "..." --top 5` → `nlm ask <id> "вопрос"` — всё
в одной сессии без повторных RPC-handshake'ов.

## v0.14.0: NLM Corpus Ingestion — `--nlm-sync` и кросс-юниверсный поиск

Виток v0.13.0 выгружает NotebookLM по одному ноутбуку: `--nlm-notes <nb>`,
`--nlm-source <nb> <src>`, `--nlm-artifacts <nb>` — владелец видит данные, но
они остаются в JSON-выводе, **не в общем индексе**. v0.14.0 замыкает круг: все
87 ноутбуков аккаунта (паспорты + источники + заметки + Studio-артефакты)
вливаются в единую базу `web-index.db` — тот же `--web-search` пробивает
**приватный NLM-корпус + локальный код + проползенный веб** одновременно, с
рёбрами `links`, замыкающими граф NLM↔веб (заметка → источник → внешний
Google Docs/YouTube → проползенная страница).

**Доноры из прошлых витков** (ноль новых зависимостей):

| Механизм | Виток | Куда легло в v0.14.0 |
|---|---|---|
| Percolator-lite (content_hash skip) | v0.9 | `content_hash()` FNV-1a 64-hex; повторный `--nlm-sync` skip'ит неизменившиеся страницы за O(1) lookup |
| Positional Inverted Index | v0.11 | фразовые запросы `"..."` ищутся по смежности delta-varint позиций **и в заметках NLM** |
| PageRank | v0.8 | итерации по `links` — заметка → ноутбук-паспорт → источник → внешний URL; `recompute_pagerank(20)` после синка |
| NLM batchexecute-протокол | v0.13 | `NlmSession::list_notebooks/notes/artifacts/load_source` — готовые данные, без нового RPC |
| Chromium-профиль (OAuth 2.0) | v0.12 | одна сессия на все NLM-операции + веб-краулинг |

**URL-схема NLM-страниц** (новый namespace в `web-index.db`):

```text
nlm://notebook/{nb_id}                          — паспорт (title + source-list)
nlm://notebook/{nb_id}/source/{src_id}          — контент источника + URL слайдов
nlm://notebook/{nb_id}/note/{note_id}           — текст заметки/чата
nlm://notebook/{nb_id}/artifact/{art_id}        — Studio-объект (title + kind + status)
```

Внешние URL источников (Google Docs, YouTube) попадают в `links` как обычные
строки — они совпадают с URL проползенных веб-страниц, образуя **сквозной
граф**. PageRank распространяет авторитет через все юниверсы.

```bash
# 0) один раз: залогиниться в профиль движка (как для --nlm-chat из v0.13)
poler-engine --google-browse https://notebook.google.com/

# Синк всех ноутбуков аккаунта в web-index.db (после первого запуска — инкремент)
poler-engine --nlm-sync
# → mode: nlm-sync, notebooks: 87, reindexed: 240, unchanged: 612, errors: 0
#   pagerank iterations: 20

# Синк одного ноутбука (для отладки или точечного обновления)
poler-engine --nlm-sync 704f2610-c02b-4ec1-9fc7-a3b72dde2af1

# После синка — обычный --web-search находит NLM-контент наравне с вебом
poler-engine --web-search '"Касіопея Astra-Nic Complex"' --top 5
# → hit 1: nlm://notebook/704f2610.../note/note-1   (notebook=«Касіопея»)
# → hit 2: nlm://notebook/704f2610.../source/src-text-1
# → hit 3: https://example.com/doc1                  (внешний URL источника)
```

**Парсер `parse_notes`** — толерантен к вариативности Google: формат `cFji9`
в реальном продакшене (см. `upload/NOTEBOOK_704f_ALL_NOTES.json`, 3.7 МБ) —
это `[items_array, metadata_array]`, где каждый item = `[id, [id, text, ?, ?,
title?, ...]]` с **5 или 6 полями во внутреннем массиве**. Эвристика
wrapper-detection (`data[0][0].is_array()` ⇔ обёрнутый формат) различает
`[items, meta]` и bare `items` — парсер остаётся устойчивым к обоим
представлениям.

**MCP**: инструмент `poler_nlm` расширен 9-м action `sync` (теперь 9 actions:
`notebooks | source | notes | artifacts | account | chat | media | shot | sync`).
LLM-агент может триггерить синк без выхода в шелл:

```json
{"method":"tools/call","params":{"name":"poler_nlm",
 "arguments":{"action":"sync"}}}
→ {"mode":"nlm-sync","stats":{"notebooks":87,"reindexed":240,...}}
```

**Аттестация v0.14.0**:

- 239 unit-тестов зелёные (227 из v0.13.0 + 12 новых `nlm_ingest`):
  URL-схема, FNV-1a хеш, `parse_notes` (bare/wrapped/пустой/5-полей/6-полей),
  `ingest_notebook` (паспорт/источник/заметка/артефакт, рёбра, skip по хешу),
  `IngestStats` счётчики;
- `cargo clippy --lib --bin` — 0 warning'ов;
- `cargo build --release --bin poler-engine` — бинарник 5.5 МБ,
  `--version` → `0.14.0`;
- e2e-скрипт `scripts/nlm_sync_test.py` написан (фактический NLM-фейк + 6
  проверок: sync all, Percolator skip, cross-universe web-search находит NLM,
  single sync, MCP action=sync) — требует профильного Chromium в окружении
  запуска (см. `scripts/mcp_nlm_test.py` из v0.13.0 для шаблона).

**Что влито в продакшене (по данным v0.13.0)**: при `--nlm-sync` против
реального аккаунта движок вольёт ~87 паспортов + ~240 источников + ~24
заметок (3.7 МБ) + 10 Studio-артефактов = ~361 страница в `web-index.db` —
первый синк идёт ~3 минуты (RPC на источник), повторный skip'ает 95%+ за
Percolator-lite.

## v0.13.0: NotebookLM без API — протокол batchexecute + медиа-канал

NotebookLM не имеет публичного API, но расширение **NLMTools.com** («NotebookLM
Tools for Gemini») работает внутри авторизованной страницы и говорит на его
внутреннем RPC. Разведка: скачали их Firefox-XPI (это zip), извлекли `inject.js`
и чанки — получили **полный протокол**: 57 RPC-методов `batchexecute`, аргументы,
парсеры ответов, структуру `WIZ_global_data`. Протокол перенесён в Rust (ноль
новых зависимостей) — движок теперь сам делает всё, что умеет NLMTools, **и то,
чего их API не отдаёт** (медиа).

| Донор (NLMTools / NotebookLM) | Что взято | Куда легло |
|---|---|---|
| `inject.js` расширения | карта RPC: `wXbhsf` (ноутбуки), `rLM1Ne` (паспорт), `hizoJc` (контент источника), `cFji9` (заметки), `gArtLc` (Studio), `ZwVcOc` (аккаунт) | `src/google/nlm.rs` |
| `batchexecute` (внутренний RPC Google) | формат `f.req`/`at`/`rpcids`, анти-XSSI-префикс `)]}'`, конверты `wrb.fr`, коды ошибок (8 — квота, 7/16 — авторизация) | `NlmSession::rpc` |
| `WIZ_global_data` | токен `SNlM0e`, app/bl/fsid, email сессии | `NlmSession::open` |
| парсеры `On`/`R` из чанков | ноутбуки/источники/артефакты, enum-типы (YouTube=9, Docs=1…), даты `[сек, наносек]` → ISO-8601 | `parse_notebooks` / `parse_source_content` / `parse_artifacts` |
| **медиа-канал (чего нет в API NLMTools)** | картинки слайдов `l[5][0]`, скачивание через профильный Chromium, скриншоты страниц | `fetch_media` / `screenshot` |

**Два канала — суть комбинации**: текст/доки/чат идут по batchexecute (точно и
структурированно, как «специальный API» NLMTools), а медиа — глазами профильного
Chromium (тот самый `--google-browse`-профиль из v0.12.0: логин один раз, куки
живут месяцами). Модель ноутбука отвечает **по его источникам** — это RAG
владельца, а не общая модель.

```bash
# 0) один раз: залогиниться в профиль движка (те же куки, что для --google-fetch)
poler-engine --google-browse https://notebook.google.com/

poler-engine --nlm-notebooks                # все ноутбуки + источники (id, типы, YouTube-id)
poler-engine --nlm-source <nb> <src>        # текст источника ИЛИ URL картинок слайдов
poler-engine --nlm-notes <nb>               # сохранённые заметки
poler-engine --nlm-artifacts <nb>           # Studio: аудио-обзоры, отчёты, квизы, миндмэпы
poler-engine --nlm-account                  # email/настройки сессии
poler-engine --nlm-chat <nb> "вопрос"       # ответ модели ПО ИСТОЧНИКАМ ноутбука (до 90 с)
poler-engine --nlm-media <URL>              # скачать картинку слайда → ~/.cache/poler-engine/nlm/
poler-engine --nlm-shot <URL>               # скриншот страницы (медиа-глазами юзера) → PNG
```

**MCP**: инструмент `poler_nlm` (итого 7) — LLM-агент получает action-модель:
`notebooks | source | notes | artifacts | account | chat | media | shot`;
ошибки валидации возвращаются `isError` с подсказкой, «не залогинен» — с
инструкцией `--google-browse`.

**Аттестация v0.13.0**:
- 12 unit-тестов протокола: парсеры конвертов/ноутбуков/источников/артефактов,
  varint-даты, анти-XSSI, коды ошибок (квота/авторизация), деградация форматов;
- живой e2e с фейковым NotebookLM (`scripts/mcp_nlm_test.py`): настоящий
  Chromium + MCP-конвейер — 7 инструментов в tools/list; `notebooks` → 2
  ноутбука с источниками и YouTube-id; `source` → текст склеен из кусков **и
  URL картинки слайда отдан**; `media` → байты PNG совпали до байта; `shot` →
  настоящий PNG-скриншот; `chat` → полная UI-автоматизация (ввод вопроса →
  Enter → клик Send → эвристика стабилизации стрима → извлечение ответа);
  валидационные ошибки — `isError` с подсказками;
- против реального notebook.google.com — честная граница: без логина в профиль
  движок отдаёт инструкцию `--google-browse` (сессию не подделываем).

227 unit + 38 integration тестов зелёные, clippy 0.

## v0.12.0: Google-сервисы без пароля — OAuth 2.0 + персистентный профиль

Интеграция с Gmail / Google Drive / NotebookLM **без передачи пароля движку** —
двумя штатными механизмами (так работает «Войти через Google» у всех
приложений):

| Механизм | Сервисы | Как работает |
|---|---|---|
| **OAuth 2.0 loopback** (RFC 8252) | Gmail, Drive (+ любые API: Calendar, Docs…) | Consent-экран открывается в **браузере владельца** — пароль остаётся между человеком и Google. poler-engine получает только узкие readonly-токены (отзыв: myaccount.google.com/permissions) |
| **Персистентный профиль Chromium** | NotebookLM и сервисы без публичного API | `--google-browse` открывает окно с профилем `~/.cache/poler-engine/google-profile` — владелец логинится **один раз своими руками**, куки живут месяцами; `--google-fetch` читает авторизованный контент headless-ом |

**HTTPS-клиент — сам Chromium** (ноль TLS-зависимостей в Rust): `GoogleHttp`
выполняет `fetch()` в контексте страницы через CDP `Runtime.evaluate` +
`awaitPromise`; google-браузер живёт на отдельном порту 9223 с флагом
`--disable-web-security` (это API-профиль, не stealth-краулер) и общим
`--user-data-dir` для headless/headed режимов.

```bash
# 1) свой OAuth-клиент (5 минут, бесплатно — см. ниже) и одноразовое согласие
poler-engine --google-auth                       # consent в твоём браузере

# 2) почта и диск — нативный синтаксис Gmail
poler-engine --google-gmail "from:me has:attachment newer_than:7d"
poler-engine --google-gmail                      # недавняя почта
poler-engine --google-drive "отчёт"              # файлы по имени
poler-engine --google-status                     # скоупы/срок/email

# 3) сервисы без API (NotebookLM): логин один раз своими руками
poler-engine --google-browse https://notebook.google.com/
poler-engine --google-fetch https://notebook.google.com/notebook/<id>
```

Своё OAuth-приложение (client_secret.json): console.cloud.google.com →
проект → включить Gmail API + Drive API → OAuth consent screen (External,
себя в Test users) → Credentials → OAuth client ID (Desktop app) → скачать
JSON в `~/.config/poler-engine/client_secret.json`. Токены:
`~/.config/poler-engine/google_tokens.json` (права 0600), refresh — тихо и
автоматически; access-токен живёт ~1 час.

**MCP**: инструменты `poler_gmail` и `poler_drive` (итого 6) — любой
LLM-агент читает почту/диск владельца через те же readonly-токены.

**Аттестация v0.12.0** (живые тесты):
- мост CDP→HTTPS против **реального Google**: token endpoint отклоняет
  мусорный обмен (401 `invalid_client`), Gmail/Drive API отклоняют фейковый
  Bearer (401) — TLS/POST/заголовки/статусы проходят честно;
- полный E2E на фейковых эндпоинтах (6 шагов): consent-URL → 302 →
  loopback-ловушка (state проверен, чужой state отбрасывается) → обмен кода
  через браузер → токены 0600 → **протухание → тихий refresh → Gmail-запрос
  идёт с обновлённым токеном** (фейк-API принимает только его) → Drive →
  status с email → `--google-fetch` → `--google-browse` честно требует
  оконный Chromium;
- MCP: tools/list отдаёт 6 инструментов, poler_gmail/poler_drive отвечают
  живыми данными через refresh-токен.

215 unit + 38 integration тестов зелёные, clippy 0.

## v0.11.0: Фразовый поиск — позиционный индекс в веб-поиске

Виток «доработки хренового»: веб-индекс был «мешком слов» — запрос
`"Rust async runtime"` находил страницу, где `Rust` в первом абзаце, а
`runtime` в футере через 5000 слов. Теперь порядок токенов сохраняется и
проверяется по смежности позиций — семантика точных цитат Google.

**Что внутри** (донорские технологии — Lucene `.prx` / Tantivy / Google
exact-quotes):

| Компонент | Откуда украдено | Что делает |
|---|---|---|
| `positions BLOB` в postings | Lucene `.prx` (positional index) | дельта-varint-позиции токенов рядом с `(term, page_id, tf)` — ~1–2 байта на вхождение |
| `phrase_occurrences()` | Lucene `PhraseScorer` | вхождение фразы в позиции `p` ⇔ каждый терм в `p+i`; бинарный поиск по отсортированным спискам |
| `parse_query()` | Google-синтаксис `"..."` | сегменты в кавычках `"..."` и `«...»` → фразы (стеммингуются!); однотокенная «фраза» деградирует до терма |
| Proximity-бонус | Lucene `phraseFreq` | `0.5 · Σidf(термов) · min(occ, 8)` добавляется к BM25 за каждое вхождение |
| TITLE_GAP=8 | — | фраза не сшивает последнее слово тела с первым словом заголовка |
| Миграция v1→v2 | — | старые БД v0.9/v0.10 открываются: `ALTER TABLE` + пересчёт позиций из сохранённого text (content_hash не трогается — Percolator-lite не пострадает) |

**Семантика**: документ обязан содержать КАЖДУЮ фразу запроса целиком
(жёсткий фильтр, как точные цитаты Google); свободные термы за кавычками
ранжируют как раньше. `phrase_occ` в JSON/Md-выдаче показывает число вхождений.

```bash
# фраза из живой страницы std::mem::swap (свежий краул v2):
$ poler-engine --web-search '"swaps the values"' --format md
## 1. swap in std::mem - Rust
- Score: 0.8000 (bm25=1.402, pagerank=0.15000, title=0.00, ε=0.02484, фраз=·1)
> …pub const fn swap<T>(x: &mut T, y: &mut T) Swaps the values at two mutable…

# переставленные слова — честный ноль:
$ poler-engine --web-search '"values the swaps"'; echo $?
1

# те же слова без кавычек — прежняя OR-семантика (3 хита вместо 1)
$ poler-engine --web-search 'values swaps the'
```

**Живая аттестация**: старая БД эпохи v0.9 (8 страниц Rust std, 4532
postings, БЕЗ колонки positions) мигрирована при открытии: все постинги
получили позиции, `"list of all items"` → ровно 1 хит (страница «List of
all items in this crate»), без кавычек — 3 хита. Кириллица: `«владения
память»` находит «владение памятью» (стемминг + смежность).

Тесты: **189 unit + 38 integration** (+23 к v0.10.0), clippy 0.

## v0.10.0: MCP-сервер — poler-engine как нативный инструмент LLM-агентов

v0.9.0 дал движку веб-поиск; v0.10.0 отдаёт его **любому LLM-агенту напрямую**:
`poler-engine --mcp` поднимает MCP-сервер (Model Context Protocol, stdio
JSON-RPC 2.0, ноль новых зависимостей). Claude Desktop, Cursor, Cline, Zed и
любой MCP-клиент получает четыре инструмента — и **агент сам решает, какие
сайты читать и обходить**: ни цель, ни тема не фиксированы.

| Инструмент | Что делает |
|---|---|
| `poler_web_search` | Поиск по постоянному индексу: WebRank, сниппеты, кириллический стемминг |
| `poler_crawl` | Обход выбранного агентом сайта в постоянную БД (robots/sitemap/SimHash/PageRank) |
| `poler_fetch` | «Прочитать любой URL сейчас»: реальный Chromium, SPA/JS рендер, перехват скрытых JSON API |
| `poler_search` | Локальный резонансный POLER-поиск: ε/R, полные сцены, K-hop граф |

```json
// claude_desktop_config.json / mcpServers:
{ "poler-engine": { "command": "/home/user/.local/bin/poler-engine", "args": ["--mcp"] } }
```

### Доработка «хреновых» частей v0.9.0 (по итогам полевого анализа романа)

| Проблема (живой баг) | Фикс v0.10.0 |
|---|---|
| **Морфологическая слепота**: «ініціац» → 0 хитов (в тексте «ініціація», «ініціації») | `web/stem.rs` — лёгкий кириллический стеммер (uk/рос): ~60 окончаний, защита коротких слов, 2 прохода. Индексация и запрос — единый путь. Живая проверка: запрос «мов**и** програмуванн**я**» находит «мов**а** програмуванн**я**» (score 0.9) |
| Агент должен сам поднимать браузер | `web::ensure_chromium` — автозапуск: $POLER_CHROME_BIN → PATH → bundle-путь; CLI `--web`/`--crawl` и MCP-инструменты поднимают Chromium молча |
| `HeadlessChrome` в User-Agent выдаёт автоматизацию | CDP-стелс: десктопный UA + `navigator.webdriver→undefined` + `--disable-blink-features=AutomationControlled` (техника puppeteer-extra-stealth). Живая проверка: httpbin.org/user-agent видит обычный Chrome/152 |
| SimHash-отпечатки считались от поверхностных форм | Единый стемминг-путь: падежный шум уходит из отпечатка — near-дубли ловятся надёжнее |

Честная граница: **аутентификационные стены не обходятся** — логины, платный
контент и приватные ноутбуки возвращают то, что видит анонимный браузер
(notebook.google.com рендерит оболочку NotebookLM; контент требует Google-логина).
Технические барьеры (SPA/JS/бот-фильтры) обходятся рендером реального Chromium.

### Живые полевые тесты MCP (реальные сайты, Chromium 152)

| Вызов | Результат |
|---|---|
| `initialize` + `tools/list` | serverInfo v0.10.0, 4 инструмента с inputSchema |
| `poler_fetch example.com` | Chromium **поднялся сам**, текст 129 Б, кэш-файл записан |
| `poler_crawl uk.wikipedia.org/wiki/Rust` (depth 0) | 1 стр, 7.7 с, автозапуск повторно |
| `poler_web_search «мови програмування»` | **Стемминг работает**: склонённый запрос → назывной заголовок, score 0.9 |
| `poler_search fixtures/example.rs «main»` | R=585.0, ε=585.0 через MCP |
| `poler_fetch httpbin.org/user-agent` | UA = `Chrome/152.0.7977.54` (не Headless), JSON API перехвачен |
| litnet.com (прямой рендер) | Каталог книг читается полностью + перехвачен внутренний JSON API `genres_for_sidebar` (54 КБ) |

## v0.9.0: Web Search for AI — краулер + BM25/PageRank-индекс + `--web-search`

Полноценный веб-поиск: v0.8.0 умел **рендерить** страницу, v0.9.0 умеет
**обходить сайты, строить индекс и отвечать на запросы**. Архитектура собрана
из проверенных боевых технологий (Google → open source → POLER-модель:
те же математические инварианты, вертикальный масштаб вместо 10 000 серверов):

| Технология-донор | Откуда | Реализация в POLER |
|---|---|---|
| Googlebot (миллион headless Chromium) | Google | `cdp.rs` — один Chromium через CDP (v0.8.0) |
| robots.txt + Sitemap | стандарт вежливости Googlebot | `robots.rs` — парсер групп UA, `Crawl-delay`, `Sitemap:`, `Allow`/`Disallow` |
| URL Frontier + Politeness | Mercator/Heritrix (краулер, выкормивший Google-конкурентов) | `crawl.rs` — BFS-граница, доменная задержка, cap на хост |
| SimHash-дедупликация | статья Google «Detecting Near-Duplicates for Web Crawling» (Manku et al., 2007) | `simhash.rs` — 64-битные отпечатки, расстояние Хэмминга ≤ 3 |
| Инвертированный индекс | Tantivy/Lucene (открытые наследники Google Index) | `index.rs` — SQLite: `pages/terms/links/hosts/meta` |
| BM25 (Okapi) | до-нейронный Google | классический BM25 (k1=1.2, b=0.75) + idf-фильтр стоп-слов |
| PageRank | статья Brin & Page 1998 | итеративный по `links`-таблице, ε-телепорт |
| Percolator (инкрементальный индекс) | Google (Colossus-стек) | content-hash skip: повторный краул переиндексирует ТОЛЬКО изменившееся |
| Scatter-Gather Top-K | Google Serving | postings scatter → аккумулятор gather → нормированный WebRank |

**Ранжирование — POLER WebRank v1**:
`0.55·BM25 + 0.15·PageRank + 0.20·title-match + 0.10·ε-плотность` —
лексическая точность BM25, ссылочная авторитетность, точность в заголовке
и POLER-мера информационной плотности в одной формуле.

```bash
# 1. Краулинг сайта (Chromium рендерит каждую страницу, robots/sitemap соблюдаются):
poler-engine "https://nginx.org/en/docs/" --crawl --crawl-depth 2 --crawl-max 50

# 2. Поиск по собранному индексу (AI-ready JSON со сниппетами):
poler-engine --web-search "gzip static" --top 5

# Индекс: $POLER_WEB_DB или ~/.local/share/poler-engine/web-index.db
```

### Полевые замеры (Chromium 152, реальные сайты)

| Сайт | Загружено | Проиндексировано | Особенности |
|---|---|---|---|
| doc.rust-lang.org/std/mem | 10 стр, 16 с | 10 | frontier нашёл ещё 230 URL |
| en.wikipedia.org/wiki/Rust | 8 стр, 43 с | 8 | кросс-доменный поиск «ownership borrow checker»: Википедия #1 |
| nginx.org/en/docs | 8 стр, 17 с | 5 | **3 SimHash-дубля поймано** (зеркала www/http), **200 URL из sitemap** |
| повторный краул Википедии | 5 стр, 27 с | **0** (5 unchanged) | Percolator-lite: content-hash skip работает |

Найден и закрыт живой баг релевантности: на **моно-тематическом корпусе**
(весь сайт про nginx) предметный терм запроса встречается на каждой странице
→ idf-фильтр стоп-слов убивал его → «gzip» давал 0 результатов. Теперь при
пустом после фильтра запросе термы откатываются к полному набору (регрессионный
тест `monothematic_corpus_subject_term_not_stopped_out`).

## v0.8.0: Web-Native Retrieval — нативный Chromium CDP

Замена «костыльной» связке Rust → Node.js → CLI → Chromium из
super-z-skills (`agent-browser --cdp 9222`): **прямой CDP-клиент на чистом
std** (`src/web/cdp.rs`, ~430 строк) — WebSocket RFC 6455 поверх
`TcpStream`, ноль новых зависимостей.

```bash
# Chromium headless (chrome-headless-shell, без X11/KDE):
chrome-headless-shell --headless --remote-debugging-port=9222 --no-sandbox &

poler-engine --web "https://en.wikipedia.org/wiki/Rust_(programming_language)" \
    -q "ownership" --web-wait-ms 2500
```

| Возможность | Как реализовано |
|---|---|
| **Bypass SPA/Shadow DOM** | браузер исполняет весь JS; `Runtime.evaluate(document.body.innerText)` — текст, который видит человек |
| **Перехват скрытых API** | `Network.responseReceived` (mimeType=application/json) + `Network.getResponseBody` — сырой JSON до превращения в HTML, до 64 ответов |
| **Cross-Universe Graph** | рендер-текст и JSON попадают в веб-кэш → общий K-hop граф с локальным репозиторием |
| **Фильтрация шума** | реклама/меню/футеры отсеиваются сами: у шаблонного мусора низкая ε-плотность относительно запроса |

### Полевые замеры (Chromium 152 headless-shell, реальные страницы)

| Страница | Рендер-текст | Результат |
|---|---|---|
| example.com | 129 Б | 3 хита, 4 мс |
| doc.rust-lang.org/std/mem/fn.swap.html | 1 001 Б | 8 хитов «swap», ε/R посчитаны |
| en.wikipedia.org/wiki/Rust_(…) | **73 686 Б**, 11.5K токенов | 9 хитов «ownership», сцена 16 КБ с FFI-контентом, граф 148 узлов |
| httpbin.org/json | — | **1 JSON API перехвачен** (pretty-printed в кэше) |
| Википедия mmap + локальный allocator.c | — | **Cross-Universe**: 24 хита из веба и кода в одном K-hop графе |

CLI: `--web` (PATH трактуется как URL), `--cdp-port` (default 9222),
`--web-wait-ms` (пауза на дочерние XHR, default 1200).

## v0.7.0: стриминговый Top-K + кэш локатора сцен

Литературный стресс на LM1B (4 файла, 28 млн токенов, запрос «the» =
**1 667 291 совпадений**) вскрыл три уровня деградации и закрыл их:

| Проблема | Исправление | Эффект |
|---|---|---|
| Аллокация `Vec<String>` окна (80 строк) на каждый хит | `calculate_epsilon(&[&str])` — zero-alloc срезы токенов | −480 млн аллокаций |
| `SceneInfo` (тройки) строилась на каждый хит до отсева | **стриминговая куча top-N** в pass2 (обычный и гигантский пути): лёгкие записи → куча → тяжёлые сцены только для выживших; per-chunk top-N ∪ глобальный top-N (IIR в чанках независим — корректность доказуема) | память O(top_n), не O(hits) |
| `locate` пересканировал заголовки всего файла на каждом хите (LM1B: 70 ГБ сканирования на чанк); поиск абзаца без ограничения окна — терабайты на корпусах без пустых строк | **`SceneLocator`** (заголовки один раз, бинарный поиск) + окно абзаца 64 КБ с выравниванием UTF-8 | «the»: 8+ мин (смерть процесса) → **53.8 с** |

### Итоги литературного стресса (LM1B, 28 млн токенов, 2 vCPU)

| Запрос | Хиты | Время | Пик RSS |
|---|---|---|---|
| «the» (суперчастотный) | 1 667 291 | **53.8 с** | 420 МБ |
| «president» | 25 950 | 13.5 с | — |
| «United States» (фраза) | — | 13.0 с | — |

Честные границы: temporal-счёт при `--metric` приближён по top-N записям
(полный счёт требует light_meta на каждую сцену — дорого на
суперчастотных запросах); сцены гигантов после стриминга строятся только
для top-N якорей (граф сущностей massive-hit запросов сужается до
выживших сцен).

## v0.6.1: кэш reuse + фикс токенизации запроса

| Исправление | Суть | Эффект |
|---|---|---|
| `--impact-reuse` | существующая `--impact-cache` база не перестраивается | повторный impact-запрос на подъядре: **39 000 мс → 30 мс (1300×)** |
| Токенизация запроса = токенизация текста | запрос разбивается по не-буквам как текст («слайд-шоу» → [слайд, шоу]) | дефисные/пунктуационные запросы находились grep'ом, но не движком: «слайд-шоу» **0 → 69 хитов**; «Цинк-4», «Тёмное Сердце» работают как фразы |

Найдено прогоном хаотичного ассоциативного запроса по Eteryya (многослойная
декомпозиция): «кальций»→Chapters_Sfera_Predela (потеря кальция, крошащиеся
зубы), «Сейф-Био»→канон EPUB-00, «Тёмное Сердце»→EPUB-04 (пик R=124570),
«Цинк-4»→chapter_p01of25 (Т-22). Честные ограничения: точный поиск не
нормализует ё/е («крио-шёлк» ≠ «крио-шелк»); «аксональное» в корпусе
отсутствует физически (0 файлов по grep).

## v0.6.0: пять багфиксов полевой аттестации + SQLite AIDDE

### Исправления по отчёту полевых стресс-тестов (Wireshark/PYCCLE)

| # | Баг | Исправление | Эффект |
|---|---|---|---|
| 1 | Квадратичный поиск по noise_spans в AIDDE (O(M×N) на файл) | Бинарный поиск по отсортированным спанам O(log K) | убраны десятки миллионов итераций на файл Wireshark |
| 2 | Однопоточный SymbolTable::build | rayon par_iter + слияние в порядке файлов (детерминизм) | оба ядра, ~2× |
| 3 | Память на многофайловых корпусах с частым словом (PYCCLE «king»: 500K+ HitRecord → 1.4 ГБ) | **Early Top-K Pruning**: per-file только top_n якорей + компактные hit_keys (8 байт/хит) + hits_temporal для честного total_hits; осиротевшие сцены удаляются | Eteryya: 168→138 МБ; корректность: глобальный top-N ⊆ ∪ per-file top-N |
| 4 | Линейный скан table.calls на каждом шаге BFS в impact | Индексы HashMap по callee/caller, O(1) lookup на уровень | AIDDE без квадратичности |
| 5 | OOM ин-мемори AIDDE на 65K файлах (~5 млн вызовов = 5–8 ГБ) | **SQLite SymbolStore** (`--impact-cache path.db`): потоковая запись чанками из rayon-воркеров (пик RAM = чанк), нормализация путей (files id/path — сжатие в ~2.5×), B-Tree индексы по callee/caller, BFS только по нужным строкам | **полное ядро Linux: 61 092 файла, 1.7 млн defs + 6.7 млн вызовов, RSS ~100 МБ** (было: OOM-kill при 4 ГБ) |

### Результаты полного AIDDE на Linux Kernel через SQLite

```bash
poler-engine ~/linux-6.12.35 --impact printk --impact-cache /tmp/sym.db
# 61 092 файла | defs=1 712 453 | calls=6 683 592
# RSS ≈ 100 МБ (ин-мемори версия умирала от OOM)
# printk → CRITICAL (44 файла), 200 upstream
```

## v0.5.0: канонический POLER-цикл из P3_Engine + экзамен на Linux Kernel

### Канонический POLER-цикл (p3_poler.zig → poler-engine)

Математика взята из **серьёзного** репозитория P3_Engine (Zig, 36K строк),
а не из ранних набросков POLER-Quantum. Каноническое уравнение:

```text
p_new = p − η · Π_Λ(D·p + γ·J·p + ∇F)
D = L·Lᵀ   — диссипатор: энтропийный горел, сжигает внимание без наблюдений
J = A − Aᵀ — резонанс: кососимметричный генератор вращения (частота
             восстанавливается из осцилляций потока ε, не из памяти)
Π_Λ         — каузальный проектор (temporal-фильтр: чужие эпохи запрещают сдвиг)
CORDIC      — ренормализация на S¹ с параметром mix (битовая магия + 3 итерации)
```

```bash
poler-engine ~/code -q "foo" --resonance-mode poler \
    --poler-eta 0.01 --poler-gamma 0.1 --poler-mix 0.1 --poler-dissipator 0.02
```

Реализация (`src/poler.rs`): честная 2D-редукция с настоящими матрицами —
`D = d²·I` (изотропное затухание), `J = [[0,−ω],[ω,0]]`, `Π_Λ = I − Jcᵀ(JcJcᵀ+δI)⁻¹Jc`
(идемпотентность доказана тестом), CORDIC `1/√x` (точность ~1e-11,
тест на 5 порядках). Дефолты η=0.01, γ=0.1, mix=0.1, δ=1e-10 — из
`P3Node.init` / `PolerEngine.initDefault` p3_poler.zig.

Принципиальное отличие от Ψ-версии: диссипатор **гарантирует** затухание
без наблюдений (D=LLᵀ ≥ 0 по построению), резонанс **вращает** фазу,
а не накапливает историю. Тест `poler_dissipator_distinguishes_sparse_and_dense_hits`
доказывает: плотная серия упоминаний даёт больший POLER-резонанс, чем
разовая вспышка.

### Экзамен на Linux Kernel 6.12 (1.2 ГБ, 85K файлов)

Полный стресс-тест на исходниках ядра Linux — см. секцию «Производительность».

## v0.4.0: POLER[Ψ] + параллельные гиганты + PII-архитектура

### Интеграция канонической математики POLER[Ψ]

Математика взята из репозиториев POLER-Quantum / poler-dynamics /
dynamis-v1 (Kotokvit) и встроена как третий режим резонанса:

```bash
poler-engine ~/Eteryya -q "адамантит" --resonance-mode psi \
    --psi-eta 0.05 --psi-gamma 0.5 --psi-rho 0.9 --psi-depth 8
```

| Формализм POLER[Ψ] | Реализация в движке |
|---|---|
| `Ω(o_t) = tanh(o_t)` — перцепция | наблюдение = ε-плотность окна совпадения |
| `ε = κ Δxᵀ G(p) Δx` — энергия значимости | `calculate_epsilon` = κ·Σ(ln N − ln freq)² — квадратичная форма с диагональной метрикой редкости |
| `R[n] = ρᵏ·s_{t−k}` — резонанс памяти | замкнутая форма = IIR `R_t = ε_t + ρ·R_{t−1}` (доказано тестом `iir_is_degenerate_case_of_psi`) |
| `Π_Λ` — проектор логики | temporal-фильтр: наблюдения чужих эпох не сдвигают внимание |
| `p_{t+1} = p_t + ηΠ(−∇F + γ∇ε)` — ψ-поток | `src/psi.rs`: точный порт `PsiField.evolve` с параметрами по умолчанию из POLER_Psi_v3.py |

**Найдено при интеграции (честная находка о исходной математике):**
условие устойчивости ψ-поля `γ·Σρᵏ < 2`. Дефолтные параметры POLER_Psi_v3
(γ=0.5, ρ=0.9, K=8: β = +0.85) дают расходимость на длинных сериях —
в коротких Python-демо она не успевает проявиться. Стабилизация:
внимание ограничено перцептивным пространством Ω = tanh ∈ (−1, 1).

### Параллельная обработка гигантов

Файлы ≥ 8 МБ режутся на чанки ~2 МБ по **границам сцен** (заголовки
markdown / границы абзацев, выравнивание по UTF-8 и строкам) и
обрабатываются в rayon-пуле параллельно. Enclosing scope не рвётся:
граница чанка — единственный безопасный разрез. Память ограничена
`~2 МБ × потоки`. Тест эквивалентности: гигант 9 МБ даёт те же сцены,
что и последовательная обработка.

### PII-маскирование перенесено на выход (3× ускорение)

Профилирование боевого прогона Eteryya вскрыло: PII-регексы по всему
корпусу (92 МБ) стоили **65% времени** (5.2 с без PII vs 15.4 с с PII).
Архитектурное исправление: токенизация и индексация работают на raw-тексте,
маскирование применяется **только при материализации выходных сцен**
(enclosing_scope + метаданные) — ровно там, где текст получает
AI-потребитель. Позиции внутри конвейера остаются raw-точными.

### Итоги боевого прогона (2 vCPU)

| Метрика | v0.3.1 | v0.4.0 |
|---|---|---|
| Eteryya полный скан (92 МБ, 8.5 млн токенов) | 15–18 с | **5.5–6.0 с (3×)** |
| Пиковая RSS | 171 МБ | **150–168 МБ** |
| Ψ-режим на Eteryya | — | работает (5.2 с) |

## v0.3.1: полевая аттестация на реальных корпусах

### Исправлен критический боевой баг (OOM на Eteryya)

При прогоне на реальном репозитории Eteryya (148 МБ, 305 md-файлов,
8.5 млн токенов) движок погибал от OOM-killer (exit 137, пик 3.6 ГБ).
Бисекция привела к файлу с **строкой 1.7 МБ без переносов**, содержащей
«- Персонажи:» в глубине текста с 8763 запятыми: `light_meta` забирал
остаток мегабайтной строки как значение поля → 9315 «субъектов» → каскад
троек → OOM. Исправлено многоуровневыми потолками:

* карточка метаданных ищется только в первых 8 КБ сцены;
* строка-кандидат обрезается до 512 байт, значение поля — до 256 байт;
* не более 16 субъектов по 80 байт;
* не более 256 троек на сцену; enclosing_scope — жёсткий потолок 1 МБ.

### Результаты полевой аттестации (2 vCPU)

| Корпус | Объём | Запрос | Результат |
|---|---|---|---|
| **Eteryya** (реальный) | 360 файлов, 92 МБ md, 8.5 млн токенов | «адамантит» | 358 хитов, **171 МБ RSS, 15 с** (было: OOM) |
| Eteryya, фразовый | то же | «не должна» | **194 МБ, 16 с** |
| Eteryya, watcher rescan | то же | «адамантит» | инкремент: **277 мс** (54× быстрее полного) |
| **tokio** (реальный код) | 836 файлов, 678K токенов | «spawn_blocking» | 282 хита, **12 МБ, 0.4 с** |
| tokio + AIDDE | то же | `--impact spawn_blocking` | **CRITICAL: 79 файлов, 200 upstream**, 21 МБ, 1.6 с |

Edge-батарея (12 кейсов): пустой файл, бинарник с .md, битый UTF-8,
BOM+CRLF, строка 100 КБ, чистая пунктуация, emoji, директория с
расширением .md, вложенность 30 уровней, symlink-цикл, .gitignore,
файл без прав на чтение — все без паник и зависаний.

### Дифф-режим watcher

```bash
poler-engine ~/Eteryya -q "адамантит" --watch --diff --interval-secs 2
```

`--diff` (с `--watch`): инкрементальные прогоны печатают только якоря,
которых не было в предыдущем прогоне (новые file+byte_pos) — поток
событий для агента вместо повторения всего топа.

## v0.3.0: потоковая архитектура памяти + AIDDE

### Исправление критического бага памяти (bug#1, v0.2.1)

Прежняя схема удерживала инвертированные индексы **всех** файлов одновременно
(`Vec<FileScan>`) — на корпусе 340 файлов / 12 млн токенов RSS раздувался до
гигабайт. Новая схема **никогда не удерживает индексы между фазами**:

```text
Проход 1 (rayon):  mmap → литеральный предфильтр (kwset-техника GNU grep)
                    ├─ литерала нет → streaming counts (без построения индекса)
                    └─ литерал есть → временный FileTokens (zero-copy &str-срезы)
                         → глобальные частоты + позиции хитов → освобождение
Проход 2 (rayon):  только hit-файлы → ε/R + лёгкая локализация сцен + тройки
Проход 3:          материализация текстов сцен только для top-N
```

Замеры на стресс-корпусе (340 файлов, 91 МБ, ~11.5 млн токенов — масштаб
репозитория Eteryya), 2 vCPU:

| Метрика | v0.2.1 | v0.3.0 |
|---|---|---|
| Пиковая RSS (VmHWM) | 431 МБ | **99 МБ (−77%)** |
| Время полного прогона | 53 с | 74 с (+40% — цена потоковой схемы) |

Память теперь ограничена `O(словарь корпуса)` + один временный индекс файла;
гигантские файлы (≥ 8 МБ, наподобие дампов `Прочее_FULL`) обрабатываются
строго последовательно. Межпроходный текстовый кэш удалён полностью.

### AIDDE: AI-Interpreted Dependency & Impact Engine

Ответ на слепоту grep/RAG и «близорукость» линейного интерпретатора
(Single-Fault Blindness). Вместо вопроса «компилируется ли?» — вопрос
«что сломается, если это изменить?»:

```bash
poler-engine ./src --impact scan_path_with_stats
```

```json
{
  "target_function": "lib::scan_path_with_stats",
  "file": "./src/lib.rs",
  "lines": "1-291",
  "upstream_dependents": [
    {"caller": "lib::scan_path", "file": "./src/lib.rs", "line": 244}
  ],
  "downstream_dependencies": [
    {"callee": "Engine::scan", "file": "./src/engine.rs"}
  ],
  "side_effects": ["Блок unsafe — снятые гарантии безопасности памяти"],
  "danger_level_if_modified": "MEDIUM (затронет 1 файл)"
}
```

Три уровня: (1) глобальная таблица символов (fn/struct/class/def + импорты,
межфайловые связи); (2) call graph с разрешением через таблицу символов,
фильтрацией определений и вызовов внутри строк/комментариев;
(3) двунаправленный BFS impact-анализ + эвристики сайд-эффектов
(unsafe, мьютексы, файловый I/O, сокеты, глобальное состояние) + danger level.

### Watcher: инкрементальный рескан по mtime

```bash
poler-engine ~/Eteryya -q "адамантит" --watch --interval-secs 2
```

Первичный полный скан, затем каждый такт сравниваются mtime/size: изменённые
и новые файлы обрабатываются заново, неизменённые гигантские дампы **не
перечитываются и не ретокенизируются**. Удалённые файлы исключаются из
глобальной статистики. Выход — Ctrl-C (SIGINT).

## Математический аппарат

### A. Информационная плотность ε(W_t)

```
ε(W_t) = κ · (1 + ln(1 + count(kw))) ·
         Σ_{w ∈ Unique(W_t) \ {kw}} (ln(N_total) − ln(freq(w)))² +
         Σ_{w ∈ W_t} Bonus_semantic(w)
```

* `N_total` — объём токенов корпуса (или файла при `--local-stats`);
* `freq(w)` — глобальная частота токена;
* `Bonus_semantic` — Ахо-Корасик словарь критических маркеров: отрицания (2.0),
  обязанность (1.5), критичность (1.5), угроза (1.2), код-маркеры (`unsafe`,
  `deprecated`, `panic!`), метрики;
* `κ` — калибровочный масштаб (`--kappa`).

### B. Линейный рекурсивный фильтр резонанса (IIR, O(N))

```
R_t = ε_t + φ · R_{t-1},    φ ∈ [0.75, 0.90]
```

Два режима (`--resonance-mode`):

* **hits** (по умолчанию) — IIR по последовательности ε совпадений в документе:
  повторные упоминания накапливают резонанс;
* **field** — ε вычисляется инкрементальным скользящим окном на **каждой**
  позиции документа (амортизированно O(1) на сдвиг), IIR прогоняется по всем
  позициям, сэмплируется в точках совпадений — строго O(N) один проход.

### C. K-hop подграф сущностей

```
SubGraph(E₀, k) = { (u, predicate, v) | dist(E₀, u) < k ∧ (u —predicate→ v) ∈ G }
```

BFS в **обе стороны** (outgoing + incoming), узлы сливаются case-insensitive,
каждый узел несёт temporal-слой. Тройки извлекаются из текста (SVO-эвристики:
`Нокс —вонзила_когти→ Солнечное сплетение`), из метаданных сцены
(`Нокс —появляется_в→ Глава 36`) и из кода (`process_data —вызывает→ compute`).

## Архитектура

```
files ──► [Проход A, rayon] mmap → PII-mask → токенизация → inverted index
              │                    (глобальные частоты токенов корпуса)
              ▼
        merge global stats (N_total, freq)
              ▼
        [Проход B, rayon] для hit-файлов:
              HitRecord (лёгкий): окно W_t → ε (+semantic bonus)
                                          → IIR R_t
                                          → локализация сцены (без клонов)
              Уникальные сцены: ScenePayload (сцена+тройки) один раз
              ▼
        EntityGraph (petgraph DiGraph) ── K-hop BFS от корневой сущности
              ▼
        temporal-фильтр → сортировка по R → top_n → материализация ContextAnchor
```

Ключевые решения производительности:

* **memmap2** — zero-copy чтение (валидный UTF-8 + нет PII → ноль копий файла);
* **PII-маскирование** возвращает `Cow::Borrowed` на чистом тексте; замаскированный
  текст малых hit-файлов кэшируется между проходами (нет повторного regex-скана);
* **HitRecord / ScenePayload** — тяжёлые payload (клон сцены, разбор троек)
  материализуются один раз на уникальную сцену и только для top-N якорей;
* **aho-corasick** — мультитокен-поиск семантических маркеров (LeftmostLongest);
* лексический сканер кода понимает строки, char-литералы (`'{'`!), raw-строки Rust
  (`r#"…"#`), комментарии и template-литералы JS с `${}` — скобки в них не считаются;
* release-профиль: `opt-level=3`, `lto=true`, `codegen-units=1`, `panic="abort"`, `strip`.

## Происхождение алгоритмов: анализ исходников базовых инструментов

Движок построен на техниках, извлечённых из исходных репозиториев
(см. `upstream/` в рабочей области):

| Источник | Файл/модуль | Техника | Внедрение в poler-engine |
|---|---|---|---|
| GNU grep 3.11 | `src/kwset.c` | Commentz-Walter: BM-сдвиги + AC-автомат, выбор исполнителя per-query | Литеральный SIMD-предфильтр `--fast`: aho-corasick по байтам **до** токенизации, файлы без ASCII-литерала отбраковываются (2.3× на разреженных корпусах) |
| ripgrep | `crates/ignore/src/walk.rs` | `WalkBuilder`: параллельный обход с .gitignore/.ignore/hidden | Обход каталогов — сам крейт `ignore` (код BurntSushi): `git_ignore`, `git_global`, `git_exclude`, `require_git(false)`, флаг `--hidden` |
| ripgrep | `regex-automata` prefilter | literal prefilter: regex не запускается без якорного байта | PiiCleaner: отсутствие `@`/цифр (проверка `memchr`) пропускает email/IP/phone/card regex |
| super-z-skills | `skills/_orchestrator/scripts/memory_graph.py` | SQLite-схема entities/relations с UNIQUE-констрейнтами | `--graph-export dump.sql`: дамп графа сущностей в этой же схеме, `sqlite3 graph.db < dump.sql` |
| GNU grep | `src/grep.c` | grep-совместимые коды выхода | 0/1/2 (совпадения/нет/ошибка) |

Отличие принципиальное: grep после нахождения строки останавливается —
poler-engine разворачивает каждое совпадение в полный аналитический
контекст (скоуп + ε + резонанс + K-hop подграф).

## Установка и сборка

```bash
cargo build --release          # бинарник: target/release/poler-engine
cargo test                     # 85 тестов (unit + integration)
cargo run --release --example bench
```

POSIX-совместимо: Linux/macOS/BSD, только чистый Rust без C-зависимостей.

## CLI

```
poler-engine [OPTIONS] --query <QUERY> <PATH>

-q, --query <QUERY>         слово или фраза (в кавычках: "не должна")
-t, --top <N>               топ-результатов [default: 10]
    --format <FORMAT>       ai-json | md | simple [default: ai-json]
    --phi <PHI>             затухание IIR-резонанса [default: 0.85]
    --kappa <K>             масштаб ε [default: 1.0]
-w, --window <RADIUS>       радиус токенного окна [default: 40]
-k, --k-hop <DEPTH>         глубина обхода графа [default: 2]
    --metric <TAG>          временной фильтр (например Т-23)
    --pii <MODE>            off | mask [default: mask]
    --resonance-mode <MODE> hits | field [default: hits]
    --local-stats           ε по статистикам файла вместо корпуса
    --extensions <LIST>     сканируемые расширения
    --max-file-size <MB>    [default: 32]
    --max-scope <BYTES>     потолок enclosing_scope [default: 16384]
    --max-relations <N>     потолок K-hop связей на якорь [default: 64]
    --impact <SYMBOL>       AIDDE: impact-паспорт символа (upstream/downstream)
    --impact-cache <DB>     disk-backed таблица символов (SQLite): для баз 65K+ файлов
    --impact-reuse          не перестраивать существующую базу (мгновенные повторы)
    --impact-depth <N>      глубина BFS impact-анализа [default: 3]
    --watch                 watcher: инкрементальный рескан по mtime
    --diff                  дифф-режим watcher: только новые якоря
    --interval-secs <N>     интервал watcher-опроса [default: 2]
    --hidden                показывать скрытые файлы (аналог rg --hidden)
    --graph-export <PATH>   дамп графа сущностей в SQL (схема super-z memory_graph)
    --max-graph-triples <N> бюджет рёбер графа [default: 200000]
    --threads <N>           потоки rayon [default: все ядра]
    --google-auth           OAuth 2.0 loopback: согласие Google в твоём браузере
    --google-gmail [Q]      поиск в своём Gmail (синтаксис Gmail; пусто — недавние)
    --google-drive [Q]      файлы Google Drive по имени (пусто — недавние)
    --google-status         состояние токенов: скоупы, срок, email
    --google-browse <URL>   открыть URL в оконном браузере с профилем poler
    --google-fetch <URL>    прочитать URL через персистентный профиль (headless)
    --google-scopes <S>     доп. скоупы OAuth для --google-auth (через пробел)
    --google-max <N>        лимит Gmail/Drive-результатов [default: 10]
    --nlm-notebooks         NotebookLM: все ноутбуки с источниками (batchexecute)
    --nlm-source <NB> <SRC> контент источника: текст ИЛИ URL картинок слайдов
    --nlm-notes <NB>        сохранённые заметки ноутбука
    --nlm-artifacts <NB>    Studio-объекты: аудио-обзоры, отчёты, квизы, миндмэпы
    --nlm-account           email/настройки сессии NotebookLM
    --nlm-chat <NB> <Q>     вопрос к модели ноутбука ПО ЕГО ИСТОЧНИКАМ
    --nlm-media <URL>       скачать медиа профильным Chromium → ~/.cache/poler-engine/nlm/
    --nlm-shot <URL>        скриншот страницы NotebookLM → PNG
    --nlm-sync [NB]         v0.14: залить ВСЕ ноутбуки в web-index.db (nlm:// URL)
    --web <URL>             v0.8: отрендерить страницу через Chromium CDP
    --web-search <Q>        v0.9+: поиск по web-index.db (BM25+PageRank+фразы)
    --web-db <PATH>         путь к индексу [default: ./web-index.db]
    --web-stats             статистика индекса: страницы, ссылки, PageRank
    --crawl <URL>           краулер: BFS от URL (robots.txt + sitemap + SimHash-дедуп)
    --crawl-depth <N>       глубина краула [default: 2]
    --crawl-max <N>         лимит страниц [default: 25]
    --crawl-delay-ms <N>    задержка между запросами [default: 1000]
    --cross-site            разрешить краулу переходы на другие домены
    --cdp-port <N>          порт CDP Chromium [default: 9222]
    --web-wait-ms <N>       ожидание рендера страницы [default: 1200]
    --headless              headless-режим Chromium
    --no-sandbox            отключить sandbox Chromium (для root/CI)
    --remote-debugging-port <N>  явный порт отладки Chromium
    --mcp                   v0.10: MCP-сервер (stdio JSON-RPC 2.0) для LLM-агентов
    --mcp-http [BIND]      v0.17.4: MCP-сервер по HTTP (Streamable HTTP) для
                           УДАЛЁННОГО агента: POST / или /mcp,
                           Authorization: Bearer <токен> [default: 127.0.0.1:8765]
    --mcp-token <TOKEN>    токен для --mcp-http (или env POLER_MCP_TOKEN;
                           без него — автогенерация при старте)
    --shell                 v0.15+: poler-shell REPL
    --tui                   v0.17: 4-панельный TUI (MiMo Code-style)
    --psi-eta <N>           POLER[Ψ]: шаг ψ-потока [default: 0.05]
    --psi-gamma <N>         POLER[Ψ]: наклон потенциала [default: 0.5]
    --psi-rho <N>           POLER[Ψ]: вес резонанса [default: 0.9]
    --psi-depth <N>         POLER[Ψ]: глубина [default: 8]
    --poler-eta <N>         POLER-цикл: learning rate [default: 0.01]
    --poler-gamma <N>       POLER-цикл: γ [default: 0.1]
    --poler-mix <N>         POLER-цикл: микс [default: 0.1]
    --poler-dissipator <N>  POLER-цикл: диссипатор [default: 0.02]
-v, --verbose               статистика прогона в stderr
```

Коды выхода (grep-совместимые): `0` — есть совпадения, `1` — нет, `2` — ошибка.

## Выходной контракт (Context Anchor)

```json
{
  "query": "нокс",
  "total_hits": 3,
  "anchors": [
    {
      "file": "/path/to/chapter_36.md",
      "token": "нокс",
      "epsilon": 2859.53,
      "resonance": 6185.5,
      "scene": {
        "chapter": "Глава 36. Инертный",
        "temporal_metric": "Метрика: Т-23",
        "location": "Локация: Разлом Каньона",
        "subjects": ["Субъекты: Мальчик (гибрид), Соболь (Нокс)"],
        "enclosing_scope": "# Глава 36. Инертный ..."
      },
      "k_hop_relations": [
        ["Нокс", "вонзила_когти", "Солнечное сплетение"],
        ["Шунт", "сбрасывает_тепло", "1300°C"]
      ]
    }
  ]
}
```

`total_hits` — все совпадения до усечения по `--top`; `k_hop_relations` —
подграф связей корневой сущности (первый токен запроса).

## Производительность

Синтетический корпус: 400 файлов × 25 абзацев, 3.14 МБ, 264 400 токенов,
10 400 совпадений, 2 потока (vCPU):

| Метрика | Значение |
|---|---|
| Полный прогон (hits-режим) | **~770 мс** |
| Field-режим (строго O(N)) | ~670 мс |
| Файлов/с | ~520 |
| Пиковая память (плотный корпус 3 МБ) | 22 МБ |
| Пиковая память (стресс-корпус 91 МБ) | **99 МБ** (v0.2.1: 431 МБ) |

Для сравнения: ripgrep находит строки в ~100 раз быстрее, но не возвращает
скоупов, метрик, троек и K-hop — это цена полной аналитики на каждое совпадение.

## Структура проекта

```
poler-engine/
├── Cargo.toml                  # clap, rayon, memmap2, petgraph, serde, regex, aho-corasick, walkdir
├── FUTURE_ROADMAP.md           # «превзойти Google»: цель записана, срок не определён
└── src/
    ├── main.rs                 # CLI: --format [ai-json|md|simple], grep-совместимые коды выхода
    ├── mcp.rs                  # MCP-сервер (stdio JSON-RPC 2.0) для LLM-агентов
    ├── lib.rs                  # двухпроходный параллельный пайплайн, EngineConfig, ScanStats
    ├── web/                    # v0.8–v0.11: веб-поиск
    │   ├── cdp.rs              # нативный Chromium CDP-клиент (WebSocket RFC 6455)
    │   ├── crawl.rs            # frontier BFS, robots.txt, sitemap, SimHash-дедуп
    │   ├── index.rs            # SQLite-инвертированный индекс + BM25 + PageRank + WebRank
    │   ├── phrase.rs           # v0.11: позиционный кодек (delta-varint) + фразовый поиск
    │   ├── stem.rs             # кириллический стеммер (uk/рос)
    │   └── …                   # robots, simhash, urlnorm, extract
    ├── google/                  # v0.12–v0.13: сервисы Google без пароля
    │   ├── mod.rs              # google-браузер (порт 9223) + персистентный профиль + GoogleHttp
    │   ├── oauth.rs            # OAuth 2.0 loopback (RFC 8252), refresh, хранилище 0600
    │   ├── api.rs              # Gmail/Drive readonly-API + форматтеры
    │   └── nlm.rs              # v0.13: NotebookLM batchexecute-протокол + медиа-канал
    ├── tokenizer/
    │   ├── pii.rs              # zero-copy (Cow) маскирование PII
    │   └── inverted_index.rs   # индекс всех токенов, включая отрицания
    ├── parser/
    │   ├── ast_code.rs         # лекс-сканер: brace/indent enclosing scope (Rust/C/JS/Python)
    │   ├── markdown_scenes.rs  # сцены: главы, метрики, локации, субъекты
    │   └── triples.rs          # SVO-тройки + call graph + co-occurrence
    ├── resonance/
    │   ├── epsilon.rs          # ε + Ахо-Корасик маркеры + скользящее окно O(1)
    │   └── iir_filter.rs       # R_t = ε_t + φ·R_{t-1}
    ├── graph/
    │   └── entity_graph.rs     # DiGraph (petgraph), K-hop BFS, temporal-слои
    └── output/
        └── context_anchor.rs   # AI-Ready JSON + рендеры md/simple
```

## Известные ограничения (честно)

* SVO-извлечение троек — эвристическое (морфология русского языка без
  полного парсера); ориентировано на воспроизводимость и полноту связей, не
  на лингвистическую точность;
* потоковая схема v0.3 платит ~30–40% времени за двойную токенизацию
  (проходы 1 и 2) — сознательный размен памяти на скорость;
* кириллические запросы проходят предфильтр через lowercase-копию текста
  (одна аллокация на файл), ASCII — через SIMD aho-corasick без аллокаций;
* AIDDE — лексический уровень (без полного парсера типов): разрешение
  перегрузок и trait-диспетчеризации недоступно;
* в field-режиме семантический бонус не начисляется (он определён на уровне
  совпадений);
* память: индексы hit-файлов и их кэшированный текст (до 1 МБ на файл)
  удерживаются до конца прогона; для сверхбольших репозиториев используйте
  `--local-stats` и послабление `--max-file-size`.

## Тестирование

143 теста: 105 unit (математика ε/IIR, сканер скобок, raw-строки, PII,
разбиение предложений, K-hop, temporal-фильтр) + 38 интеграционных
(воспроизведение контракта спецификации на фикстуре главы 36, call graph,
PII-маскирование, детерминизм, сортировка, режимы резонанса).

```bash
cargo test
cargo clippy --all-targets   # 0 предупреждений
```

## Docker и CI

```bash
# Локальная сборка образа (~120 МБ, debian-slim)
docker build -t poler-engine .

# Поиск в смонтированном корпусе
docker run --rm -v ~/Eteryya:/data poler-engine:latest /data -q "адамантит" -t 5

# AIDDE impact-анализ
docker run --rm -v ~/project:/data poler-engine:latest /data --impact main
```

CI (`.github/workflows/ci.yml`): матрица ubuntu/macos, clippy с `-D warnings`,
полные тесты, смоук-тесты бинарника и контейнера, микробенчмарк, автосборка
релизных tarball с SHA256SUMS по тегам `v*`.

## Лицензия

**POLER Custom Source-Available & Modification Disclosure License v1.0**
(модель Unreal Engine EULA, с v0.22.0; см. `LICENSE.md` — юридический
инструмент, `TERMS.md` — практическая сводка):

- исходники открыты для **изучения, локальной сборки и модификации**;
- **обязательное уведомление** авторов (dev@poler-engine.org, 14 дней)
  при дистрибуции/деплое продукта на **модифицированном** ядре
  (Notification Clause);
- публичная редистрибуция ядра и форков **запрещена**;
- коммерческое использование — тиры Community/Pro/Enterprise +
  роялти 5% выручки продукта свыше $25 000/квартал (safe harbor
  $10 000/квартал);
- обход Ed25519 License Gate — нарушение лицензии (автоматическое
  прекращение).

Снапшоты до v0.17.7 включительно остаются под MIT OR Apache-2.0.
Статус в бинарнике: `poler-engine --license`.
