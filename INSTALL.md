# poler-engine v0.17.3 — Companion Bridge M2+M3+M4 + future-streaming-archives

## Что нового

### v0.17.3 (Companion Bridge M2+M3+M4)

- **M2 ✓** `GcpEnterpriseProvider` — реал-имплементация 9 операций оф. Pre-GA NotebookLM Enterprise API через `ureq` (rustls) с Bearer из `oauth::ensure_gcp_fresh` (cloud-platform scope, `gcp_tokens.json`):
  - `list_notebooks`, `get_notebook`, `batch_create_sources`, `upload_file` (`X-Goog-Upload-Protocol: raw`), `get_source`, `batch_delete_sources`, `create_audio_overview`, `delete_audio_overview`.
  - lazy-init `Agent` (60s timeout), `bearer_json`/`bearer_upload`/`bearer_get` хедеры, `ureq_err` → `BridgeError`.

- **M3 ✓** `HybridProvider` routing + fallback:
  - `HybridProvider::route<F,G,R>(op, f_gcp, f_cdp)` generic-helper.
  - Routing policy: `GcpOnly`/`CdpOnly` — primary only, fallback off; `Auto` (default) — primary=GCP если `gcp.supports(op)`, иначе CDP; fallback on (только если secondary `supports(op)`).
  - Fallback триггерится только на `NotSupported`/`NotConfigured`; `Http`/`Transport`/`Parse` propagates без fallback.
  - `primary_for(op)` и `fallback_enabled()` — pure-fns для тестов.

- **M4 ✓** TUI Enter-handler на источнике (Sources panel):
  - Источник из `poler_sources` маппится в `companion::SourceKind` (file → FileUpload, url → Web, repo → Web{github.com}).
  - `enter_action()` → `EnterAction`: `EditLocal` (spawn `$EDITOR`), `OpenUrl` (`xdg-open`), `EditTemp` (write `/tmp/...` + `$EDITOR`), `FallbackFetch` (message для будущей CdpBatchexecuteProvider имплементации).

- **Дизайн-нот на будущее** — `docs/future-streaming-archives.md` (Zero-Storage Streaming Archives):
  - Дословная фиксация идеи из research-сессии + **доработанная архитектура** под существующие модули poler-engine (`streaming.rs`, `resonance/`, `web/simhash.rs`, `tokenizer/pii.rs`, `aidde/`, `psi.rs`).
  - 4 целевые аудитории: обучение локальных LLM, RAG-context injection для готовых LLM, помощь человеку (TUI discovery), параллельный поиск по конкретным и смежным темам (rayon `par_iter`).
  - Полная математика: топологическая адресация (zip: O(δ) ≈ 64 КБ), ε(W_k), IIR R[n], SimHash F(d), энтропия Шеннона H(W_k), POLER[Ψ] ψ-поле с importance sampling.
  - Roadmap SA1–SA7 (после v0.18.0).

### Совместимость

- 521 unit-test зелёных (+8 M3 routing tests +3 M2 helper tests; v0.17.1 было 510).
- Бинарь 12 МБ (стабилен с v0.17.1).
- База данных `web-index.db` — без изменений схемы (M2-M4 не трогают хранилище).
- Исторические маркеры `// v0.17.0:` сохранены как источник правды о том, какая фича в каком релизе добавлена.

## Установка

```bash
# Распаковать
tar -xzf poler-engine-v0.17.3-linux-x86_64.tar.gz

# Установить в ~/.local/bin
install -m 0755 poler-engine ~/.local/bin/

# Проверить
poler-engine --version
# poler-engine 0.17.3
```

## Companion Bridge: первый запуск

```bash
# 1. OAuth consent screen в Google Cloud Console (тип: External).
#    Scopes: gmail.readonly + drive.readonly + cloud-platform.
#    Добавить пользователя в "Test users" (Pre-GA).

# 2. Скачать OAuth client_secret JSON и положить:
mkdir -p ~/.config/poler-engine
chmod 600 client_secret.json
mv client_secret.json ~/.config/poler-engine/

# 3. Авторизоваться (со scope cloud-platform — для оф. NotebookLM API):
poler-engine --gcp-auth

# 4. Установить POLER_GCP_PROJECT_NUMBER (число из GCP Console):
export POLER_GCP_PROJECT_NUMBER=123456789012

# 5. Тесты Companion Bridge:
poler-engine --google-gmail "Ollama"   # Gmail (v0.16.0 фича)
poler-engine --google-drive            # Drive  (v0.16.0 фича)
# M2: GcpEnterpriseProvider.list_notebooks — оф. Pre-GA API
#     (после релиза M5 CLI subcommands: poler-engine nlm list-enterprise)
```

## Архитектура Companion Bridge

```text
shell/TUI/CLI  →  HybridProvider  →  GcpEnterpriseProvider (оф. Pre-GA)
                                  ↘  CdpBatchexecuteProvider (существ. nlm.rs)

Routing policy:
  GcpOnly   → primary=GCP, fallback OFF
  CdpOnly   → primary=CDP, fallback OFF
  Auto      → primary=GCP если gcp.supports(op), иначе CDP
              fallback ON: NotSupported/NotConfigured → secondary (если supports(op))
              Http/Transport/Parse → propagate без fallback
```

## Источник

Полные записи работы — в `worklog.md`:
- v0.18.0-m2-gcp-real-calls
- v0.18.0-m3-hybrid-routing
- v0.18.0-m4-tui-enter-handler
- v0.17.3-release-pack
