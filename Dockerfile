# Build stage
FROM rust:1.85-slim-bookworm AS builder
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

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y ca-certificates git curl openssh-client && \
    rm -rf /var/lib/apt/lists/*

# Install GitHub CLI
RUN curl -fsSL https://cli.github.com/packages/githubcli-archive-keyring.gpg \
      -o /usr/share/keyrings/githubcli-archive-keyring.gpg && \
    echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/githubcli-archive-keyring.gpg] https://cli.github.com/packages stable main" \
      > /etc/apt/sources.list.d/github-cli.list && \
    apt-get update && \
    apt-get install -y gh && \
    rm -rf /var/lib/apt/lists/*

# Install gitleaks for secret scanning
RUN curl -sSL https://github.com/gitleaks/gitleaks/releases/download/v8.21.2/gitleaks_8.21.2_linux_x64.tar.gz \
      | tar xz -C /usr/local/bin gitleaks

# Install Rust toolchain (needed for cargo fmt/clippy/test/build during self-improvement)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# Git config: identity, workspace trust, SSH for auth
RUN git config --global safe.directory /workspace && \
    git config --global user.name "saga" && \
    git config --global user.email "saga@zakstak.dev" && \
    git config --global core.hooksPath hooks

# SSH config: use the mounted key for github.com without host key prompts
RUN mkdir -p /root/.ssh && \
    echo "Host github.com\n  IdentityFile /root/.ssh/id_ed25519\n  StrictHostKeyChecking accept-new\n  UserKnownHostsFile /root/.ssh/known_hosts" > /root/.ssh/config && \
    chmod 700 /root/.ssh && chmod 600 /root/.ssh/config

# Copy the compiled binary
COPY --from=builder /usr/src/app/target/release/kley /usr/local/bin/kley

# Copy scripts and agent config
COPY .agents /app/.agents
COPY preflight.sh self-improve.sh /usr/local/bin/
RUN chmod +x /usr/local/bin/preflight.sh /usr/local/bin/self-improve.sh

ENTRYPOINT ["kley"]
CMD ["chat"]
