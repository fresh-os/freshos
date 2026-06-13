#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OVMF_CODE="/opt/homebrew/share/qemu/edk2-aarch64-code.fd"
OVMF_VARS_SRC="/opt/homebrew/share/qemu/edk2-arm-vars.fd"
OVMF_VARS="$SCRIPT_DIR/edk2-arm-vars.fd"

PROFILE="debug"
CARGO_FLAGS=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
    shift
fi

TARGET_DIR="$SCRIPT_DIR/target/aarch64-unknown-uefi/$PROFILE"
USER_TARGET_DIR="$SCRIPT_DIR/target/aarch64-unknown-none/$PROFILE"
ESP_DIR="$SCRIPT_DIR/esp-arm/EFI/BOOT"
INIT_DIR="$SCRIPT_DIR/esp-arm/EFI/FreshOS"

echo ":: Building FreshOS kernel for aarch64 ($PROFILE)..."
rustup run nightly cargo build --package freshos-kernel --target aarch64-unknown-uefi $CARGO_FLAGS
echo ":: Building FreshOS init for aarch64 ($PROFILE)..."
rustup run nightly cargo build --package freshos-init --target aarch64-unknown-none $CARGO_FLAGS
echo ":: Building FreshOS pong service for aarch64 ($PROFILE)..."
rustup run nightly cargo build --package freshos-pong --target aarch64-unknown-none $CARGO_FLAGS
echo ":: Building FreshOS pulse service for aarch64 ($PROFILE)..."
rustup run nightly cargo build --package freshos-pulse --target aarch64-unknown-none $CARGO_FLAGS
echo ":: Building FreshOS fault service for aarch64 ($PROFILE)..."
rustup run nightly cargo build --package freshos-fault --target aarch64-unknown-none $CARGO_FLAGS

echo ":: Preparing UEFI boot image..."
mkdir -p "$ESP_DIR"
mkdir -p "$INIT_DIR"
cp "$TARGET_DIR/freshos-kernel.efi" "$ESP_DIR/BOOTAA64.EFI"
cp "$USER_TARGET_DIR/freshos-init" "$INIT_DIR/INIT.ELF"
cp "$USER_TARGET_DIR/freshos-pong" "$INIT_DIR/PONG.ELF"
cp "$USER_TARGET_DIR/freshos-pulse" "$INIT_DIR/PULSE.ELF"
cp "$USER_TARGET_DIR/freshos-fault" "$INIT_DIR/FAULT.ELF"

# Create writable UEFI vars file if it doesn't exist
if [ ! -f "$OVMF_VARS" ]; then
    echo ":: Copying UEFI vars..."
    cp "$OVMF_VARS_SRC" "$OVMF_VARS"
fi

echo ":: Launching QEMU aarch64 (HVF primary demo path, serial on stdio)..."
exec qemu-system-aarch64 \
    -machine virt,accel=hvf,highmem=off \
    -cpu host \
    -m 512M \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF_CODE" \
    -drive if=pflash,format=raw,file="$OVMF_VARS" \
    -device ramfb \
    -device virtio-gpu-device \
    -global virtio-mmio.force-legacy=false \
    -display cocoa \
    -device qemu-xhci \
    -device usb-kbd \
    -serial mon:stdio \
    -drive format=raw,file=fat:rw:"$SCRIPT_DIR/esp-arm" \
    "$@"
