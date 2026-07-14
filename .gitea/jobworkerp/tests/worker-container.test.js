const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');

const repoRoot = path.resolve(__dirname, '../../..');

function readRepoFile(relativePath) {
  return fs.readFileSync(path.join(repoRoot, relativePath), 'utf8');
}

test('GPU worker image builds the worker binary in an Ubuntu 24.04 builder stage', () => {
  const dockerfile = readRepoFile('worker-main/Dockerfile');

  assert.match(
    dockerfile,
    /FROM nvcr\.io\/nvidia\/cuda:13\.1\.2-cudnn-devel-ubuntu24\.04 AS builder/,
  );
  assert.match(dockerfile, /ARG CARGO_FEATURES=mysql/);
  assert.match(
    dockerfile,
    /cargo build --release --locked --features "\$CARGO_FEATURES" --bin worker/,
  );
  assert.match(
    dockerfile,
    /COPY --from=builder --chown=jobworkerp:daemon \/build\/target\/release\/worker \./,
  );
  assert.doesNotMatch(dockerfile, /COPY --chown=jobworkerp:daemon \.\/target\/release\/worker \./);
});

test('worker runtime image includes Git for command-runner repository operations', () => {
  const dockerfile = readRepoFile('worker-main/Dockerfile');
  const runtimeStage = dockerfile.split('FROM nvcr.io/nvidia/cuda:13.1.2-cudnn-runtime-ubuntu24.04')[1] ?? '';

  assert.match(runtimeStage, /apt-get install -y --no-install-recommends[\s\S]*?\n\s*git\s*\\/);
});

test('Docker build context includes only the host binaries used by runtime images', () => {
  const dockerignore = readRepoFile('.dockerignore');

  assert.match(dockerignore, /^\*$/m);
  assert.match(dockerignore, /^!worker-main\/\*\*$/m);
  assert.match(dockerignore, /^!modules\/\*\*$/m);
  assert.match(dockerignore, /^!target\/$/m);
  assert.match(dockerignore, /^!target\/release\/$/m);
  assert.match(dockerignore, /^!target\/release\/grpc-front$/m);
  assert.match(dockerignore, /^!target\/release\/mcp-http$/m);
  assert.doesNotMatch(dockerignore, /^!target\/\*\*$/m);
});
