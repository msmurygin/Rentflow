# Сборка: docker build -t rentflow .
# Запуск: docker run -d --name rentflow --restart unless-stopped --env-file .env -v rentflow-data:/data rentflow
FROM rust:1.97-slim-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends build-essential pkg-config ca-certificates \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY migrations migrations
COPY src src
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home /data --uid 10001 rentflow && mkdir -p /data && chown rentflow /data
COPY --from=build /src/target/release/rentflow /usr/local/bin/rentflow
# SQLite и фото живут в томе /data — без него данные пропадут вместе с контейнером
ENV DATABASE_URL=sqlite:///data/rentflow.db PHOTO_DIR=/data/photos
VOLUME /data
USER rentflow
CMD ["rentflow"]
