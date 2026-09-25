/// QEMU `virt` machine (as run by `run-arm.sh`, with HVF).
use super::VirtioMmio;

pub const NAME: &str = "QEMU virt";

/// PL011 UART.
pub const PL011_BASE: usize = 0x0900_0000;

/// GICv2 distributor and CPU interface. HVF gives GICv2, not v3.
pub const GICD_BASE: usize = 0x0800_0000;
pub const GICC_BASE: usize = 0x0801_0000;

/// virtio-mmio transports, probed for the virtio-GPU.
pub const VIRTIO_MMIO: Option<VirtioMmio> = Some(VirtioMmio {
    base: 0x0a00_0000,
    stride: 0x200,
    slots: 32,
});
