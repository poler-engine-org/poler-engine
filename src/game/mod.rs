//! # Ядро Игры (цикл S, v0.54.0)
//!
//! Фундамент игрового движка POLER — ответ на разбор недостатков
//! UE/Unity/Godot ([`docs/GAME_ENGINE_ROADMAP_UE_ANALYSIS.md`]).
//!
//! ```text
//!   UE (C++)                          POLER GAME CORE (Rust)
//!   ──────────                         ──────────────────────
//!   UObject + GC                →      Entity(index, generation):
//!                                       слот мёртвого не воскресает,
//!                                       GC не существует конструктивно
//!   AActor + UActorComponent    →      World + плотные компонентные
//!                                       хранилища (Option<T> на слот)
//!   Tick groups (Pre/Post…)     →      фазы систем в фиксированном тике
//!   Level / World               →      Scene (serde JSON → .poler потом)
//!   Replication                 →      state-hash: детерминизм бит-в-бит
//!   .pak ассеты                  →      .poler-контейнер (CDC-дедуп)
//!   Рендер RHI/RDG              →      P³ FFI: RGB+depth+seg, d_FS
//! ```
//!
//! Пайплайн тика:
//!
//! ```text
//!   GameClock::advance(real_s) → accumulator → N целых тиков
//!         │
//!         ▼  World::tick(dt) — фиксированный порядок фаз:
//!   1. Orbit     angle += ω·dt; локальная позиция = R(наклон)·[r·cosα, 0, r·sinα]
//!   2. Transform DFS от корней: world = parent.world + local (иерархия)
//!   3. Hash      FNV-1a по (слот, поколение, world_pos биты) — маяк
//!               детерминизма: одинаковый вход → бит-в-бит тот же мир
//! ```
//!
//! Кеплер-лайт честность: угловая скорость орбиты выводится из массы
//! родителя — ω = √(G·M_parent)/r^1.5 (третий закон Кеплера), а не
//! подбирается «на глаз» как в бутафорских движках.
//!
//! Рендер headless-first: кадр = тройка PNG (RGB/depth/seg) через
//! C-ABI в ядро P³ — тот же путь, что у «кадра из гамильтониана».

pub mod audio;
pub mod loop_;
pub mod orbit;
pub mod render;
pub mod scene;
pub mod texture;
pub mod transform;
pub mod world;

pub use loop_::{GameClock, FIXED_DT};
pub use orbit::Orbit;
pub use render::{render_frame, FrameConfig, FrameOutput};
pub use scene::{demo_scene, BodySpec, CameraSpec, SceneFile};
pub use transform::Transform;
pub use world::{Body, BodyClass, Entity, World};
