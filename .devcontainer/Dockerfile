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
    # JSON processing
    jq \
    unzip \
    && rm -rf /var/lib/apt/lists/*

# --------------------------------------------------------------------
# Chrome + chromedriver for headless WASM tests (make wasm-test)
#
# chromedriver is pinned to the installed Chrome's major version.  wasm-pack
# would otherwise download the newest driver on its own, which refuses to drive
# an older browser, and the bare driver binary also needs Chrome's shared libs
# (libnspr4 & friends) that only the browser package pulls in.
# --------------------------------------------------------------------
RUN curl -fsSL https://dl.google.com/linux/linux_signing_key.pub \
        | gpg --dearmor -o /usr/share/keyrings/google-chrome.gpg \
    && echo "deb [arch=$(dpkg --print-architecture) signed-by=/usr/share/keyrings/google-chrome.gpg] https://dl.google.com/linux/chrome/deb/ stable main" \
        > /etc/apt/sources.list.d/google-chrome.list \
    && apt-get update && apt-get install -y --no-install-recommends google-chrome-stable \
    && rm -rf /var/lib/apt/lists/* \
    && CHROME_MAJOR="$(google-chrome --version | sed -E 's/[^0-9]*([0-9]+).*/\1/')" \
    && DRIVER_URL="$(curl -fsSL https://googlechromelabs.github.io/chrome-for-testing/known-good-versions-with-downloads.json \
        | jq -r --arg m "${CHROME_MAJOR}." '[.versions[] | select(.version | startswith($m))] | last | .downloads.chromedriver[] | select(.platform == "linux64") | .url')" \
    && [ -n "$DRIVER_URL" ] && [ "$DRIVER_URL" != "null" ] \
    && curl -fsSL -o /tmp/chromedriver.zip "$DRIVER_URL" \
    && unzip -oq /tmp/chromedriver.zip -d /tmp \
    && install -m 0755 /tmp/chromedriver-linux64/chromedriver /usr/local/bin/chromedriver \
    && rm -rf /tmp/chromedriver.zip /tmp/chromedriver-linux64 \
    && chromedriver --version

# Points wasm-pack/wasm-bindgen-test at the pinned driver instead of letting them
# download the newest one into ~/.cache/.wasm-pack.
ENV CHROMEDRIVER=/usr/local/bin/chromedriver

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
#
# RUST_VERSION must match rust-toolchain.toml's channel. It is repeated here
# because this image builds from the .devcontainer/ context and so cannot read
# the repo root; `make check-toolchain-pin` fails when the two drift apart.
# --------------------------------------------------------------------
ARG RUST_VERSION=1.98.0
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain ${RUST_VERSION} --profile minimal \
      -c rustfmt -c clippy
ENV PATH="/home/$USERNAME/.cargo/bin:${PATH}"

# The first two mirror rust-toolchain.toml's `targets` so the image needs no
# download on first use; the thumbv6m/thumbv7m pair is a devcontainer-only
# convenience for MCU work outside what the workspace builds.
RUN rustup target add thumbv7em-none-eabihf \
    && rustup target add wasm32-unknown-unknown \
    && rustup target add thumbv6m-none-eabi \
    && rustup target add thumbv7m-none-eabi

# Install core AimDB development tools
RUN cargo install cargo-audit cargo-watch cargo-expand

# Install probe-rs with proper error handling
RUN cargo install probe-rs --features cli || \
    (echo "Warning: probe-rs installation failed. Install manually if needed for embedded debugging." && \
    echo "Run: cargo install probe-rs --features cli")

# --------------------------------------------------------------------
# Use Bash for subsequent RUN instructions (important for fnm)
# --------------------------------------------------------------------
SHELL ["/bin/bash", "-c"]

# --------------------------------------------------------------------
# Install fnm (Fast Node Manager)
# --------------------------------------------------------------------
RUN curl -fsSL https://fnm.vercel.app/install | bash -s -- --skip-shell \
    && export PATH="/home/$USERNAME/.local/share/fnm:$PATH" \
    && fnm install 20 \
    && fnm default 20

# Add fnm shell integration to bashrc
RUN echo '' >> /home/$USERNAME/.bashrc \
    && echo '# fnm' >> /home/$USERNAME/.bashrc \
    && echo 'FNM_PATH="/home/'"$USERNAME"'/.local/share/fnm"' >> /home/$USERNAME/.bashrc \
    && echo 'if [ -d "$FNM_PATH" ]; then' >> /home/$USERNAME/.bashrc \
    && echo '  export PATH="$FNM_PATH:$PATH"' >> /home/$USERNAME/.bashrc \
    && echo '  eval "$(fnm env --shell bash)"' >> /home/$USERNAME/.bashrc \
    && echo 'fi' >> /home/$USERNAME/.bashrc

ENV PATH="/home/$USERNAME/.local/share/fnm/aliases/default/bin:/home/$USERNAME/.local/share/fnm:${PATH}"

# --------------------------------------------------------------------
# Final working directory
# --------------------------------------------------------------------
WORKDIR /aimdb
