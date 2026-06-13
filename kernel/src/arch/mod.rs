/// Architecture abstraction layer.
///
/// Each target architecture provides the same public interface. Portable
/// kernel code (`ipc.rs`, `scheduler.rs`, `main.rs`, etc.) calls through
/// `arch::*` and never uses architecture-specific types directly.

#[cfg(target_arch = "x86_64")]
mod x86_64;
#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "aarch64")]
mod aarch64;
#[cfg(target_arch = "aarch64")]
pub use aarch64::*;
