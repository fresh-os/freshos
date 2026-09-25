/// Raspberry Pi 4 (BCM2711), booted by the `pftf/RPi4` UEFI firmware.
///
/// Addresses are for low-peripheral mode (the firmware default), from the
/// BCM2711 peripherals document. Not yet verified on hardware.
///
/// The kernel does not initialise the PL011: it relies on the firmware having
/// set it up at 115200 8n1 on GPIO 14/15, which needs Bluetooth moved off it
/// (`dtoverlay=disable-bt` in `config.txt`).
use super::{Gic, VirtioMmio};

pub const NAME: &str = "Raspberry Pi 4";

/// PL011 UART0 (bus address 0x7E20_1000).
pub const PL011_BASE: usize = 0xFE20_1000;

/// GIC-400 (GICv2) distributor and CPU interface.
pub const GIC: Gic = Gic::V2 {
    gicd: 0xFF84_1000,
    gicc: 0xFF84_2000,
};

/// No MCP UART yet. The Pi 4's extra PL011s (UART2-5) need GPIO pin muxing
/// and baud-rate setup first.
pub const MCP_UART_BASE: Option<usize> = None;

/// No virtio on real hardware: the compositor uses the UEFI framebuffer.
pub const VIRTIO_MMIO: Option<VirtioMmio> = None;
