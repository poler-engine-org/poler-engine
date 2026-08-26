//! AIDDE — AI-Interpreted Dependency & Impact Engine.
//!
//! Ответ на слепоту grep/RAG и «близорукость» линейного интерпретатора:
//! движок строит **сквозной граф зависимостей** между файлами проекта и
//! выдаёт Impact Passport — что сломается, если изменить или удалить
//! символ (upstream), и от чего неявно зависит сам символ (downstream).
//!
//! * **Уровень 1** — глобальная таблица символов: определения (`fn`,
//!   `struct`, `class`, `def`, …) с файлами и строками + вызовы + импорты;
//! * **Уровень 2** — call graph, разрешённый через таблицу символов
//!   (межфайловые связи: `pipeline.rs` —вызывает→ `allocator.rs`);
//! * **Уровень 3** — двунаправленный impact-анализ (BFS по прямым и
//!   обратным рёбрам) + эвристики сайд-эффектов + danger level.

pub mod impact;
pub mod symbols;

pub use impact::{impact_analysis, Dependency, Dependent, ImpactReport};
pub use symbols::{CallSite, Definition, ImportStmt, SymbolTable};
