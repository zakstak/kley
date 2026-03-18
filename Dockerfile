# Build stage
FROM rust:1.80-slim-bookworm AS builder
WORKDIR /usr/src/app

# Install dependencies required for building
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# Copy manifests and source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY .agents ./.agents

# Build the release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
WORKDIR /app

# Install CA certificates for HTTPS/WSS connections
RUN apt-get update && \
    apt-get install -y ca-certificates && \
    rm -rf /var/lib/apt/lists/*

# Copy the compiled binary
COPY --from=builder /usr/src/app/target/release/kley /usr/local/bin/kley

# Copy the skills/rules directory as the agent needs it at runtime
COPY .agents /app/.agents

# Run as non-root user for security
RUN useradd -m kleyuser && chown -R kleyuser:kleyuser /app
USER kleyuser

ENTRYPOINT ["kley"]
CMD ["chat"]
