#!/usr/bin/env bash
# Build FreshOS and run the automated QEMU tests in tests/.
# Extra arguments go to unittest, e.g. ./test.sh -k latency
set -euo pipefail
cd "$(dirname "$0")"
exec python3 -m unittest discover -s tests -t tests "$@"
