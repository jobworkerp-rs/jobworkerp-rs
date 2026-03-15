#!/bin/bash

# Exit code to use when shutting down
FINAL_EXIT_CODE=0

# Cleanup function to stop nginx on exit
cleanup() {
    local exit_code=${1:-$FINAL_EXIT_CODE}
    echo "Stopping nginx..."
    nginx -s quit
    exit "$exit_code"
}

# Trap SIGTERM and SIGINT to cleanup with appropriate exit code
trap 'cleanup 143' SIGTERM  # 128 + 15 (SIGTERM)
trap 'cleanup 130' SIGINT   # 128 + 2 (SIGINT)

# Generate .env from Docker environment variables if not mounted
if [ ! -f /home/jobworkerp/.env ]; then
    env | grep -E '^(WORKER_|JOB_|STORAGE_|GRPC_|LOG_|RUST_LOG|MCP_|AG_UI_|USE_)' > /home/jobworkerp/.env
    chown jobworkerp:jobworkerp /home/jobworkerp/.env
fi

# Generate runtime config for admin-ui
/home/jobworkerp/env.sh

# Start nginx in background (runs as root)
nginx

# Start all-in-one as jobworkerp user (foreground)
# MCP_ENABLED and AG_UI_ENABLED are set via environment variables
gosu jobworkerp ./all-in-one &
ALL_IN_ONE_PID=$!

# Wait for all-in-one to exit
wait $ALL_IN_ONE_PID
FINAL_EXIT_CODE=$?

# Cleanup nginx when all-in-one exits
cleanup "$FINAL_EXIT_CODE"
