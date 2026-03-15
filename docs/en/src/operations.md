# Operations

- Signal-based shutdown
  - Upon receiving SIGINT (Ctrl+C) or SIGTERM, workers wait for ongoing jobs to complete before shutting down
  - In all-in-one mode (`all-in-one` binary), the process is forcefully terminated after a 5-second timeout following signal reception
  - When running worker/grpc-front separately, workers wait indefinitely for job completion with no timeout
  - Command execution Runners (COMMAND/PYTHON_COMMAND) send SIGTERM to child processes, then forcefully terminate them with SIGKILL after a 5-second wait
- The `periodic_interval` (repeat interval in milliseconds) for periodic jobs must be greater than `JOB_QUEUE_FETCH_INTERVAL` (interval for periodic job fetch queries to RDB, default 1000ms)
  - For time-specified jobs, prefetching from RDB ensures execution at the specified time even if there is a gap between the fetch interval and execution time
- ID generation
  - IDs (e.g., job IDs) use Snowflake, with the machine ID derived from the lower 10 bits of the IPv4 address
  - Avoid using subnets with host parts exceeding 10 bits or instances with identical host parts across different subnets, as this may result in duplicate IDs
  - In IPv6 environments, the machine ID falls back to a random value
- When deploying jobworkerp-rs on Kubernetes and using the DOCKER runner, Docker Outside of Docker (DooD) or Docker in Docker (DinD) configuration is required for Pod-level Docker API access (unverified)
- If a panic occurs within a Runner Plugin, the entire Worker process will crash. It is recommended to operate Workers with fault-tolerant mechanisms such as supervisord or Kubernetes Deployments. See [Plugin Development](plugin-development.md) for details
