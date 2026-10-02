# BUILDING — сборка POLER Engine (канонический протокол)

> Цель: собрать движок на любой машине — от рабочей станции до Core i5
> с 4 ГБ RAM. Протокол ниже — **канонический**: золотое правило `-j1`,
> debug-профиль для проверок, release — только для бинарника, без
> каких-либо переопределений профиля. Быстрая карта: [README.md](README.md) ·
> вклад: [CONTRIBUTING.md](CONTRIBUTING.md) (коммиты/PR — §3.5) ·
> тесты: [docs/TESTING.md](docs/TESTING.md).

---

## 1. Что понадобится

| Требование | Минимум | Примечание |
|---|---|---|
| ОС | Linux x86-64 (glibc) | Ubuntu / ядро 5.x–6.x |
| Rust | ≥ 1.80 (`rust-version` в Cargo.toml) | проверено на 1.99.0 stable |
| RAM | **4 ГБ** при `-j1` | 8+ ГБ → можно `-j2` (как CI) |
| Свободное место | ~5 ГБ | `target/` release-артефакты |
| Сеть | только первый `cargo build` | далее `--offline` работает |

Установка Rust (если нет):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"          # или новый терминал
rustc --version                    # должно быть >= 1.80
```

## 2. Получение исходников

```bash
git clone https://github.com/poler-engine-org/poler-engine.git
cd poler-engine
git log --oneline -1               # убедись, что на свежем main
```

Проект — гибрид package+workspace: корневой пакет `poler-engine` плюс крейты
`crates/{pqc,pqw,reader,poler-ffi}`. Свежий clone + cargo build работает
из коробки, ничего дополнительно настраивать не надо.

---

## 3. Канонический протокол сборки

### Шаг 1 — быстрая проверка и тесты (debug-профиль, LTO не нужен)

Проверка корректности кода и математика собираются быстро и легко по памяти —
релизный профиль для этого не требуется:

```bash
cargo check --workspace
cargo test -p poler-engine --lib calc::matrix::tests::eigen_sturm   # Штурм-спектр
cargo test -p pqc                                                   # квантовое ядро
```

### Шаг 2 — релизный бинарник

```bash
cargo build --release -p poler-engine -j1
```

> **Золотое правило памяти.** В манифесте включена максимальная оптимизация:
> LTO + `codegen-units = 1` + `opt-level = 3`. Если свободно меньше 8–16 ГБ
> RAM, параллельная сборка всеми ядрами может словить OOM Killer на этапе
> линковки. Правило: **всегда `-j1`** (или `-j2` на 8+ ГБ). Профиль не
> переопределяется — никаких `CARGO_PROFILE_RELEASE_*` переменных:
> канонический бинарник один, и он full-LTO.

### Шаг 3 — установка и обязательный чек-лист

```bash
cp -f target/release/poler-engine ~/.local/bin/poler-engine
```

Чек-лист после сборки (все три пункта обязательны):

1. `poler-engine --version` → актуальная версия; `poler-engine --schema`
   возвращает чистый JSON.
2. `poler-engine --exec 'calc eigen_sturm(tridiag(2,-1,256))' --json` —
   спектр 256×256 за ≤ 60 мс.
3. `poler-engine --triune-speak 'привет' --triune-tokens 12` — отклик
   Триединства без сбоев.

---

## 4. Все 37 MCP-инструментов (pnd-ffi, Zig-ядро)

Если нужен прямой мост к крипто-ядру PND и системный пул `poler_exec*`
(32 базовых инструмента → 37):

```bash
cargo build --release -p poler-engine --features pnd-ffi -j1
```

Требует Zig-тулчейн (0.14+ / 0.16, `os/core/`). Опционально: для базовой
сборки Zig **не нужен**.

## 5. AI-шлюз poler-api (совсем без Cargo)

`tools/ai-gateway/poler-api.rs` — автономный шлюз над установленным
бинарником (schema-манифест + подкоманды + MCP-прокси 34 инструмента),
чистый std, собирается голым `rustc` за секунды на любом железе:

```bash
rustc -O tools/ai-gateway/poler-api.rs -o ~/.local/bin/poler-api
poler-api selftest   # 15/15 PASS = готово
```

## 6. Запуск через poler-box (изоляция ядра, Zero-Disk)

Канонический путь запуска собранного движка — в микро-контейнере без
Docker и ОС. Утилиты контейнеризации живут в монорепозитории `poler`
(`crates/poler-box`, `crates/poler-archive`):

```bash
# 1) сборка poler-box
cd poler && cargo build --release -p poler-box -j1
cp -f target/release/poler-box ~/.local/bin/poler-box

# 2) упаковка движка в защищённый .poler-контейнер (Zstd/FastCDC/BLAKE3)
poler pack engine_container.poler ~/.local/bin/poler-engine

# 3) запуск расчёта внутри изоляции с жёсткими лимитами
poler-box run --rss-mb 256 --cpu-s 30 --tmpfs-mb 128 \
  engine_container.poler poler-engine \
  --exec 'calc eigen_sturm(tridiag(2,-1,256))' --json
```

Под капотом: `unshare -Ur` + namespaces (pid/mnt/net/ipc/uts) — движок
получает PID 1; бинарник стримится из архива прямо в анонимную память
(`memfd_create` + `execveat`) — на диск не пишется ни байта; seccomp —
белый список системных вызовов; губернатор обрывает процесс при
превышении RSS/CPU-бюджета без вреда хосту.

## 7. Timings (реальный слабый хост: 2 vCPU, 4 ГБ RAM, `-j1`)

| Этап | Время |
|---|---|
| `cargo check --workspace` | ~2 м 16 с |
| `cargo test -p poler-engine --lib eigen_sturm` | 2 passed / 0 failed (0.30 с на прогоны) |
| Release-сборка `-p poler-engine -j1` (тёплый `target/`) | **8 м 48 с** |
| `rustc -O poler-api.rs` (шлюз) | ~10 с |

Чек-лист на этом же хосте: `eigen_sturm(tridiag(2,-1,256))` — **36.5 мс**
(лимит канона 60 мс), det(T₂₅₆) = 257.0, trace = 512.0, `--schema` — чистый
JSON, `--triune-speak` — без сбоев.

## 8. Траблшутинг

**OOM Killer на линковке** (`dmesg | grep -i 'killed process'`):
1. Убедись, что сборка идёт с `-j1` (§3, шаг 2) — это канон и 90% случаев.
2. Добавьте swap (самый сильный приём для слабых машин):

```bash
sudo fallocate -l 4G /swapfile && sudo chmod 600 /swapfile
sudo mkswap /swapfile && sudo swapon /swapfile
# отключить после сборки: sudo swapoff /swapfile && sudo rm /swapfile
```

**CI-требование `--locked`**: если меняли `Cargo.toml` (версия/зависимости),
локально запустите любой `cargo build` — он обновит `Cargo.lock`, и **оба
файла коммитьте вместе** (иначе CI упадёт на `--locked`).

**Место кончилось**: `cargo clean` — `target/` пересоберётся с нуля.

**Сеть недоступна для crates.io**: `cargo build --offline` работает, если
зависимости уже закешированы в `~/.cargo` хотя бы раз.

---

> Документ добавлен в v0.62.0 (AI-API реформа). Канон протокола сборки —
> владельческий: `-j1`, профиль не трогать, чек-лист из трёх пунктов
> обязателен после каждой пересборки.
