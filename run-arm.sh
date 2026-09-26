#!/usr/bin/env bash
set -euo pipefail

# Build FreshOS for aarch64, stage an ESP, and boot it in QEMU.
#
# Optional environment overrides (tests/harness.py uses these):
#   ESP_DIR          where to stage the ESP                    (default: ./esp-arm)
#   MCP_SOCK         the MCP bridge's Unix socket              (default: ./mcp.sock)
#   EXTRA_ELFS       extra userbin packages, without "freshos-", to build and stage
#   OMIT_ELFS        userbin packages to leave off the ESP, e.g. "init"
#   EXTRA_FILES_DIR  files copied as-is into \EFI\FreshOS\ (e.g. malformed ELFs)
#   OVMF_VARS        writable UEFI variable store              (default: ./edk2-arm-vars.fd)
#   SKIP_BUILD=1     stage what is already built
#   BUILD_ONLY=1     build, then exit without staging or booting
#   FRESHOS_ACCEL    hvf (default) or tcg

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OVMF_CODE="/opt/homebrew/share/qemu/edk2-aarch64-code.fd"
OVMF_VARS_SRC="/opt/homebrew/share/qemu/edk2-arm-vars.fd"
OVMF_VARS="${OVMF_VARS:-$SCRIPT_DIR/edk2-arm-vars.fd}"
ESP_ROOT="${ESP_DIR:-$SCRIPT_DIR/esp-arm}"
MCP_SOCK="${MCP_SOCK:-$SCRIPT_DIR/mcp.sock}"
ACCEL="${FRESHOS_ACCEL:-hvf}"

PROFILE="debug"
CARGO_FLAGS=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
    shift
fi

# Every userbin, by package name without the "freshos-" prefix.
USERBINS="init pong pulse fault ${EXTRA_ELFS:-}"

TARGET_DIR="$SCRIPT_DIR/target/aarch64-unknown-uefi/$PROFILE"
USER_TARGET_DIR="$SCRIPT_DIR/target/aarch64-unknown-none/$PROFILE"
BOOT_DIR="$ESP_ROOT/EFI/BOOT"
FRESHOS_DIR="$ESP_ROOT/EFI/FreshOS"

# ESP file name for a userbin: upper case, no hyphens, at most 8 characters (FAT 8.3).
elf_name() {
    local n="${1//-/}"
    n="$(printf '%s' "$n" | tr '[:lower:]' '[:upper:]')"
    printf '%s.ELF' "${n:0:8}"
}

if [[ -z "${SKIP_BUILD:-}" ]]; then
    echo ":: Building FreshOS kernel for aarch64 ($PROFILE)..."
    rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi $CARGO_FLAGS
    for bin in $USERBINS; do
        echo ":: Building $bin for aarch64 ($PROFILE)..."
        rustup run nightly cargo build --package "freshos-$bin" --target aarch64-unknown-none $CARGO_FLAGS
    done
fi
if [[ -n "${BUILD_ONLY:-}" ]]; then
    exit 0
fi

echo ":: Preparing UEFI boot image in $ESP_ROOT..."
rm -rf "$FRESHOS_DIR"
mkdir -p "$BOOT_DIR" "$FRESHOS_DIR"
cp "$TARGET_DIR/freshos-kernel.efi" "$BOOT_DIR/BOOTAA64.EFI"
for bin in $USERBINS; do
    if [[ " ${OMIT_ELFS:-} " == *" $bin "* ]]; then
        echo ":: Omitting $bin"
        continue
    fi
    cp "$USER_TARGET_DIR/freshos-$bin" "$FRESHOS_DIR/$(elf_name "$bin")"
done
if [[ -n "${EXTRA_FILES_DIR:-}" ]] && compgen -G "$EXTRA_FILES_DIR/*" > /dev/null; then
    cp "$EXTRA_FILES_DIR"/* "$FRESHOS_DIR/"
fi

if [ ! -f "$OVMF_VARS" ]; then
    echo ":: Copying UEFI vars..."
    cp "$OVMF_VARS_SRC" "$OVMF_VARS"
fi

CPU="host"
if [[ "$ACCEL" == "tcg" ]]; then
    CPU="max"
fi

echo ":: Launching QEMU aarch64 ($ACCEL, serial on stdio, MCP on $MCP_SOCK)..."
exec qemu-system-aarch64 \
    -machine virt,accel="$ACCEL",highmem=off,gic-version=3 \
    -cpu "$CPU" \
    -m 512M \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$OVMF_VARS" \
    -device ramfb \
    -global virtio-mmio.force-legacy=false \
    -display cocoa \
    -device qemu-xhci \
    -device usb-kbd \
    -serial mon:stdio \
    -serial unix:"$MCP_SOCK",server=on,wait=off \
    -drive format=raw,file=fat:rw:"$ESP_ROOT" \
    "$@"
