FROM node:22-bookworm AS web
WORKDIR /src
COPY package.json package-lock.json ./
COPY web ./web
RUN npm ci && npm run build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
COPY web ./web
COPY deploy ./deploy
RUN cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl libssl3 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/casepilot /usr/local/bin/casepilot
COPY --from=web /src/web /opt/casepilot/web
ENV CASEPILOT_WEB=/opt/casepilot/web \
    CASEPILOT_BIND=0.0.0.0:8088 \
    CASEPILOT_DB=/data/casepilot.db \
    CASEPILOT_SETUP=/data/setup.json \
    CONNECTOR_API_KEY=dev-token
EXPOSE 8088
VOLUME ["/data"]
CMD ["casepilot"]
