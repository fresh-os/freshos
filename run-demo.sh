#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"

if [[ "$HOST_OS" == "Darwin" && ( "$HOST_ARCH" == "arm64" || "$HOST_ARCH" == "aarch64" ) ]]; then
    exec "$SCRIPT_DIR/run-arm.sh" "$@"
fi

echo "FreshOS runs on aarch64 only (decision 0003); run-arm.sh needs an Apple Silicon Mac for HVF." >&2
exit 1
