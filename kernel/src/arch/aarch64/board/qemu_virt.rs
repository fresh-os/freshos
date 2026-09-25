/// QEMU `virt` machine (as run by `run-arm.sh`, with HVF).
use super::{Gic, VirtioMmio};

pub const NAME: &str = "QEMU virt";

/// PL011 UART.
pub const PL011_BASE: usize = 0x0900_0000;

/// GICv3. QEMU 11 refuses GICv2 under HVF ("HVF does not support GICv2
/// emulation"); older QEMU gave GICv2 by default. The redistributor for
/// CPU 0 is the first in the 0x080A_0000 region.
pub const GIC: Gic = Gic::V3 {
    gicd: 0x0800_0000,
    gicr: 0x080A_0000,
};

/// virtio-mmio transports, probed for the virtio-GPU.
pub const VIRTIO_MMIO: Option<VirtioMmio> = Some(VirtioMmio {
    base: 0x0a00_0000,
    stride: 0x200,
    slots: 32,
});
