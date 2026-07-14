# Build the worker in the same Ubuntu release as the runtime image so host
# toolchains cannot leak into a deployable image.
FROM nvcr.io/nvidia/cuda:13.1.2-cudnn-devel-ubuntu24.04 AS builder

ARG CARGO_FEATURES=mysql

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        curl \
        mold \
        pkg-config \
        protobuf-compiler \
    && apt-get clean \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable

WORKDIR /build
COPY . .

RUN cargo build --release --locked --features "$CARGO_FEATURES" --bin worker

FROM nvcr.io/nvidia/cuda:13.1.2-cudnn-runtime-ubuntu24.04

RUN apt-get update && apt-get -y dist-upgrade && apt-get install -y --no-install-recommends \
        ca-certificates \
        docker.io \
        docker-compose \
        git \
        libcurl4 \
        libgomp1 \
        protobuf-compiler \
    && apt-get clean -y \
    && rm -rf /var/lib/apt/lists/*

RUN adduser --system jobworkerp

RUN mkdir -p /home/jobworkerp && chown jobworkerp:daemon /home/jobworkerp
RUN mkdir -p /home/jobworkerp/plugins && chown jobworkerp:daemon /home/jobworkerp/plugins
ENV LD_LIBRARY_PATH=/home/jobworkerp/plugins:/home/jobworkerp/data/plugin/runner:/home/jobworkerp/data/plugin/cuda_runner:/usr/local/cuda/lib64:$LD_LIBRARY_PATH
RUN /sbin/ldconfig

WORKDIR /home/jobworkerp
COPY --from=builder --chown=jobworkerp:daemon /build/target/release/worker .

USER jobworkerp

CMD ["./worker"]
