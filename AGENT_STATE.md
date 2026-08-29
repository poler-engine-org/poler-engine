# AGENT STATE — машиночитаемое состояние агента (poler-engine)

> Обновляется в конце каждой сессии и после значимых коммитов.
> Истинный HEAD — `git log -1 --oneline`. Формат — строгий `key: value`.

updated_utc: 2026-08-29T21:05:00Z
repo: poler-engine
branch: main
commit: pending
tag: v0.17.6 (релизные теги — не на каждый патч)
pushed: true
tests: cargo test 667 пройдено (628 lib + 38 integration + 1 doc, 0 failed, 4 ignored=live); secret-scan-commit PASS; самопроверка: --google-status authorized:true refreshable:true; --google-gmail живой запрос; refresh-флоу живой (токен обновлён по refresh_token)
current_task: врезка официального OAuth в poler-engine --google-auth — ЗАВЕРШЕНА И ПРОВЕРЕНА E2E
current_task_note: движок читает живой client_secret.json (проект POLER Engine, Desktop-клиент) из ~/.config/poler-engine/; полный путь пройден дважды E2E (poler-auth-selftest.js): URL → consent-клики → код на loopback движка → token exchange 200 → google_tokens.json 0600 + аудит oauth.auth; v0.17.7: гарантированный exit после успешного --google-auth с явным shutdown_owned_headless_browser() до него (наблюдалось редкое незавершение процесса при переиспользованном CDP-браузере — работа завершена, токены сохранены, но процесс жил ~2 мин; корневая причина не локализована, патч — защита)
next_task: production-вывод: Publish app + verification (Privacy Policy на GitHub Pages + unlisted YouTube-скринкаст) по плану владельца; опционально — рут-коз редкой висячки через strace на живом репро
blocked_on: ничего (для рут-коза висячки нужен один успешный consent-прогон под strace — требует тапа владельца)
tmux_sessions: нет (контейнер без root; на сервере обязателен tmux — §2 канона)
credentials: ВАЛИДЕН — файл-хранилище upload/«гитхаб токен .txt» (API 200, перепроверен 2026-08-29); подача через /home/z/my-project/scripts/gh-cred-helper.sh; автопроверка — agent_bootstrap.sh (канон)
notes: канон протокола Context-Free Resilience — POLER-Quantum-RS v0.3.8 (AGENT.md); workspace-репо /home/z/my-project/.git локальное, НЕ пушить; dev-stand живёт в песочнице /home/z/my-project (стенд = копия в dev-stand/, синхронизировать при правках); gcp-live.json/gcp-oauth.json/client_secret.json — только 0600 в ~/.config, в git не попадают

## Последние сессии

| Дата (UTC) | Задача | Результат |
|---|---|---|
| 2026-08-29 | oauth-врезка | v0.17.7: --google-auth на живом клиенте POLER Engine — E2E пройден дважды (токены 0600, аудит, --google-status/--google-gmail/refresh живые); hard-exit патч + shutdown браузера; poler-auth-selftest.js/fakecode.js в dev-stand; cargo test 667 зелёные; запушено |
| 2026-08-29 | gcp-e2e | E2E OAuth-тест ПРОЙДЕН: consent → auth-code → token exchange 200 → refresh 200; brand «POLER Engine» + Desktop-клиент + Drive/Gmail API подтверждены через API (gcp-verify-state.js); набор gcp-*-скриптов (cdp-machinery, e2e, newclient, branding-audience, secret, verify-state) в dev-stand; запушено |
| 2026-08-29 | gcp-setup | gcp-setup.js + daemon в dev-stand: цифра подтверждения в лог+превью, одна попытка 61 мин без рестартов; подтверждение 79 пройдено, auth-code пойман; token exchange 401 invalid_client (креды gcloud SDK) — блокер; секреты вынесены в 0600-конфиг; запушено |
| 2026-08-29 | dev-stand | headless-стенд Auth Companion: CDP-релей превью + супервизор + security-audit (27/2/0); вход Google подтверждён (28 cookies, exit=0); коммит 741c5ae, запушено |
| 2026-08-28 | agent-протокол | AGENT.md-стаб + AGENT_STATE.md (канон: POLER-Quantum-RS v0.3.8) |
| 2026-08-28 | sync-remotes | merge 36b825f двух линий v0.17.4, 601 тест, запушено |
| 2026-08-28 | Transcript View | F3-лента + Response View, PTY-смоук PASS, c7a1d86 |
