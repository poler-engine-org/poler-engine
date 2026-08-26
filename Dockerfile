# syntax=docker/dockerfile:1
# ============================================================
# POLER-Engine: многоступенчатая сборка (промышленный образ)
# Итоговый образ ~120 МБ (debian-slim + статический бинарник)
# ============================================================

FROM rust:1.98-slim AS builder
WORKDIR /build

# Слой зависимостей кэшируется отдельно от исходников
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "pub fn _placeholder() {}" > src/lib.rs && \
    mkdir -p tests examples && \
    cargo build --release 2>/dev/null || true

COPY src ./src
COPY tests ./tests
COPY examples ./examples
RUN touch src/lib.rs src/main.rs && \
    cargo build --release --locked && \
    cargo test --release --quiet

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
