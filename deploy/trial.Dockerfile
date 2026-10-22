# Public trial image. CasePilot, Connector OS, and Surreal share one
# container so a free Render web service can run the desk.
FROM node:22-bookworm AS web
WORKDIR /src
COPY package.json package-lock.json ./
COPY web ./web
RUN npm ci && npm run build

FROM rust:1-bookworm AS build
WORKDIR /src
ENV CARGO_BUILD_JOBS=1
COPY Cargo.toml Cargo.lock ./
COPY build.rs ./
COPY src ./src
COPY web ./web
COPY deploy ./deploy
RUN cargo build --release

FROM surrealdb/surrealdb:v2.2.1 AS surreal

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl adduser libssl3 \
    && curl -fL -o /tmp/connector-os_amd64.deb \
         https://github.com/GlobalSushrut/connector-oss/releases/download/prebuilt/connector-os_amd64.deb \
    && dpkg -i /tmp/connector-os_amd64.deb \
    && rm -rf /var/lib/apt/lists/* /tmp/connector-os_amd64.deb

COPY --from=surreal /surreal /usr/local/bin/surreal
COPY --from=build /src/target/release/casepilot /usr/local/bin/casepilot
COPY --from=web /src/web /opt/casepilot/web
COPY deploy/connector-start.sh /usr/local/bin/connector-start.sh
COPY deploy/trial.sh /usr/local/bin/casepilot-trial.sh
RUN chmod 755 /usr/local/bin/surreal /usr/local/bin/connector-start.sh /usr/local/bin/casepilot-trial.sh

ENV CASEPILOT_WEB=/opt/casepilot/web \
    CASEPILOT_DB=/tmp/casepilot/casepilot.db \
    CASEPILOT_SETUP=/tmp/casepilot/setup.json \
    CONNECTOR_API_URL=http://127.0.0.1:9091 \
    CONNECTOR_API_KEY=dev-token \
    CONNECTOR_HOST=127.0.0.1 \
    CONNECTOR_PORT=9091 \
    CONNECTOR_DATA_DIR=/tmp/connector \
    CONNECTOR_PRESET=local \
    CONNECTOR_ENV=development \
    CONNECTOR_DEV_MODE=1 \
    CONNECTOR_DEV_AGENT_CAP=64 \
    CONNECTOR_ALLOW_IN_PROCESS_EFFECTS=1 \
    SURREAL_URL=http://127.0.0.1:8000 \
    SURREAL_USER=root \
    SURREAL_PASS=root

EXPOSE 8088
ENTRYPOINT ["/usr/local/bin/casepilot-trial.sh"]
