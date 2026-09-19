//! Поисковые подсистемы: глубокий сбор диска (harvester).

pub mod disk_harvester;

pub use disk_harvester::{run_harvest, HarvestConfig, HarvestFormat, HarvestMode, HarvestStats};
