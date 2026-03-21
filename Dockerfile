# Build stage
FROM rust:slim-bookworm AS builder
WORKDIR /usr/src/app

# Install dependencies required for building
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# Copy manifests and source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY templates ./templates
COPY assets ./assets
COPY .agents ./.agents

# Build the release binary
RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
WORKDIR /app

# Install core runtime deps + C/C++ build toolchain
RUN apt-get update && \
    apt-get install -y \
      ca-certificates git curl openssh-client \
      build-essential cmake pkg-config libssl-dev && \
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
RUN GITLEAKS_VERSION="$(curl -fsSL https://api.github.com/repos/gitleaks/gitleaks/releases/latest | grep -oE '"tag_name":\s*"v[^"]+"' | cut -d '"' -f4)" && \
    curl -fsSL "https://github.com/gitleaks/gitleaks/releases/download/${GITLEAKS_VERSION}/gitleaks_${GITLEAKS_VERSION#v}_linux_x64.tar.gz" \
      | tar xz -C /usr/local/bin gitleaks

# Install Rust toolchain + rust-analyzer LSP
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
RUN /root/.cargo/bin/rustup component add rust-analyzer
ENV PATH="/root/.cargo/bin:${PATH}"

RUN curl -fsSL https://deb.nodesource.com/setup_current.x | bash - && \
    apt-get install -y nodejs && \
    rm -rf /var/lib/apt/lists/*

RUN GO_VERSION="$(curl -fsSL https://go.dev/VERSION?m=text | head -n1)" && \
    curl -fsSL "https://go.dev/dl/${GO_VERSION}.linux-amd64.tar.gz" \
      | tar -C /usr/local -xz
ENV PATH="/usr/local/go/bin:/root/go/bin:${PATH}"

# Go tools: LSP + linter
RUN go install golang.org/x/tools/gopls@latest && \
    go install github.com/golangci/golangci-lint/cmd/golangci-lint@latest

# npm global tools: LSPs, tsgo, linters, formatters
RUN npm install -g \
      @typescript/native-preview \
      typescript \
      typescript-language-server \
      bash-language-server \
      yaml-language-server \
      vscode-langservers-extracted \
      prettier \
      markdownlint-cli \
      eslint

# Cargo tools
RUN cargo install cargo-nextest --locked

# Extra dev utilities
RUN apt-get update && \
    apt-get install -y \
      jq ripgrep fd-find \
      python3 python3-pip python3-venv \
      shellcheck sqlite3 tree bat \
      wget unzip patch procps && \
    rm -rf /var/lib/apt/lists/* && \
    ln -sf /usr/bin/fdfind /usr/local/bin/fd && \
    ln -sf /usr/bin/batcat /usr/local/bin/bat

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
