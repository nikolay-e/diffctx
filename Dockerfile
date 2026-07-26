FROM rust:1.92-bookworm AS builder

WORKDIR /build
COPY README.md ./
COPY Cargo.toml Cargo.lock ./
COPY crates/diffctx-native/Cargo.toml ./crates/diffctx-native/
COPY crates/diffctx-native/src ./crates/diffctx-native/src
COPY crates/diffctx-native/tests ./crates/diffctx-native/tests

WORKDIR /build/crates/diffctx-native
RUN cargo build --release --locked --bin diffctx

FROM debian:bookworm-slim AS runtime

ARG VERSION=0.0.0
LABEL org.opencontainers.image.title="diffctx" \
      org.opencontainers.image.description="Selects the minimum code an LLM needs to review a git diff" \
      org.opencontainers.image.source="https://github.com/nikolay-e/diffctx" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}"

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/diffctx /usr/local/bin/diffctx

# Bind-mounted host repositories carry foreign ownership; without this git
# refuses to read them ("dubious ownership") and every --diff run fails.
RUN git config --system --add safe.directory '*' \
    && useradd --system --uid 10001 --create-home diffctx
USER 10001:10001

WORKDIR /repo
ENTRYPOINT ["diffctx"]
CMD ["--help"]
