# The dibs server image. Build it from the repository root, which is the build context:
#
#   docker build -f deploy/Dockerfile -t dibs .
#
# The context is the whole workspace, `web/` included: the server embeds the browser bundle at
# compile time.

ARG RUST_VERSION=1.99.1

FROM rust:${RUST_VERSION}-bookworm AS build
WORKDIR /src
COPY . .
# The image's own toolchain builds the server. The pinned channel file would make rustup fetch
# the formatter and linter components, which a release build does not use.
RUN rm -f rust-toolchain.toml \
 && cargo build --release --locked -p dibs-server \
 && install -D target/release/dibs-server /out/dibs-server \
 && mkdir -p /out/data /out/snapshots

FROM gcr.io/distroless/cc-debian12:nonroot
LABEL org.opencontainers.image.title="dibs-server" \
      org.opencontainers.image.description="The dibs coordination server" \
      org.opencontainers.image.source="https://github.com/hryvinskyi/dibs" \
      org.opencontainers.image.licenses="MIT"
COPY --from=build /out/dibs-server /dibs-server
# The database and the snapshots are written by the unprivileged user the image runs as (65532);
# a named volume starts out owned by it, a bind-mounted directory must be handed to it.
COPY --from=build --chown=65532:65532 /out/data /data
COPY --from=build --chown=65532:65532 /out/snapshots /snapshots
ENV DIBS_DATABASE=/data/dibs.db \
    DIBS_SNAPSHOT_DIR=/snapshots
VOLUME ["/data", "/snapshots"]
EXPOSE 8080
USER 65532:65532
# The image has no shell, so it cannot carry a HEALTHCHECK: probe GET /health from the orchestrator.
ENTRYPOINT ["/dibs-server"]
