# Build stage
FROM rust:1-slim-bookworm AS build

WORKDIR /src

# Compile dependencies against a stub main so the (slow) dependency build is
# cached and only re-runs when Cargo.toml/Cargo.lock change.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src templates \
    && echo 'fn main() {}' > src/main.rs \
    && touch templates/dashboard.html \
    && cargo build --release --locked --bin robovac \
    && rm -rf src templates

COPY src ./src
COPY templates ./templates

# `touch` invalidates the stub's fingerprint so the real sources are compiled.
RUN touch src/main.rs && cargo build --release --locked --bin robovac

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --create-home --shell /usr/sbin/nologin robovac

COPY --from=build /src/target/release/robovac /usr/local/bin/robovac

USER 10001
EXPOSE 8080

ENV BIND=0.0.0.0:8080 \
    RUST_LOG=info,robovac=debug

# No curl in the image, so the binary probes itself.
HEALTHCHECK --interval=30s --timeout=5s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/robovac", "--healthcheck"]

ENTRYPOINT ["/usr/local/bin/robovac"]
