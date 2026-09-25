/// Architecture abstraction layer.
///
/// Portable kernel code (`ipc.rs`, `main.rs`, etc.) calls through `arch::*`
/// and never uses architecture-specific types directly. aarch64 is the only
/// architecture (decision 0003).
#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
