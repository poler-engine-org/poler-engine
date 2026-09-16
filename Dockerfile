# syntax=docker/dockerfile:1
# ============================================================
# POLER-Engine: многоступенчатая сборка (промышленный образ)
# Итоговый образ ~120 МБ (debian-slim + статический бинарник)
# ============================================================

FROM rust:1.98-slim-bookworm AS builder
WORKDIR /build

# M2: квантовые крейты pqc/pqw живут в самом репозитории (crates/,
# единый Cargo Workspace) — контекст сборки больше не требует
# соседнего клона POLER-Quantum-RS_repo (см. docs/MERGE_PLAN.md).

# Слой зависимостей кэшируется отдельно от исходников движка:
# манифесты + крейты целиком (внешних зависимостей у pqc/pqw ноль).
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN mkdir -p src tests examples && \
    echo "pub fn _placeholder() {}" > src/lib.rs && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release 2>/dev/null || true

COPY src ./src
COPY tests ./tests
COPY examples ./examples
RUN touch src/lib.rs src/main.rs && \
    cargo build --release --locked -p poler-engine && \
    cargo test --release --quiet -p poler-engine

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/* && \
    useradd --system --create-home poler

COPY --from=builder /build/target/release/poler-engine /usr/local/bin/poler-engine

USER poler
WORKDIR /data

# Проверка работоспособности образа
RUN poler-engine --version

ENTRYPOINT ["poler-engine"]
CMD ["--help"]
