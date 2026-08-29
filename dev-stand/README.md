# Dev-Stand: headless-стенд Auth Companion

Платформенная песочница (контейнер без дисплея и без графического окружения)
не может показать окно `--auth-ui`. Этот стенд решает задачу: **поднимает
auth-companion на виртуальном дисплее и транслирует его экран владельцу через
превью-прокси платформы** — со скринкастом, вводом мыши/клавиатуры и кнопками
навигации.

## Архитектура

```
[браузер владельца]
     ⇅ https/wss (превью-прокси платформы :81)
[next dev :3000]  — UI-пульт (React: скринкаст + ввод + статус)
     ⇐ /api/* (rewrite → 127.0.0.1:3100)
[auth-preview relay :3100]  — CDP-клиент + HTTP/WS/SSE-сервер
     ⇐ Page.startScreencast (JPEG-кадры) / Input.dispatch* (мышь, клавиши)
[auth-companion.js :8765]   — единственный, кто читает куки
     └─ spawn → [Chromium @ Xvfb :99, изолированный профиль движка]

над всем — auth-dev-orchestrator.js (супервизор) + auth-stack-daemon.py
(double-fork: PPID→1, переживает завершение bash-сессии платформы)
```

## Состав

| файл | назначение |
|---|---|
| `auth-stack.sh` | `start \| stop \| status` — управление всем стеком |
| `auth-stack-daemon.py` | double-fork-демон: удерживает оркестратор живым |
| `auth-dev-orchestrator.js` | супервизор: Xvfb :99 → relay :3100 → companion :8765 → next :3000; авто-подъём умерших компонентов; **при захваченной сессии окно не поднимает** (`google_session.json` существует → companion не стартует) |
| `auth-preview.js` | CDP-релей: screencast → JPEG-кадры (WS/SSE/polling), ввод → `Input.dispatch*`, навигация (Назад/Вперёд/Обновить/goto — только http/https), статус companion; **подхватывает живой браузер `gcp-setup.js`** (файл `gcp-live.json`: экран + цифра подтверждения в превью) |
| `gcp-setup.js` | автономная настройка GCP-проекта через живую сессию Google: фазы `token → probe → brand → client → user` (Drive/Gmail API, consent screen, OAuth-клиент Desktop → `client_secret.json` 0600, test user). Цифра подтверждения Google извлекается из DOM и печатается в лог + превью; **одна попытка, таймаут 61 мин, никаких перезапусков** |
| `gcp-setup-daemon.py` | double-fork-обёртка `gcp-setup.js` (PPID→1, переживает bash-сессии песочницы) |
| `gcp-cdp-machinery.js` | переиспользуемый CDP-модуль: свой WebSocket-клиент (без playwright), `launchChromium`/`connectPageRetry`/`evalRetry`, лог; общая база для всех gcp-скриптов |
| `gcp-newclient.js` | создание OAuth-клиента Desktop через консоль GCP (когда brand-через-API недоступен): навигация по консоли, форма «Create OAuth client», выгрузка `client_secret.json` (0600) |
| `gcp-branding-audience.js` | финальные штрихи consent screen через консоль: App name «POLER Engine» (branding) + test user (audience), терпеливые reload-ретраи |
| `gcp-secret.js` | выгрузка client_secret из консоли GCP (DOM + shadow DOM + clipboard), сохранение в `~/.config/poler-engine/client_secret.json` (0600) |
| `gcp-e2e-oauth.js` | **end-to-end тест OAuth-клиента POLER**: аккаунт → consent «Разрешить» → auth-code на loopback → token exchange → refresh-тест → userinfo; проверяет ровно путь `poler-engine --google-auth`; токены не печатаются |
| `gcp-e2e-daemon.py` | double-fork-обёртка `gcp-e2e-oauth.js` (PPID→1, лог `logs/gcp-e2e.log`) |
| `gcp-e2e-retry.js` | реанимация зависшего consent после Google Error 500: навигация живого браузера на тот же auth-URL (loopback-ловушка e2e остаётся ждать код) |
| `gcp-e2e-enter-code.js` | ввод Защитного кода (ootp-челлендж) в форму + атомарный клик «Далее» одним eval |
| `gcp-e2e-probe.js` | чтение текущего состояния страницы живого браузера e2e (URL/title/body) — отладка |
| `gcp-verify-state.js` | API-проверка настройки GCP по refresh-токену: проект, brand (applicationTitle), статусы Drive/Gmail API — факты без секретов |
| `app/` | Next.js-пульт: `src/app/page.js` (скринкаст + ввод + кнопки), `next.config.mjs` (`/api/*` → релей) |
| `security-audit.py` | 29 проверок безопасности (артефакты, сеть, утечки через HTTP, логи, статический анализ, процессы) |
| `test-relay-ws.js` | smoke-тест WS-канала релея |

## Запуск

```bash
bash dev-stand/auth-stack.sh start    # весь стек + статус
bash dev-stand/auth-stack.sh status   # процессы/порты/companion
bash dev-stand/auth-stack.sh stop     # аккуратная остановка

python3 dev-stand/security-audit.py   # аудит (exit 0 = FAIL-ов нет)
node dev-stand/test-relay-ws.js       # проверка потока кадров
```

В песочнице: превью платформы → `next dev :3000`. Локально на хосте
стенд не нужен — там `poler-engine --auth-ui` открывает окно напрямую.

## Модель безопасности (кто что может)

* **Релей не читает сессию.** `google_session.json` используется только через
  `statSync` (факт существования для статуса «authorized»). Значения кук
  проходят только внутри companion → файл 0600.
* **Релей не логирует ввод.** Клавиши/клики идут транзитом в CDP; в логах —
  только подключения/отключения и URL-хосты.
* **Всё слушает 127.0.0.1**, кроме `next :3000` (контракт превью платформы).
  CDP-порт Chromium закрыт сразу после завершения companion.
* **Навигация ограничена.** Кнопка «goto» принимает только http/https —
  никакого `file://` / `chrome://`.
* **Watchdog уважает контракт exit-кодов** companion (0/2/3/4/130): после
  `exit=0` (сессия захвачена) окно не поднимается никогда; после 2/3 —
  поднимется заново (не дать превью «слететь»), лимит 6 рестартов / 10 мин.
* **Аудит-скрипт не печатает секреты**: значения кук ищутся в логах/HTML/API
  по вхождению подстрок, в отчёт попадают только факты «найдено/не найдено».

## gcp-setup: настройка GCP без «фарминга» цифр

Скрипт открывает OAuth-consent gcloud SDK в headless Chromium на изолированном
профиле движка. Если Google требует подтвердить вход с телефона:

* цифра извлекается из DOM и пишется в лог (`🔢 ЦИФРА НА ЭКРАНЕ`) и в
  `~/.config/poler-engine/gcp-live.json` → превью показывает её владельцу;
* скрипт ждёт до 61 минуты, **не перезапуская браузер** (каждый рестарт =
  новая цифра);
* после подтверждения сам кликает «Разрешить» и ловит authorization code
  на loopback-редиректе.

Креды gcloud SDK не хранятся в коде: env `GC_ID`/`GC_SEC` или
`~/.config/poler-engine/gcp-oauth.json` (0600). Полученный в конце
`client_secret.json` (секрет OAuth-клиента POLER) пишется рядом, тоже 0600,
и в репозиторий не попадает.

```bash
python3 dev-stand/gcp-setup-daemon.py verification-506705 all   # фон (PPID→1)
tail -f logs/gcp-setup.log                                     # мониторинг
node dev-stand/gcp-setup.js verification-506705 probe           # одна фаза
```

## gcp-e2e: end-to-end тест OAuth-клиента

Полная проверка того пути, которым будет ходить `poler-engine --google-auth`:
loopback-редирект → consent «Разрешить» → authorization code → **token
exchange** → **refresh-тест** → userinfo. Секреты читаются только из
`~/.config/poler-engine/` (0600), в лог попадают длины и хвосты, не значения.

Подтверждение Google (цифра на телефоне / Защитный код с устройства) — единственный
ручной шаг: e2e-цикл сам кликает аккаунт и «Разрешить», а код владельца вводится
`gcp-e2e-enter-code.js`. Если consent упал в Error 500 (бывает на
challenge-завершении), `gcp-e2e-retry.js` переоткрывает auth-URL в том же
браузере — повторный заход после подтверждения личности проходит без челленджа.

```bash
python3 dev-stand/gcp-e2e-daemon.py          # фон: E2E от начала до конца
tail -f logs/gcp-e2e.log                     # 🔢 цифра / код → вводит владелец
node dev-stand/gcp-e2e-retry.js <cdp> <port> # реанимация после Error 500
node dev-stand/gcp-verify-state.js           # факт-чек настройки по API
```

## Известные ограничения

* Скринкаст — JPEG-кадры (не видео): статичная страница обновляется
  keep-alive-снимком раз в 3 с, при вводе — мгновенно.
* `POLER_CHROME_NO_SANDBOX=1` в стенде — это контейнер без userns;
  на хосте companion запускается БЕЗ этого флага (канон движка).
