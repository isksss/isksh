FROM debian:13-slim

ARG MISE_VERSION=2026.7.12

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
       bash build-essential ca-certificates curl dash file git jq \
       mingw-w64 musl-tools binutils llvm xz-utils \
    && rm -rf /var/lib/apt/lists/*

ENV MISE_DATA_DIR=/mise
ENV MISE_CONFIG_DIR=/mise
ENV MISE_CACHE_DIR=/mise/cache
ENV MISE_INSTALL_PATH=/usr/local/bin/mise
ENV PATH=/mise/shims:/root/.cargo/bin:${PATH}

RUN curl -fsSL https://mise.run | MISE_VERSION=${MISE_VERSION} sh

WORKDIR /workspace
COPY mise.toml /workspace/mise.toml
RUN mise trust /workspace/mise.toml && mise install --yes

CMD ["bash"]

