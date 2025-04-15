# 基础镜像
FROM debian:bookworm-slim as builder

# 安装 Rust 工具链和构建依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    build-essential \
    cmake \
    perl \
    pkg-config \
    libclang-dev \
    libssl-dev \
    musl-tools \
    && rm -rf /var/lib/apt/lists/*

# 安装 Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

# 设置工作目录
WORKDIR /app

# 复制 Cargo 配置文件
COPY Cargo.toml Cargo.lock* ./

# 复制整个源代码目录
COPY src ./src

# 构建应用程序
RUN cargo build --release

# 最终镜像
FROM debian:bookworm-slim

# 安装运行时依赖
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    libssl1.1 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# 从构建器阶段复制二进制文件
COPY --from=builder /app/target/release/clewdr /usr/local/bin/

# 设置容器启动命令
CMD ["clewdr"]
