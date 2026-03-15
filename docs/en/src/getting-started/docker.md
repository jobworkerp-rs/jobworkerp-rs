# Docker

## Launch Example Using Docker Image

```shell
# Launch with docker-compose
$ docker-compose up

# Launch with scalable configuration (for production)
$ docker-compose -f docker-compose-scalable.yml up --scale jobworkerp-worker=3
```

## Building Docker Image Locally

There are two Dockerfiles available for the all-in-one image:

| Dockerfile | Purpose | Description |
|------------|---------|-------------|
| `Dockerfile` | CI / Demo distribution | Requires a pre-built `./target/release/all-in-one` binary. For publishing images via GitHub Actions or environments with pre-built binaries. |
| `Dockerfile.full` | Local development / testing | Full multi-stage build. Builds both Rust binary and admin-ui inside Docker. No pre-built binary required. |

```shell
# Option 1: Using pre-built binary (Dockerfile)
# First build the Rust binary locally
$ cargo build --release
# Then build the Docker image
$ docker build -t jobworkerp-all-in-one .

# Option 2: Full build inside Docker (Dockerfile.full)
# No pre-built binary required - everything is built inside Docker
$ docker build -f Dockerfile.full -t jobworkerp-all-in-one .

# Run the container
# - Port 8080: Admin UI (nginx + MCP/AG-UI reverse proxy)
# - Port 9000: gRPC / gRPC-Web
# - Port 8000: MCP Server (direct access)
# - Port 8001: AG-UI Server (direct access)
# - docker.sock: Mount host Docker daemon (for DOCKER runner)
$ docker run -p 8080:8080 -p 9000:9000 -p 8000:8000 -p 8001:8001 \
  -v /var/run/docker.sock:/var/run/docker.sock \
  jobworkerp-all-in-one
```

## Accessing After Launch

Once the container is running, open [http://localhost:8080](http://localhost:8080) in your browser to access the Admin UI. The Admin UI provides a GUI for creating and managing Workers, submitting and monitoring jobs, and other operations.

### Port Configuration

| Port | Service | Access URL |
|------|---------|------------|
| 8080 | Admin UI (nginx) | `http://localhost:8080` |
| 9000 | gRPC / gRPC-Web | `localhost:9000` |
| 8000 | MCP Server | `http://localhost:8000/mcp` |
| 8001 | AG-UI Server | `http://localhost:8001/ag-ui/run` |

Proxy access via nginx is also available: `http://localhost:8080/mcp`, `http://localhost:8080/ag-ui/`

> **Note**: `Dockerfile.full` takes longer to build but is convenient for local development and testing as it doesn't require a local Rust toolchain.
