# Сборка: docker build -t rentflow .
# Запуск: docker run -d --name rentflow --restart unless-stopped --env-file .env -v rentflow-data:/data rentflow
# Один образ: Rust-бот + HTTP API + собранный Mini App (Node нужен только на этапе сборки, в production его нет).

FROM node:22-slim AS web
WORKDIR /w
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
RUN npm run build

FROM rust:1.97-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends build-essential pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations migrations
COPY src src
RUN cargo build --release --locked --bin rentflow

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /data --uid 10001 rentflow && mkdir -p /data /app/web && chown rentflow /data
COPY --from=build /src/target/release/rentflow /usr/local/bin/rentflow
COPY --from=web /w/dist /app/web
# SQLite и фото живут в томе /data — без него данные пропадут вместе с контейнером
ENV DATABASE_URL=sqlite:///data/rentflow.db PHOTO_DIR=/data/photos WEB_DIR=/app/web HTTP_BIND=0.0.0.0:8080
VOLUME /data
EXPOSE 8080
USER rentflow
CMD ["rentflow"]
