# GitHub Actions optimized Dockerfile
# Requires pre-built binary at ./target/release/all-in-one
# For local development with full build, use `Dockerfile.full` instead

# Admin UI Build Stage
FROM node:20-slim AS ui-builder

WORKDIR /app

# Install pnpm
RUN npm install -g pnpm

# Copy admin-ui package files
COPY admin-ui/package.json admin-ui/pnpm-lock.yaml ./

# Install dependencies
RUN pnpm install --frozen-lockfile

# Copy admin-ui source code
COPY admin-ui/ .

# Build the application
RUN pnpm build

# Runtime Stage
FROM nvcr.io/nvidia/cuda:12.8.1-cudnn-runtime-ubuntu24.04

RUN apt-get update && apt-get -y dist-upgrade && apt-get install -y libssl3 libcurl4 libgomp1 docker.io docker-compose nginx gosu protobuf-compiler \
    && apt-get clean -y && rm -rf /var/lib/apt/lists/*

RUN adduser --system --group jobworkerp

RUN mkdir -p /home/jobworkerp && chown jobworkerp:jobworkerp /home/jobworkerp
RUN mkdir -p /home/jobworkerp/plugins && chown jobworkerp:jobworkerp /home/jobworkerp/plugins
ENV LD_LIBRARY_PATH=/home/jobworkerp/plugins:/home/jobworkerp/data/plugin/runner:/home/jobworkerp/data/plugin/cuda_runner:/usr/local/cuda/lib64:$LD_LIBRARY_PATH
RUN /sbin/ldconfig

WORKDIR /home/jobworkerp

# Copy pre-built all-in-one binary (built by GitHub Actions or locally)
COPY --chown=jobworkerp:jobworkerp ./target/release/all-in-one .

# Copy admin-ui built assets
COPY --from=ui-builder /app/dist /usr/share/nginx/html

# Copy nginx config
COPY docker/nginx.conf /etc/nginx/sites-available/default

# Copy env.sh for runtime config generation
COPY --chmod=755 admin-ui/docker/env.sh /home/jobworkerp/env.sh

# Copy startup script
COPY --chmod=755 docker/start-all-in-one.sh /home/jobworkerp/start.sh

# Default log level
ENV RUST_LOG=info,h2=warn,sqlx=warn
# Enable gRPC-Web support (required for admin-ui browser access)
ENV USE_GRPC_WEB=true
# Enable MCP Server and AG-UI Server by default
ENV MCP_ENABLED=true
ENV AG_UI_ENABLED=true
# Bind MCP and AG-UI to all interfaces (accessible both directly and via nginx proxy)
ENV MCP_ADDR=0.0.0.0:8000
ENV AG_UI_ADDR=0.0.0.0:8001
# Worker channel configuration for LLM jobs
ENV WORKER_DEFAULT_CONCURRENCY=4
ENV WORKER_CHANNELS=llm
ENV WORKER_CHANNEL_CONCURRENCIES=1
# Enable job status RDB indexing for job list queries
ENV JOB_STATUS_RDB_INDEXING=true

# Expose ports
# - 8080: nginx (Admin UI + gRPC-Web + MCP + AG-UI proxy)
# - 9000: gRPC (direct access)
# - 8000: MCP Server (direct access)
# - 8001: AG-UI Server (direct access)
EXPOSE 8080 9000 8000 8001

# Run as root initially to start nginx, then drop privileges for all-in-one
CMD ["/home/jobworkerp/start.sh"]
