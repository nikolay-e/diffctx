# syntax=docker/dockerfile:1.27@sha256:bde3983e9c939224420ddaf6b784cc30e09b035a4dea01f581230c50809f372e
# Docker Hardened Images from dhi.io itself (the CI jobs log in with a read-only token; the digests
# are the index digests dhi.io serves). The -dev variant builds; the git runtime
# has no shell and no package manager, and DHI rebuilds it when a package is fixed.
FROM dhi.io/rust:1.99.1-debian13-dev@sha256:be6c6ec91c61ae15b80521052df1c7f60fefd303293563b3dbd9f57e58783809 AS builder

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
RUN --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    --mount=type=cache,target=/build/target,sharing=locked \
    cargo build --release --locked --bin diffctx \
    && cp /build/target/release/diffctx /usr/local/bin/diffctx

# Bind-mounted host repositories carry foreign ownership; without the safe.directory entry git
# refuses to read them ("dubious ownership") and every --diff run fails. The runtime has no shell
# to run `git config`, so the system gitconfig is written here.
RUN printf '[safe]\n\tdirectory = *\n' > /etc/gitconfig

FROM dhi.io/git:2.47.3-debian13@sha256:2a26cd1cca8ee949ebfc0421e838b4195336acc9c2d7add14dee53f3ce2c7e51 AS runtime

COPY --from=builder /etc/gitconfig /etc/gitconfig
COPY --from=builder /usr/local/bin/diffctx /usr/local/bin/diffctx

# Last: VERSION changes on every release, and an ARG invalidates every RUN below it.
ARG VERSION=0.0.0
LABEL org.opencontainers.image.title="diffctx" \
      org.opencontainers.image.description="Selects the minimum code an LLM needs to review a git diff" \
      org.opencontainers.image.source="https://github.com/nikolay-e/diffctx" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="${VERSION}"

# No passwd entry for the uid: git needs no home, and HOME points at a path nothing writes.
ENV HOME=/tmp
USER 10001:10001

WORKDIR /repo
ENTRYPOINT ["diffctx"]
CMD ["--help"]
