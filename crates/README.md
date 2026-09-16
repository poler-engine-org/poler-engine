# crates/ — квантовое ядро POLER (всасано из POLER-Quantum-RS)

Фаза **M2** плана слияния (docs/MERGE_PLAN.md): крейты `pqc` и `pqw`
перенесены из `poler-engine-org/POLER-Quantum-RS` (v1.3.0) через
`git filter-repo --subdirectory-filter crates` + `git subtree add` —
**25 коммитов истории RQ1–RQ23 сохранены** (второй родитель merge-коммита,
`git log <merge>^2`; `git blame` проходит сквозь merge до исходных коммитов
Kotokvit).

| Крейт | Назначение |
|---|---|
| `pqc` | квантовое ядро: statevector, Ry(arccos p)-ансатц, Born-лотерея, решётка LENS, архетипическая алгебра ⊗_ε, curriculum-обучение, TLS-примитивы |
| `pqw` | формат весов `.poler`/`.pqw`: трит-квантование, sparse LENS-топология, mmap zero-copy, McWeeny-инвариант |

Свойства:

* **Внешние зависимости: ноль** (`pqc` → `pqw`, `pqw` → ничего) — слияние
  не добавило в дерево poler-engine ни одного внешнего крейта.
* **Лицензия крейтов: MIT** (см. `LICENSE` в этом каталоге) — независимо
  от source-available EULA корневого poler-engine (корень: LICENSE.md).
* **Версии**: квантовое ядро живёт на своей линии `1.x` (наследуется из
  `[workspace.package]` корневого Cargo.toml); poler-engine не наследует
  и живёт на `0.28.x`.
* **Профиль сборки**: release-профиль общий с корнем (LTO fat,
  codegen-units=1, panic=abort) — catch_unwind в крейтах нет.
* Тесты: `cargo test --workspace` (CI) гоняет сьюты pqc (~700+) и pqw
  (~90) вместе с движком; qiskit-паритет само-скипается без Python.

Старый репозиторий-источник `poler-engine-org/POLER-Quantum-RS` после
всасывания — архив-указатель на это место (фаза M2 плана слияния).
Дальнейшая эволюция структуры — M3: виртуальный манифест, движок переезжает
в `crates/poler-engine` (docs/MERGE_PLAN.md §3,
docs/MONOREPO_CONSOLIDATION_PLAN.md).
