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
//!
//! Цикл U (v0.56.0): события → ввод → камера → окно. Очередь с
//! коалесцингом (`events`), свёрнутое состояние (`input`), орбит-камера
//! с детерминированным демпфированием (`camera`), оконные бэкенды
//! (`window`): Offscreen (replay-эталон, PNG+хеши) и X11 (dlopen,
//! zero-dep). Один и тот же код получает и CI-эталон, и настоящее окно.
//!
//! Цикл V (v0.57.0): вихревой кодек «Шеннон-байпас» (`vortex`) —
//! квантование шума фазовыми вихрями Навье–Стокса: 2D-FFT → топ-моды
//! Колмогорова → GF(3)-триты амплитуды/фазы (5 трит/байт, 3^5=243≤256)
//! → VRTX-контейнер. Честный зачёт против zstd-19 и порядкового
//! предела Шеннона; белый шум не сжимает никто — кодек это
//! показывает, а не скрывает.
//!
//! Цикл V1 (v0.58.0): спектральная гидродинамика (`water`) — вода как
//! состояние-спектр по разбору «OpenAI решила задачу»: без плотных
//! f32-сеток (Re^(9/4) точек) и без brute force — K волн с K41-равновесием,
//! дисперсией √(gk+γk³) и вязкостью Ламба; эволюция — целочисленные
//! трит-операции (счётчик фаз в фикс-точке, без дрейфа), синтез — FFT
//! по требованию, течения — аналитические. Приоритет перенаправлен
//! владельцем с BVH-коллизий (отложены) на физику воды.

pub mod audio;
pub mod camera;
pub mod events;
pub mod input;
pub mod loop_;
pub mod orbit;
pub mod render;
pub mod scene;
pub mod texture;
pub mod transform;
pub mod vortex;
pub mod water;
pub mod window;
pub mod world;

pub use camera::OrbitCamera;
pub use events::{Event, EventQueue, KeyPhase, MouseButton};
pub use input::{ActionMap, Bind, Input, KeyCode};
pub use loop_::{GameClock, FIXED_DT};
pub use orbit::Orbit;
pub use render::{render_frame, render_raw, FrameConfig, FrameOutput, RawFrame};
pub use scene::{demo_scene, BodySpec, CameraSpec, SceneFile};
pub use transform::Transform;
pub use window::{FrameRecord, OffscreenWindow, WindowBackend, X11Window};
pub use world::{Body, BodyClass, Entity, World};
