#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
HOST_OS="$(uname -s)"
HOST_ARCH="$(uname -m)"

if [[ "$HOST_OS" == "Darwin" && ( "$HOST_ARCH" == "arm64" || "$HOST_ARCH" == "aarch64" ) ]]; then
    echo ":: FreshOS primary demo path: aarch64 + HVF (Apple Silicon)"
    exec "$SCRIPT_DIR/run-arm.sh" "$@"
fi

echo ":: FreshOS demo fallback: x86_64 + UEFI"
exec "$SCRIPT_DIR/run.sh" "$@"
