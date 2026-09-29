# syntax=docker/dockerfile:1.27@sha256:bde3983e9c939224420ddaf6b784cc30e09b035a4dea01f581230c50809f372e
FROM rust:1.92-bookworm@sha256:e90e846de4124376164ddfbaab4b0774c7bdeef5e738866295e5a90a34a307a2 AS builder

WORKDIR /build
COPY README.md ./
COPY Cargo.toml Cargo.lock ./
COPY crates/diffctx-native/Cargo.toml ./crates/diffctx-native/
COPY crates/diffctx-native/src ./crates/diffctx-native/src
COPY crates/diffctx-native/tests ./crates/diffctx-native/tests

# Read by option_env! at compile time, so `provenance.engine.build` names the
# commit. A build arg reaches RUN as an env var only when it was passed; an ENV
# would set it to "" on a plain local build.
ARG DIFFCTX_BUILD_SHA
WORKDIR /build/crates/diffctx-native
# target/ is a cache mount so dependencies compile once, not per commit; it is
# absent from the layer, hence the copy out in the same RUN.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --locked --bin diffctx \
    && cp /build/target/release/diffctx /usr/local/bin/diffctx

FROM debian:bookworm-slim@sha256:3783cc01769c7b2b1b83a5c5ad96c815348e28ed7da68e2e3687004faa906251 AS runtime

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates git \
    && rm -rf /var/lib/apt/lists/*

# Keyed to the UTC day CI passes, so an unchanged Dockerfile still picks up
# Debian security fixes instead of reusing the upgrade layer forever.
ARG APT_REFRESH=unset
# Bind-mounted host repositories carry foreign ownership; without the
# safe.directory entry git refuses to read them ("dubious ownership") and
# every --diff run fails. It rides in the same layer as the upgrade: both are
# cheap, and one layer keyed to the day is one cache entry, not two.
RUN echo "security upgrade keyed to ${APT_REFRESH}" \
    && apt-get update \
    && DEBIAN_FRONTEND=noninteractive timeout 300 apt-get upgrade -y --no-install-recommends \
    && rm -rf /var/lib/apt/lists/* \
    && git config --system --add safe.directory '*' \
    && useradd --system --uid 10001 --create-home diffctx

COPY --from=builder /usr/local/bin/diffctx /usr/local/bin/diffctx

# Last: VERSION changes on every release, and an ARG invalidates every RUN below it.
ARG VERSION=0.0.0
LABEL org.opencontainers.image.title="diffctx" \
      org.opencontainers.image.description="Selects the minimum code an LLM needs to review a git diff" \
      org.opencontainers.image.source="https://github.com/nikolay-e/diffctx" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}"

USER 10001:10001

WORKDIR /repo
ENTRYPOINT ["diffctx"]
CMD ["--help"]
