/// Board layer — the addresses that differ between aarch64 machines.
///
/// Decision 0003: QEMU `virt` and the Raspberry Pi 4 should differ only in
/// addresses and drivers. Everything board-specific lives here, selected at
/// compile time by exactly one cargo feature:
///
///   board-qemu-virt   (default)  QEMU `virt` machine, the development loop
///   board-rpi4                   Raspberry Pi 4 (BCM2711), the reference board
///
/// Build for the Pi with `--no-default-features --features board-rpi4`.

#[cfg(all(feature = "board-qemu-virt", feature = "board-rpi4"))]
compile_error!("select exactly one board: pass --no-default-features with board-rpi4");

#[cfg(not(any(feature = "board-qemu-virt", feature = "board-rpi4")))]
compile_error!("select a board feature: board-qemu-virt or board-rpi4");

#[cfg(feature = "board-qemu-virt")]
mod qemu_virt;
#[cfg(feature = "board-qemu-virt")]
pub use qemu_virt::*;

#[cfg(feature = "board-rpi4")]
mod rpi4;
#[cfg(feature = "board-rpi4")]
pub use rpi4::*;

/// A window of virtio-mmio transports: `slots` devices, `stride` bytes apart.
pub struct VirtioMmio {
    pub base: usize,
    pub stride: usize,
    pub slots: usize,
}
