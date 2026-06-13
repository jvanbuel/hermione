# Build the Hermione backend.
# The proto build step uses a vendored protoc, so no protobuf-compiler is needed.
FROM rust:1-bookworm AS builder
WORKDIR /app
COPY . .
RUN cargo build --release -p hermione-server

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/hermione-server /usr/local/bin/hermione-server

# The web viewer (HTML + vendored xterm.js) is embedded in the binary.
EXPOSE 8080 50051
ENV HERMIONE_GRPC_ADDR=0.0.0.0:50051 \
    HERMIONE_HTTP_ADDR=0.0.0.0:8080
ENTRYPOINT ["hermione-server"]
