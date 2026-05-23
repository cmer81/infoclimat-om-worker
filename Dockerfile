FROM rust:1.85-bookworm AS builder
WORKDIR /app

RUN apt-get update && apt-get install -y --no-install-recommends \
    cmake \
    clang \
    pkg-config \
    libssl-dev \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml ./
RUN mkdir src && echo 'fn main() {}' > src/main.rs && cargo fetch
COPY src ./src
RUN cargo build --release --locked && \
    cp target/release/infoclimat-om-worker /app/server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd -r -u 10001 -m -d /home/app app

COPY --from=builder /app/server /usr/local/bin/server
USER app
EXPOSE 8080
ENV RUST_LOG=info
ENTRYPOINT ["/usr/local/bin/server"]
