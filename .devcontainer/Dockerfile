FROM ubuntu:22.04

ARG USERNAME=vscode
ARG USER_UID=1000
ARG USER_GID=1000

ENV DEBIAN_FRONTEND=noninteractive

# --------------------------------------------------------------------
# System packages as root + AimDB protocol dependencies
# --------------------------------------------------------------------
RUN apt-get update && apt-get install -y \
    gh \
    bash-completion \
    build-essential \
    curl \
    git \
    gnupg2 \
    libssl-dev \
    pkg-config \
    ca-certificates \
    sudo \
    openssl \
    libudev-dev llvm libclang-dev \
    protobuf-compiler libssl-dev \
    cmake ninja-build gcc-arm-none-eabi gdb-multiarch \
    # AimDB connector dependencies
    mosquitto-clients \
    # Performance profiling tools
    valgrind \
    perf-tools-unstable \
    # Additional dependencies for probe-rs
    libusb-1.0-0-dev \
    libudev-dev \
    && rm -rf /var/lib/apt/lists/*

# --------------------------------------------------------------------
# Create non-root user (with sudo) as root
# --------------------------------------------------------------------
RUN groupadd --gid $USER_GID $USERNAME \
    && useradd --uid $USER_UID --gid $USER_GID -m $USERNAME \
    && echo "$USERNAME ALL=(ALL) NOPASSWD:ALL" >> /etc/sudoers

# --------------------------------------------------------------------
# Switch default shell to bash (requires root)
# --------------------------------------------------------------------
RUN rm /bin/sh && ln -s /bin/bash /bin/sh

# --------------------------------------------------------------------
# Switch to non-root user + set home working dir
# --------------------------------------------------------------------
USER $USERNAME
WORKDIR /home/$USERNAME

# --------------------------------------------------------------------
# Install Rust + AimDB-specific targets and tools
# --------------------------------------------------------------------
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/home/$USERNAME/.cargo/bin:${PATH}"

# Add embedded targets for AimDB MCU support
RUN rustup target add thumbv7em-none-eabihf \
    && rustup target add thumbv6m-none-eabi \
    && rustup target add thumbv7m-none-eabi

# Install core AimDB development tools
RUN cargo install cargo-audit cargo-watch cargo-expand

# Install probe-rs with proper error handling
RUN cargo install probe-rs --features cli || \
    (echo "Warning: probe-rs installation failed. Install manually if needed for embedded debugging." && \
    echo "Run: cargo install probe-rs --features cli")

# --------------------------------------------------------------------
# Use Bash for subsequent RUN instructions (important for nvm)
# --------------------------------------------------------------------
SHELL ["/bin/bash", "-c"]

# --------------------------------------------------------------------
# Final working directory
# --------------------------------------------------------------------
WORKDIR /aimdb
