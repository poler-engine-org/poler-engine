# AGENT STATE — машиночитаемое состояние агента (poler-engine)

> Обновляется в конце каждой сессии и после значимых коммитов.
> Истинный HEAD — `git log -1 --oneline`. Формат — строгий `key: value`.

updated_utc: 2026-08-29T18:45:00Z
repo: poler-engine
branch: main
commit: (см. git log -1; сессия: gcp-setup в dev-stand)
tag: v0.17.6
pushed: true
tests: secret-scan-commit PASS (26 подстрок: куки+токен+gcloud-secret, 15 файлов); security-audit 27/2/0 (сессия выше); Rust-код не тронут
current_task: gcp-setup — автономная настройка GCP-проекта (verification-506705)
current_task_note: подтверждение телефона 79 ПРОЙДЕНО (consent «Разрешить» кликнут, authorization code пойман 73 симв.); блокер — token exchange 401 invalid_client: креды gcloud SDK отозваны/недействительны; фазы probe/brand/client/user не выполнены; креды вынесены из кода в ~/.config/poler-engine/gcp-oauth.json (0600)
next_task: чинить token exchange: либо найти рабочие креды gcloud SDK (свежий google-cloud-sdk), либо вести OAuth-флоу через собственный клиент (создать OAuth-клиент в консоли вручную один раз) → дальше фазы probe/brand/client/user; после client_secret.json — self-test движка с OAuth-клиентом
blocked_on: валидные client_id/client_secret для cloud-platform token exchange (401 invalid_client на кредах gcloud SDK)
tmux_sessions: нет (контейнер без root; на сервере обязателен tmux — §2 канона)
credentials: ВАЛИДЕН — файл-хранилище upload/«гитхаб токен .txt» (API 200, перепроверен 2026-08-29); подача через /home/z/my-project/scripts/gh-cred-helper.sh; автопроверка — agent_bootstrap.sh (канон)
notes: канон протокола Context-Free Resilience — POLER-Quantum-RS v0.3.8 (AGENT.md); workspace-репо /home/z/my-project/.git локальное, НЕ пушить; dev-stand живёт в песочнице /home/z/my-project (стенд = копия в dev-stand/, синхронизировать при правках); gcp-live.json/gcp-oauth.json/client_secret.json — только 0600 в ~/.config, в git не попадают

## Последние сессии

| Дата (UTC) | Задача | Результат |
|---|---|---|
| 2026-08-29 | gcp-setup | gcp-setup.js + daemon в dev-stand: цифра подтверждения в лог+превью, одна попытка 61 мин без рестартов; подтверждение 79 пройдено, auth-code пойман; token exchange 401 invalid_client (креды gcloud SDK) — блокер; секреты вынесены в 0600-конфиг; запушено |
| 2026-08-29 | dev-stand | headless-стенд Auth Companion: CDP-релей превью + супервизор + security-audit (27/2/0); вход Google подтверждён (28 cookies, exit=0); коммит 741c5ae, запушено |
| 2026-08-28 | agent-протокол | AGENT.md-стаб + AGENT_STATE.md (канон: POLER-Quantum-RS v0.3.8) |
| 2026-08-28 | sync-remotes | merge 36b825f двух линий v0.17.4, 601 тест, запушено |
| 2026-08-28 | Transcript View | F3-лента + Response View, PTY-смоук PASS, c7a1d86 |
