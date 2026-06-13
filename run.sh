#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
OVMF="/opt/homebrew/share/qemu/edk2-x86_64-code.fd"
DISK="$SCRIPT_DIR/disk.img"

PROFILE="debug"
CARGO_FLAGS=""
if [[ "${1:-}" == "--release" ]]; then
    PROFILE="release"
    CARGO_FLAGS="--release"
    shift
fi

TARGET_DIR="$SCRIPT_DIR/target/x86_64-unknown-uefi/$PROFILE"
ESP_DIR="$SCRIPT_DIR/esp/EFI/BOOT"

echo ":: Building FreshOS kernel ($PROFILE)..."
RUSTUP_TOOLCHAIN=nightly cargo build --package freshos-kernel --target x86_64-unknown-uefi $CARGO_FLAGS

echo ":: Preparing UEFI boot image..."
mkdir -p "$ESP_DIR"
cp "$TARGET_DIR/freshos-kernel.efi" "$ESP_DIR/BOOTX64.EFI"

# Create a 1 MiB disk image if it doesn't exist (for the ATA PIO driver)
if [ ! -f "$DISK" ]; then
    echo ":: Creating disk image..."
    dd if=/dev/zero of="$DISK" bs=1M count=1 2>/dev/null
fi

echo ":: Launching QEMU (VNC on :5900)..."
exec qemu-system-x86_64 \
    -machine q35 \
    -drive if=pflash,format=raw,readonly=on,file="$OVMF" \
    -drive format=raw,file=fat:rw:"$SCRIPT_DIR/esp",if=none,id=esp \
    -device virtio-blk-pci,drive=esp,bootindex=0 \
    -drive file="$DISK",format=raw,if=none,id=disk0 \
    -device ide-hd,drive=disk0 \
    -m 256M \
    -device virtio-gpu-pci \
    -display vnc=localhost:0 \
    -serial stdio \
    "$@"
