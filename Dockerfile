# Production image for the Aurix Net node: static-ish release build, non-root, SIGTERM drains
# (rooms are checkpointed and handed to another node), /healthz drives the container healthcheck.
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p net-server --locked

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home /var/lib/aurix-net --create-home aurix
COPY --from=build /src/target/release/aurix-net-server /usr/local/bin/aurix-net-server
USER aurix
WORKDIR /var/lib/aurix-net
# HTTP/WS control plane, QUIC and WebTransport datagrams.
EXPOSE 8090/tcp 7000/udp 7443/udp
# Containers are reached through published ports, so the control plane must bind all interfaces
# (the binary defaults to 127.0.0.1:8090 for bare-metal dev runs).
ENV NET__SERVER__HTTP_ADDR=0.0.0.0:8090
# Configuration: built-in defaults, optionally `--config /etc/aurix-net/net.toml` (mount it),
# and NET__SECTION__KEY environment overrides (e.g. NET__AUTH__GRANT_SECRET).
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=10s --timeout=3s --start-period=10s --retries=3 \
    CMD curl -fsS http://127.0.0.1:8090/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/aurix-net-server"]
