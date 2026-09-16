# os/ — криптографическое ядро POLER (поглощённый репозиторий poler-os)

> **Статус (2026-09-17, консолидация M3):** репозиторий
> `poler-engine-org/poler-os` влит в этот монорепозиторий целиком —
> **все 147 коммитов истории сохранены** (`git log -- os/`).
> Владелец принял стратегическое решение: POLER — не отдельная ОС под BIOS,
> а **единый суверенный инструмент работы с данными**, работающий на любом
> готовом Linux (Arch, Ubuntu, CachyOS, Debian). BIOS/Ring-0 модули
> заморожены. Этот каталог — то, что от OS-ветки живёт и развивается дальше.

---

## Что здесь сейчас

| Путь | Что это | Роль в едином инструменте |
|---|---|---|
| `core/poler_core.zig` | Криптографическое и диффузионное ядро PND v8 (1881 строка, единственный импорт — `std`) | Статическая библиотека `libpoler_core.a` (C-ABI) для Rust-движка: `pndMix`, биекция `Φ`, LHCA, Feistel ×20, ctSbox, MDS |
| `core/build.zig` | Сборка ядра как userspace-библиотеки (Zig 0.14.0) | `zig build` → `libpoler_core.a`; `zig build test` → **23/23 crypto-теста зелёные** (проверено standalone на Linux) |
| `docs/` | Вся база знаний OS-ветки: аудит Шнайера A2Z (`SCHNEIER_AUDIT_A2Z.md`), math-sources, архитектурные doc'и, исторический README, `docs/kernel/` — спецификации ядра (SMP, legal-аудит) | Эпистемический субстрат: первоисточники для MVR-провенанса Гиппокампа |
| `AGENT.md`, `AGENT_STATE.md`, `LICENSE` | Исторические протоколы и лицензия репозитория-донора | Археология; читаются агентами как контекст решений |

## Что удалено из рабочего дерева (код живёт в истории)

Решение зафиксировано в чате владельца 2026-09-17: *«нужно убрать модули из zig кода для запуска этого на биос и сосредоточиться на том, что это инструмент работы с данными»*.

Удалены (найти можно через `git log -- os/zig-kernel` и предков merge-коммита):

- **`zig-kernel/`** — всё Ring-0 ядро: `boot*.S` (16/32/64-битные загрузчики),
  GDT/IDT/TSS, PMM/VMM (таблицы страниц), планировщик, SMP, ACPI, PCI,
  драйверы (VGA, virtio-blk/net/gpu, framebuffer, evdev, PS/2), FAT32,
  squashfs, VFS, ELF/PE-загрузчики, Win32-стабы, `linux_syscalls`,
  pacman, live-orchestrator, DRM/KMS, testdata. **Исключение:** ядро
  шифра `poler_core.zig` — извлечено в `core/` (32- и 64-битные копии
  были байт-идентичны).
- **`scripts/`** — сборка ISO, QEMU-окружение, e2e-харнессы ядра, паковка
  rootfs.
- **`userspace/`** — init, sh, композитор, шрифт OS.
- **`iso/`** — GRUB-конфигурации.

## Что отфильтровано из истории совсем (не код)

Незкодовые артефакты исключены при переносе истории (`git filter-repo`,
`--prune-empty never` — ни один коммит не потерян, 147/147 сообщений,
авторов и дат сохранены). Они остаются доступными в архивном репозитории
[`poler-engine-org/poler-os`](https://github.com/poler-engine-org/poler-os) на GitHub:

- `qemu-portable/` — портативный QEMU + BIOS-ROM'ы (75 МБ);
- `cachyos-root/` — rootfs для live-ISO;
- `upload/` — загруженные архивы (снапшоты, xorriso, qemu.zip);
- `logs-p11/` — отладочные логи JIT-волны (~187 МБ);
- `zig-kernel/testdata/pacman/` — бинарные пакеты-фикстуры.

## Сборка ядра

```bash
cd os/core
zig build test          # 23 встроенных crypto-теста (Feistel, phi, SAC, LHCA)
zig build               # zig-out/lib/libpoler_core.a
zig build -Doptimize=ReleaseFast
```

Тулчейн: Zig 0.14.0 (как в исходном poler-os).

## Дальнейший путь

Следующие шаги для этого каталога — в `docs/UNIFIED_ARCHITECTURE.md`
(корень монорепозитория): M4 — C-ABI export-обвязка + FFI-мост в Rust +
P0-фиксы аудита Шнайера (расписание ключей `key[4..7]` → полный 256-бит,
замена PolerPrng на DRBG, IV/nonce для каскада) с верификацией на
golden-векторах `tools/verifiers/golden/`.
