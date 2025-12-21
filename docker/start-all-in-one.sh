#!/bin/bash

# Cleanup function to stop nginx on exit
cleanup() {
    echo "Stopping nginx..."
    nginx -s quit
    exit 0
}

# Trap SIGTERM and SIGINT to cleanup
trap cleanup SIGTERM SIGINT

# Generate runtime config for admin-ui
/home/jobworkerp/env.sh

# Start nginx in background (runs as root)
nginx

# Start all-in-one as jobworkerp user (foreground)
# Use exec to replace shell process, but we need to keep trap working
gosu jobworkerp ./all-in-one &
ALL_IN_ONE_PID=$!

# Wait for all-in-one to exit
wait $ALL_IN_ONE_PID
EXIT_CODE=$?

# Cleanup nginx when all-in-one exits
cleanup

exit $EXIT_CODE
