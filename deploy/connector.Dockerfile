# Installs the published Debian package. No compiler and no private registry.
# Linux x86_64, glibc 2.35 or newer. Debian bookworm supplies glibc 2.36.
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl adduser \
    && curl -fL -o /tmp/connector-os_amd64.deb \
         https://github.com/GlobalSushrut/connector-oss/releases/download/prebuilt/connector-os_amd64.deb \
    && dpkg -i /tmp/connector-os_amd64.deb \
    && rm -rf /var/lib/apt/lists/* /tmp/connector-os_amd64.deb

ENV CONNECTOR_HOST=0.0.0.0 \
    CONNECTOR_PORT=9091 \
    CONNECTOR_DATA_DIR=/var/lib/connector \
    CONNECTOR_CONFIG_FILE=/etc/connector/connector.yaml \
    CONNECTOR_PRESET=local \
    CONNECTOR_ENV=development \
    CONNECTOR_DEV_MODE=1 \
    CONNECTOR_API_KEY=dev-token \
    RUST_LOG=info

COPY deploy/connector-start.sh /usr/local/bin/connector-start.sh
RUN chmod 755 /usr/local/bin/connector-start.sh

EXPOSE 9091
WORKDIR /var/lib/connector

HEALTHCHECK --interval=15s --timeout=3s --start-period=45s --retries=5 \
  CMD curl -fsS "http://127.0.0.1:${CONNECTOR_PORT:-9091}/healthz" || exit 1

ENTRYPOINT ["/usr/local/bin/connector-start.sh"]
