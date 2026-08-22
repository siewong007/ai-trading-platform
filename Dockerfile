# syntax=docker/dockerfile:1
# trading_platform — research-only build. Default command prints help;
# nothing here ever trades without explicit operator action.

FROM rust:1-slim AS build
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY config ./config
# cache deps across source edits
RUN mkdir -p .cargo && cargo fetch && cargo build --release

FROM debian:trixie-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/trading_platform /usr/local/bin/trading_platform
COPY config /app/config
WORKDIR /app
VOLUME ["/app/data"]
ENTRYPOINT ["/usr/local/bin/trading_platform"]
CMD ["--help"]
