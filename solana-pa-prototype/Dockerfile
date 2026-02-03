# Solana Protocol Adapter Development Environment
# Includes: Solana CLI, Anchor, Rust with sbpf target, Node.js, CUDA toolkit (optional GPU)

FROM ubuntu:22.04

# Avoid interactive prompts
ENV DEBIAN_FRONTEND=noninteractive

# Install system dependencies
RUN apt-get update && apt-get install -y \
    curl \
    docker.io \
    git \
    build-essential \
    pkg-config \
    libssl-dev \
    libudev-dev \
    llvm \
    clang \
    cmake \
    ca-certificates \
    jq \
    gnupg \
    && rm -rf /var/lib/apt/lists/*

# Install CUDA toolkit for GPU-accelerated proving (used when docker-compose.gpu.yml is active)
RUN curl -fsSL https://developer.download.nvidia.com/compute/cuda/repos/ubuntu2204/x86_64/cuda-keyring_1.1-1_all.deb -o /tmp/cuda-keyring.deb \
    && dpkg -i /tmp/cuda-keyring.deb \
    && rm /tmp/cuda-keyring.deb \
    && apt-get update \
    && apt-get install -y cuda-toolkit-12-4 \
    && rm -rf /var/lib/apt/lists/*

ENV PATH="/usr/local/cuda/bin:${PATH}"
ENV LD_LIBRARY_PATH="/usr/local/cuda/lib64:${LD_LIBRARY_PATH}"

# Install Node.js 20.x
RUN curl -fsSL https://deb.nodesource.com/setup_20.x | bash - \
    && apt-get install -y nodejs \
    && npm install -g yarn

# Create non-root user with UID matching host user for volume permissions
ARG UID=1001
ARG GID=1001
ARG DOCKER_GID=30001
RUN groupadd -g ${GID} developer \
    && useradd -m -s /bin/bash -u ${UID} -g ${GID} developer \
    && (getent group ${DOCKER_GID} || groupadd -g ${DOCKER_GID} dockersock) \
    && usermod -aG dockersock developer
USER developer
WORKDIR /home/developer

# Install Rust via rustup (with proxy shims)
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
ENV PATH="/home/developer/.cargo/bin:${PATH}"

# Install nightly toolchain (needed for sbpf target features)
RUN rustup install nightly \
    && rustup component add rustfmt clippy

# Install RISC Zero toolchain (needed by risc0-build / embed_methods for zkVM guests)
RUN cargo install rzup --locked \
    && rzup install rust \
    && rzup install cargo-risczero

# Install Solana CLI (v2.3.9 to match risc0-solana requirements)
RUN sh -c "$(curl -sSfL https://release.anza.xyz/v2.3.9/install)"
ENV PATH="/home/developer/.local/share/solana/install/active_release/bin:${PATH}"

# Install Anchor CLI 0.31.1 (matches risc0-solana requirements)
RUN cargo install --git https://github.com/coral-xyz/anchor --tag v0.31.1 anchor-cli --locked

# Pre-create paths that are later backed by named volumes so empty volumes
# get initialized with the correct ownership (Docker copies existing dir
# contents/metadata into a fresh volume on first mount).
RUN mkdir -p /home/developer/.config/solana \
    && mkdir -p /home/developer/validator-ledger \
    && mkdir -p /home/developer/.cache/solana

# Copy entrypoint script (ensures keypair exists on container start)
COPY --chown=developer:developer scripts/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Set working directory for the project
WORKDIR /workspace

# Entrypoint handles runtime initialization (keypair generation, etc.)
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["/bin/bash"]
