# Adds the microsandbox runtime (msb + libkrunfw) to a published jobworkerp
# image for the SANDBOX runner (usage: docker-compose-sandbox.yml).
#
# Published images do not ship the runtime: libkrunfw bundles a GPL-2.0-only
# Linux kernel and LGPL-2.1-only code, so redistributing its binary requires
# shipping its source. Build this image locally and push it only to a private
# registry.
#
# The pins follow the microsandbox crate in Cargo.lock, since the SDK and the
# runtime are released together; worker-main/tests/docker_microsandbox_runtime.rs
# fails when they drift.
ARG BASE_IMAGE=ghcr.io/jobworkerp-rs/jobworkerp:latest
ARG MICROSANDBOX_VERSION=0.7.6
ARG LIBKRUNFW_VERSION=5.6.1

FROM ghcr.io/superradcompany/microsandbox:${MICROSANDBOX_VERSION} AS microsandbox

FROM ${BASE_IMAGE}
ARG LIBKRUNFW_VERSION
COPY --from=microsandbox /usr/local/bin/msb /usr/local/bin/msb
COPY --from=microsandbox /usr/local/lib/libkrunfw.so.${LIBKRUNFW_VERSION} /usr/local/lib/libkrunfw.so.${LIBKRUNFW_VERSION}
ENV MSB_PATH=/usr/local/bin/msb \
    MSB_LIBKRUNFW_PATH=/usr/local/lib/libkrunfw.so.${LIBKRUNFW_VERSION}
