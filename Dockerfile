# syntax=docker/dockerfile:1
FROM rust:1.99.1-trixie AS rust-build
RUN apt-get update && apt-get install -y --no-install-recommends cmake clang && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git \
    cargo build --release --locked -p bridge-server --bins

# Explicit target for the Linux regression suite; never included in the runtime image.
FROM rust-build AS test
COPY tests/fixtures/identity.json ./tests/fixtures/identity.json
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git \
    cargo test --workspace --locked

# Opt-in release workload; its executable is never copied into production images.
FROM test AS storage-measure-build
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/usr/local/cargo/git \
    cargo test --release --locked -p bridge-core --lib --no-run \
    && for candidate in target/release/deps/bridge_core-*; do \
         if [ -f "$candidate" ] && [ -x "$candidate" ]; then cp "$candidate" /tmp/bridge-storage-test; fi; \
       done \
    && test -x /tmp/bridge-storage-test

FROM oven/bun:1.3.11 AS ui-build
WORKDIR /src
COPY package.json bun.lock ./
COPY ui/package.json ./ui/package.json
RUN bun install --frozen-lockfile
COPY ui ./ui
RUN bun run build

FROM debian:trixie-slim AS server
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 bridge && useradd --uid 10001 --gid bridge --no-create-home bridge \
    && mkdir -p /var/lib/bridge /run/bridge-secrets \
    && chown 10001:10001 /var/lib/bridge /run/bridge-secrets \
    && chmod 0700 /var/lib/bridge /run/bridge-secrets
COPY --from=rust-build /src/target/release/bridge-server /src/target/release/bridge-admin /usr/local/bin/
COPY --from=ui-build /src/ui/dist /usr/share/codex-rc-bridge/ui
COPY LICENSE /usr/share/doc/codex-rc-bridge/LICENSE
LABEL org.opencontainers.image.licenses="MPL-2.0" \
      org.opencontainers.image.source="https://github.com/geniucker-dev/codex-rc-bridge"
USER 10001:10001
WORKDIR /var/lib/bridge
# These are paths only; key material is generated separately and mounted at runtime.
ENV BRIDGE_BIND=0.0.0.0:8787 \
    BRIDGE_UI_DIR=/usr/share/codex-rc-bridge/ui \
    BRIDGE_DATABASE=/var/lib/bridge/bridge.sqlite \
    BRIDGE_MASTER_KEY=/run/bridge-secrets/master.key
EXPOSE 8787
STOPSIGNAL SIGTERM
ENTRYPOINT ["bridge-server"]

FROM server AS storage-measure
COPY --from=storage-measure-build /tmp/bridge-storage-test /usr/local/bin/bridge-storage-test
# Use the same persistent-volume mount point and UID as the server, never /tmp tmpfs.
ENV TMPDIR=/var/lib/bridge
ENTRYPOINT ["bridge-storage-test", "--exact", "storage::capacity::sqlite_capacity", "--ignored", "--nocapture"]
