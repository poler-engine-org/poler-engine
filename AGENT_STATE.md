# AGENT STATE — машиночитаемое состояние агента (poler-engine)

> Обновляется в конце каждой сессии и после значимых коммитов.
> Истинный HEAD — `git log -1 --oneline`. Формат — строгий `key: value`.

updated_utc: 2026-08-29T23:20:00Z
repo: poler-engine
branch: main
commit: (заполняется после коммита сессии)
tag: v0.17.6 (релизные теги — не на каждый патч; v0.18.0 — License Gate)
pushed: true
remote: https://github.com/poler-engine-org/poler-engine.git (репо ПЕРЕНЕСЁН в оргу poler-engine-org 2026-08-29; старые URL Kotokvit/* редиректят; второй репо орги — poler-engine-org/POLER-Quantum-RS, тоже private)
tests: cargo test 687 пройдено (648 lib + 38 integration + 1 doc, 0 failed, 4 ignored=live); secret-scan-commit PASS (27 подстрок, вкл. seed лицензий); самопроверка: --license Pro до 2036; --google-status authorized:true + строка лицензии; --google-gmail живой запрос под Pro
current_task: web-ux-audit (Dogfooding, запрос владельца «насколько ебануто ручной парсинг + не всраивать ли поиск в браузер») — ЖИВОЙ АУДИТ ВЫПОЛНЕН: краул ed25519.cr.yp.to (3 стр/6.5с), RFC 8032 с rfc-editor (2.0с), docs.rs ЗАВИС >180с (JS-тяжёлый, частичный индекс выжил); --web-search 7-30мс с прозрачным скорингом (0.55·BM25+0.15·PageRank+0.20·title+0.10·ε-density); перефраз нашёл RFC (score 0.733), русский запрос к англ. корпусу = 0 результатов (кросс-язычного моста НЕТ); REPL пайп-режим работает
current_task_note: вердикт: для владельца/агентов (MCP) — рабочее; для человека-в-браузере — нет. Боль вживую: (1) POLER_CHROME_BIN не автодетектится из playwright-кеша, (2) нет per-page таймаута в крауле, (3) robots-skip молчит счётчиком, (4) осиротевший CDP после kill → «ws frame hdr». Архитектура WebLens зафиксирована в FUTURE_ROADMAP §8: T0 (CLI/MCP — есть), T1 (фикс-лист + --browser-index, v0.19.0-окно), T2 (MV3-расширение; бэкенд УЖЕ ЕСТЬ = --mcp-http 127.0.0.1:8765 с Bearer+CORS, mcp_http.rs:293,307,387 — писать только JS-оболочку), T3 (форк Chromium ОТКЛОНЁН с цифрами: ~100ГБ исходники, сборки часами, релиз-поезд 4 недели — соло-дев перестанет развивать движок). Дефляция маркетинга Гемини: «на любом языке» = ложь для MVP; киллер-ценность WebLens — capture (страница→индекс→один поиск по всему), не подсветка
next_task: T1 по команде владельца (Dogfooding-мультипликатор): автодетект chrome из playwright-кеша (src/web/cdp.rs), per-page таймаут+прогресс в crawl(), человекочитаемое robots-сообщение, тихий перезапуск осиротевшего CDP, флаг --browser-index <URL> (механика google-fetch + upsert в web-index.db); T2 WebLens MVP после T1
blocked_on: ничего (для рут-коза висячки нужен один успешный consent-прогон под strace — требует тапа владельца)
tmux_sessions: нет (контейнер без root; на сервере обязателен tmux — §2 канона)
credentials: ВАЛИДЕН — файл-хранилище upload/«гитхаб токен .txt» (API 200, перепроверен 2026-08-29); подача через /home/z/my-project/scripts/gh-cred-helper.sh; автопроверка — agent_bootstrap.sh (канон)
notes: канон протокола Context-Free Resilience — POLER-Quantum-RS v0.3.8 (AGENT.md); workspace-репо /home/z/my-project/.git локальное, НЕ пушить; dev-stand живёт в песочнице /home/z/my-project (стенд = копия в dev-stand/, синхронизировать при правках); gcp-live.json/gcp-oauth.json/client_secret.json — только 0600 в ~/.config, в git не попадают

## Последние сессии

| Дата (UTC) | Задача | Результат |
|---|---|---|
| 2026-08-29 | web-ux-audit | Живой аудит веб-конвейера (Dogfooding): замеры краул/поиск/REPL; 4 точки боли; FUTURE_ROADMAP §8 — тиры T0-T3 браузерной поверхности: WebLens = MV3-расширение поверх существующего --mcp-http (Bearer+CORS готовы), форк Chromium отклонён с цифрами; дефляция маркетинга (кросс-язычности нет — замер 0 результатов) |
| 2026-08-29 | org-transfer | Оба репо перенесены в poler-engine-org (HTTP 202, private=true сохранён, admin/push на месте, редиректы Kotokvit/* работают); орга: 1 участник, 2 private/0 public, default_perm=read; private-only-политика недоступна на free (422, некритично); origin переключён на оргу; ссылки README/AGENT.md обновлены; стратегия Dogfooding + MoR-планы зафиксированы в FUTURE_ROADMAP.md §7 |
| 2026-08-29 | license-gate v0.18.0 | License Gate ed25519 (PO1): тиры+trial+квоты+grace, license-tool, гейты CLI/shell/MCP, --license/--license-import, прозрачность NotebookLM в --google-auth; 687 тестов; E2E: активация/подделка отклонена/квота блок/live gmail под pro; секрет-скан PASS; лицензия владельца pro до 2036 |
| 2026-08-29 | monetization-strategy | Репо poler-engine + POLER-Quantum-RS переведены в PRIVATE (0 форков/0 релизов — потери нет, обратимо). Факты Google: скоупы движка оба restricted; CASA Tier 2 = $540/год (TAC); Testing-режим = 100 юзеров + 7-дневные refresh. Решение: платная закрытая бета БЕЗ верификации → License Gate (ed25519) следующим шагом; верификация отложена до платящих юзеров |
| 2026-08-29 | oauth-врезка | v0.17.7: --google-auth на живом клиенте POLER Engine — E2E пройден дважды (токены 0600, аудит, --google-status/--google-gmail/refresh живые); hard-exit патч + shutdown браузера; poler-auth-selftest.js/fakecode.js в dev-stand; cargo test 667 зелёные; запушено |
| 2026-08-29 | gcp-e2e | E2E OAuth-тест ПРОЙДЕН: consent → auth-code → token exchange 200 → refresh 200; brand «POLER Engine» + Desktop-клиент + Drive/Gmail API подтверждены через API (gcp-verify-state.js); набор gcp-*-скриптов (cdp-machinery, e2e, newclient, branding-audience, secret, verify-state) в dev-stand; запушено |
| 2026-08-29 | gcp-setup | gcp-setup.js + daemon в dev-stand: цифра подтверждения в лог+превью, одна попытка 61 мин без рестартов; подтверждение 79 пройдено, auth-code пойман; token exchange 401 invalid_client (креды gcloud SDK) — блокер; секреты вынесены в 0600-конфиг; запушено |
| 2026-08-29 | dev-stand | headless-стенд Auth Companion: CDP-релей превью + супервизор + security-audit (27/2/0); вход Google подтверждён (28 cookies, exit=0); коммит 741c5ae, запушено |
| 2026-08-28 | agent-протокол | AGENT.md-стаб + AGENT_STATE.md (канон: POLER-Quantum-RS v0.3.8) |
| 2026-08-28 | sync-remotes | merge 36b825f двух линий v0.17.4, 601 тест, запушено |
| 2026-08-28 | Transcript View | F3-лента + Response View, PTY-смоук PASS, c7a1d86 |
