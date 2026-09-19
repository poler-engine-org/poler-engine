//! Модуль суверенного браузерного ядра POLER Browser Core (v0.38.0).
//!
//! Обеспечивает:
//! - Легковесный DOM-парсер без накладных расходов памяти (Zero-Copy DOM Tree).
//! - Квантовую фильтрацию рекламы и шума по плотности ε (Epsilon Semantic Filter).
//! - Прямую трансляцию содержимого веб-страниц в память Кристалла Trit5 (.t5c).
//! - Моторный интерфейс S2→E2 для автономного взаимодействия со страницами.

pub mod dom_tree;
pub mod filter;
pub mod session;

pub use dom_tree::{DomDocument, DomNode, NodeType};
pub use filter::{ContentFilter, FilterStats};
pub use session::{BrowserConfig, BrowserWindow, TabSession};

#[cfg(test)]
mod tests;

