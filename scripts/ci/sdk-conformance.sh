#!/usr/bin/env bash
# Mirrors the "SDK source conformance" job in .github/workflows/ci-rust.yml.
# Requires protoc, a JDK, cargo, and initialized submodules.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
exec "$repo_root/scripts/verify-sdk-source.sh"
