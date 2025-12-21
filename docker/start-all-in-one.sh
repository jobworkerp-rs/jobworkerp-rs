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

# Generate runtime config for admin-ui
/home/jobworkerp/env.sh

# Start nginx in background (runs as root)
nginx

# Start all-in-one as jobworkerp user (foreground)
gosu jobworkerp ./all-in-one &
ALL_IN_ONE_PID=$!

# Wait for all-in-one to exit
wait $ALL_IN_ONE_PID
FINAL_EXIT_CODE=$?

# Cleanup nginx when all-in-one exits
cleanup "$FINAL_EXIT_CODE"
