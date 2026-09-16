# Карта документации POLER Engine

> Этот файл — точка входа во всю документацию проекта. Если вы открыли репозиторий
> впервые, начните отсюда, затем идите в `ARCHITECTURE.md`.

POLER Engine — поисково-аналитический движок полного логического скоупа,
спроектированный как **инструмент для ИИ-агентов**: индексирует, находит,
отдаёт, связывает по метаданным. Не понимает контент, не генерирует текст,
не принимает решений — понимает ИИ, творит автор.

Принципы проекта (не нарушать): суверенный стек без облачных API, Rust + CPU
only, нативный инференс через собственное ядро `pqc`, банальность как сила
(как grep/SQL/git).

---

## Сводная таблица

| Документ | Что покрывает | Для кого |
|---|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | Целостная архитектура v2.0: слои, модули, потоки данных, интерфейсы | все, кто меняет код |
| [THEORY.md](THEORY.md) | Математический аппарат: POLER[Ψ], ε-плотность, IIR-резонанс R(t), RaBitQ, FSST, Teddy, R1CS, Trit5 | чтобы понимать «почему так» |
| [HISTORY.md](HISTORY.md) | Генеалогия проекта (от лора Этерии до v2.0) и хроника разработки | все, кто хочет контекста |
| [quantum-eri.md](quantum-eri.md) | Квантовый мост, POLER-ERI v3.2.0, Reverse Meta-Compiler (`src/quantum/`) | инженеры SIMD/компиляторов |
| [MERGE_PLAN.md](MERGE_PLAN.md) | План слияния репозиториев в монорепозиторий POLER (**M2 исполнена** 2026-09-16: pqc/pqw в `crates/`, единый workspace) | архитекторы экосистемы |
| [MODULES.md](MODULES.md) | Справочник по 29 модулям `src/` с публичным API | навигация по кодовой базе |
| [CLI.md](CLI.md) | Референс всех режимов и флагов командной строки | пользователи и агенты |
| [TESTING.md](TESTING.md) | Философия тестирования: дифференциалы, golden, детерминизм | контрибьюторы |
| [formats/PQW_FORMAT.md](formats/PQW_FORMAT.md) | Спецификация формата весов `.pqw` v2 | конвертеры, ядро pqc |
| [formats/PRBQ_FORMAT.md](formats/PRBQ_FORMAT.md) | Спецификация хранилища квантованных векторов PRBQ v1 | векторный субстрат |
| [formats/WEB_INDEX_FORMAT.md](formats/WEB_INDEX_FORMAT.md) | Схема SQLite веб-индекса (pages/terms/links/hosts) | веб-краулер, --web-search |
| [../GLOSSARY.md](../GLOSSARY.md) | Глоссарий терминологии POLER (ε, R(t), русла J, сцены, архетипы…) | все |
| [../CONTRIBUTING.md](../CONTRIBUTING.md) | Как контрибьютить: процесс, инварианты, дисциплина коммитов | контрибьюторы |
| [../INSTALL.md](../INSTALL.md) | Сборка (включая path-зависимость POLER-Quantum-RS), конвертация моделей | первый запуск |

## Специализированные документы

| Документ | Тема |
|---|---|
| [POLER_REVERSE_META_COMPILER.md](POLER_REVERSE_META_COMPILER.md) | Англоязычная техспецификация meta_compiler (параллельная сессия): Reverse/Meta-стадии, dual execution targets, 31 400 проходов/сек — дополнение к [quantum-eri.md](quantum-eri.md) |
| [MONOREPO_CONSOLIDATION_PLAN.md](MONOREPO_CONSOLIDATION_PLAN.md) | Англоязычный мастер-план монорепо (параллельная сессия): дерево crates/ — дополнение к [MERGE_PLAN.md](MERGE_PLAN.md) (фазы M0–M5 и исторический контекст — там) |
| [COGNITIVE_ARCHITECTURE_299_SOURCES_SYNTHESIS.md](COGNITIVE_ARCHITECTURE_299_SOURCES_SYNTHESIS.md) | Синтез теоретических столпов из 299 источников (FEP, резонанс фаз) — дополнение к [THEORY.md](THEORY.md) §1–2 и [HISTORY.md](HISTORY.md) (карта тем архива — там) |
| [ENCYCLOPEDIA_299_SOURCES.md](ENCYCLOPEDIA_299_SOURCES.md) | **Полная энциклопедия и реестр всех 299 первоисточников** (215 КБ): сквозной каталог с #001 по #299 с описанием тем, математики и привязки к коду |
| [mathematical-treatise/VOLUME_I_QUANTUM_DYNAMICS_AND_ROTORS.md](mathematical-treatise/VOLUME_I_QUANTUM_DYNAMICS_AND_ROTORS.md) | **Трактат Том I:** Квантовая динамика, кососимметричный ротор J=U-Uᵀ, закон сохранения энергии d/dt∥ψ∥²=0, мера Борна |
| [mathematical-treatise/VOLUME_II_R1CS_SIMD_META_COMPILER_ALGEBRA.md](mathematical-treatise/VOLUME_II_R1CS_SIMD_META_COMPILER_ALGEBRA.md) | **Трактат Том II:** Алгебра R1CS, битовые маски VectorGate8 {-1,0,+1}, AVX2 без умножений и коммутативный граф CSE |
| [mathematical-treatise/VOLUME_III_RABITQ_AND_HIGH_DIMENSIONAL_GEOMETRY.md](mathematical-treatise/VOLUME_III_RABITQ_AND_HIGH_DIMENSIONAL_GEOMETRY.md) | **Трактат Том III:** 1-битные метрические пространства RaBitQ, вращение Уолша-Адамара, несмещённая оценка arcsin-MLE |
| [mathematical-treatise/VOLUME_IV_ACTIVE_INFERENCE_AND_ENERGY_DYNAMICS.md](mathematical-treatise/VOLUME_IV_ACTIVE_INFERENCE_AND_ENERGY_DYNAMICS.md) | **Трактат Том IV:** FEP Фристона, функционал свободной энергии, Z-преобразование и передаточная функция IIR R(t) |
| [mathematical-treatise/VOLUME_V_NONLINEAR_CRYPTOGRAPHIC_DIFFUSION_PND_V8.md](mathematical-treatise/VOLUME_V_NONLINEAR_CRYPTOGRAPHIC_DIFFUSION_PND_V8.md) | **Трактат Том V:** Нелинейная криптографическая диффузия PND v8, биективность ARX Φ(x), уничтожение линейных путей δ≤8 |
| [MVR_PROTOCOL.md](MVR_PROTOCOL.md) | **POLER-MVR-v3 — протокол тотальной инструментальной верификации:** фазы 0–6 (археология источника → CAS/SMT → побитовая сверка с кодом → тест/бенчмарк → фиксация аксиомы), жизненный цикл инструментов LOADED/STANDBY/EVICTED, трассировка theorem_id → verifier → commit → file#L |
| [../tools/verifiers/REGISTRY.md](../tools/verifiers/REGISTRY.md) | **Реестр верификаторов:** циклы A–E (Z3/SymPy/NumPy против кода), Rust-тесты, рождённые верификацией, честные находки (Σθ-неинвариантность precess_step, семантика fp.eq, домен теоремы II.1) |
| [algebra_of_sense_trit5.md](algebra_of_sense_trit5.md) | Алгебра смысла и кодек Trit5 (краткая версия, см. THEORY.md §6) |
| [terminal-gateway-architecture.md](terminal-gateway-architecture.md) | Terminal Gateway v0.22–0.28: двойной контур исполнения, sandbox, root broker |
| [native-retrieval-analysis.md](native-retrieval-analysis.md) | Разбор GNU grep / text-splitter → дизайн слоёв 0/B/S |
| [future-streaming-archives.md](future-streaming-archives.md) | Zero-Storage Streaming Archives (дизайн-нок будущего) |
| [research/dialogue_tool_vs_ai.md](research/dialogue_tool_vs_ai.md) | Ключевой тезис «инструмент, не ИИ» (основа PLAN_POLER_V2 Part B) |
| [research/](research/) | SOTA-сурвеи 2026: semantic search, code+agentic, RAG+KG, streaming NLP |
| [sources-archive/](sources-archive/) | ZIP: 299 первоисточников «Когнитивная архитектура семантического резонанса» — полная интеллектуальная история POLER (карта тем — в HISTORY.md, приложение A) |
| [chat_dialogue.md](chat_dialogue.md) | Санитизированный транскрипт сессии-марафона 2026-09-03…15 (10 МБ; выжимка — HISTORY.md; сам файл — первоисточник, не документация) |

## Корневые документы

| Документ | Тема |
|---|---|
| [../README.md](../README.md) | Чейнджлог и витрина возможностей (новейшие релизы сверху) |
| [../PLAN_POLER_V2.md](../PLAN_POLER_V2.md) | Мастер-план v2.0: 12 разделов + Parts B–F (стратегия суверенного стека) |
| [../FUTURE_ROADMAP.md](../FUTURE_ROADMAP.md) | Дальняя дорога: физика ПК, Streaming Archives, монетизация |
| [../SKILL.md](../SKILL.md) | Манифест скилла poler-engine для ИИ-агентов (принципы, сборка, CLI) |
| [../AGENT.md](../AGENT.md) | Протокол агента репозитория (context-free resilience) |
| [../AGENT_STATE.md](../AGENT_STATE.md) | Машиночитаемый вектор движения: current_task → next_task |
| [../TERMS.md](../TERMS.md) | Юридические термины использования |

## Порядок чтения для нового агента/разработчика

1. `SKILL.md` — что это и чего делать нельзя (принципы).
2. `AGENT.md` + `AGENT_STATE.md` — протокол работы и текущее состояние.
3. `ARCHITECTURE.md` — как устроена система сейчас.
4. `MODULES.md` — куда смотреть по конкретной подсистеме.
5. `HISTORY.md` — почему система стала такой (контекст решений).
6. `THEORY.md` — математика, на которой всё держится.
7. `TESTING.md` + `CONTRIBUTING.md` — перед первым PR.

## Устаревшее (читать с оговорками)

- `companion-bridge-design.md` — описывает NotebookLM/Google-мост, **удалённый
  в v2.0** (суверенный стек). Оставлен как исторический дизайн-документ.
- Нижние секции `README.md` («Структура проекта», счётчики тестов) частично
  описывают v0.3.x — актуальную структуру смотрите в `MODULES.md`.
- `INSTALL.md` до сентября 2026 содержал инструкции Google OAuth — переписан.

## Поддержка актуальности

Документация — часть определения Done для каждого «кирпича» (см.
CONTRIBUTING.md). Правило простое: **изменил публичный API/формат/CLI — обновил
соответствующий документ в том же коммите**. Карта владения:

- новый флаг CLI → `CLI.md` (+ `--help` сам обновится из doc-комментариев);
- новый модуль или изменение pub-API → `MODULES.md` (+ `//!`-шапка модуля);
- изменение бинарного формата → `formats/*.md` с указанием новой версии магии;
- новый релиз/кирпич → `README.md` (верхняя секция) + `HISTORY.md`;
- изменение принципов → `SKILL.md` (согласовывать с владельцем).
