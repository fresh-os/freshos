---
title: "feat: Port FreshOS to aarch64 with architecture abstraction layer"
type: feat
date: 2026-04-02
deepened: 2026-04-02
---

# Port FreshOS to aarch64 with Architecture Abstraction

## Enhancement Summary

**Deepened on:** 2026-04-02
**Research agents used:** architecture-strategist, performance-oracle, code-simplicity-reviewer, best-practices-researcher, framework-docs-researcher

### Key Improvements from Research
1. **Merged to 3 phases** (from 5) — serial boot and timer are not useful checkpoints on their own
2. **Trimmed portable interface** — dropped x86-only concepts, allowed targeted cfg in portable code
3. **Critical QEMU discovery** — virtio-gpu-pci required for framebuffer after ExitBootServices on aarch64 virt; ramfb is the simpler alternative
4. **Performance-optimised context switch** — lightweight SVC path (14 regs) vs heavyweight IRQ path (34 regs)
5. **ASID-based TLB management** — avoids expensive broadcast invalidation on context switch
6. **Concrete QEMU command line** with HVF, including `highmem=off` requirement

### Critical Discoveries
- **GOP framebuffer caveat**: On QEMU virt with virtio-gpu-pci, the GOP framebuffer uses `PixelBltOnly` (no linear framebuffer after ExitBootServices). Must use `-device ramfb` for a simple linear framebuffer, or write a virtio-gpu driver. **ramfb** is the pragmatic choice for initial bring-up.
- **Memory barriers everywhere**: ARM's weak memory model requires `isb` after every system register write, `dsb ish` + `isb` after every page table modification, `dsb sy` after GIC configuration. Missing any of these causes silent corruption.
- **HVF constraint**: `highmem=off` required on Apple Silicon. Guest runs at EL1 only (no EL2 access).

---

## Overview

Split the kernel into portable and architecture-specific layers, then implement the aarch64 backend targeting QEMU's `virt` machine with HVF hardware virtualisation on Apple Silicon. This removes the QEMU TCG performance bottleneck (10-50x slowdown) and enables native-speed development.

## Motivation

The compositor takes 30-60 seconds per frame under QEMU TCG on Apple Silicon. QEMU HVF for aarch64 runs at near-native speed. This unblocks all visual polish, interactive testing, and demo preparation. The Pi 4/5 become future bare-metal targets.

---

## Technical Approach

### Architecture Module Structure

Use **cfg-switched module re-exports** (not traits). This is the proven pattern in real Rust kernels (Hermit-OS, rpi-OS tutorials). Traits add indirection the compiler can't always devirtualise in `no_std`, and buy nothing when only one arch is compiled at a time.

```
kernel/src/
  arch/
    mod.rs                  // cfg-switched re-exports
    x86_64/
      mod.rs
      gdt.rs, idt.rs, pic.rs, timer.rs, serial.rs
      syscall.rs, context.rs, paging.rs, speaker.rs
    aarch64/
      mod.rs
      exceptions.rs         // exception vector table + VBAR_EL1
      gic.rs                // GICv3 (GICD at 0x0800_0000, GICR at 0x080A_0000)
      timer.rs              // ARM generic timer (CNTP_*, CNTPCT_EL0)
      syscall.rs            // SVC handler via ESR_EL1
      context.rs            // save/restore + TTBR0 switch
      uart.rs               // PL011 at 0x0900_0000
      paging.rs             // ARM page tables, TTBR0/TTBR1, TCR_EL1
  // Portable (unchanged location):
  ipc.rs, frame_alloc.rs, heap.rs, framebuffer.rs
  font.rs, font_aa.rs, scripting.rs, main.rs
```

**Keyboard, mouse, and speaker stay where they are** behind `#[cfg(target_arch = "x86_64")]` — they're x86-only features, not architecture abstractions. Move them into `arch/x86_64/` only when aarch64 has equivalent input/audio paths.

### Portable Interface

The `arch` module exposes ~12 functions that both architectures implement with meaningful code:

```rust
// arch/mod.rs
pub fn init_early();              // GDT+IDT / exception vectors+VBAR
pub fn init_interrupts();         // PIC remap / GIC distributor+redistributor+ICC
pub fn init_timer(hz: u32);       // PIT channel 0 / generic timer CNTP_TVAL
pub fn init_serial();             // COM1 8250 / PL011 UART
pub fn time_ns() -> u64;          // rdtsc+calibration / CNTPCT_EL0+CNTFRQ
pub fn interrupt_disable();       // cli / DAIFSet #0x2
pub fn interrupt_enable();        // sti / DAIFClr #0x2
pub fn halt();                    // hlt / wfi
pub fn create_kernel_pages() -> u64;     // PML4+CR3 / L0+TTBR1
pub fn create_user_pages(...) -> u64;    // PML4 with USER / L0 for TTBR0
pub fn setup_task_frame(...);     // fake iretq frame / fake eret frame
pub fn switch_address_space(pml4: u64);  // mov cr3 / msr TTBR0_EL1 + tlbi

pub macro serial_println;         // per-arch serial output
```

**Dropped from the original plan** (per simplicity review):
- `halt_no_irq()` — inline with cfg at the one call site (panic handler)
- `init_syscalls()` — fold into `init_early()` on x86 (no-op on ARM, handled by exception vectors)
- `KERNEL_CODE_SEL` / `USER_CODE_SEL` — x86-only, stay in `arch/x86_64/`
- Strict "zero cfg in portable code" rule — allow targeted cfg where it's clearer

### aarch64 Design Decisions

**Exception vector table (2048 bytes, 16 entries of 128 bytes):**
```
Entries that matter for the kernel:
  current_el_spx_irq    → timer/device IRQs while in EL1
  lower_el_a64_sync     → SVC syscalls + page faults from EL0
  lower_el_a64_irq      → timer preempting user tasks
```
Defined as a `global_asm!(include_str!("exception.s"))` following the rpi-OS tutorials pattern.

**Syscall ABI (AAPCS64):**
- `x8` = syscall number, `x0`-`x5` = arguments, `x0` = return value
- SVC #0 triggers synchronous exception to EL1
- No shadow space (unlike x86 MS ABI) — simpler assembly

**Two-speed context switch** (per performance review):
- **SVC path (lightweight)**: save x0-x8, x30, SP_EL0, ELR_EL1, SPSR_EL1 — 14 registers. The caller already preserved x19-x30 per AAPCS64.
- **IRQ path (heavyweight)**: save all x0-x30 + SP_EL0 + ELR_EL1 + SPSR_EL1 — 34 registers. Required because the timer can preempt at any instruction.

**ASID-based TLB management** (per performance review):
- Assign each task an 8-bit ASID (up to 256 tasks without recycling)
- Write `(ASID << 48) | page_table_phys` to TTBR0_EL1
- Use `tlbi aside1, xN` (ASID-specific invalidation) instead of `tlbi vmalle1is` (broadcast all)
- Saves ~100 cycles per context switch on real hardware

**Page tables — split address space:**
- `TTBR0_EL1` = user space (swapped per task, with ASID)
- `TTBR1_EL1` = kernel space (fixed, same for all tasks)
- 4KB granule, 4-level translation, 48-bit VA
- TCR_EL1: T0SZ=16, T1SZ=16, inner-shareable, write-back cacheable

**Memory barrier checklist:**
- After `msr VBAR_EL1` → `isb`
- After `msr SCTLR_EL1` (MMU enable) → `isb`
- After `msr TCR_EL1` / `msr TTBR0_EL1` / `msr TTBR1_EL1` → `dsb ish` + `isb`
- After page table entry writes → `dsb ish` + `tlbi` + `dsb ish` + `isb`
- After GIC register writes (GICD, GICR) → `dsb sy`
- After `msr ICC_*` (GIC CPU interface) → `isb`
- After any system register write that affects execution → `isb`

**System register access: hand-rolled** (10-20 lines of inline asm, no crate dependency).

### QEMU Setup (Critical Details)

**The working command line:**
```bash
qemu-system-aarch64 \
    -machine virt,accel=hvf,highmem=off \
    -cpu host \
    -m 512M \
    -drive if=pflash,format=raw,readonly=on,file=/opt/homebrew/share/qemu/edk2-aarch64-code.fd \
    -drive if=pflash,format=raw,file=edk2-arm-vars.fd \
    -device ramfb \
    -display cocoa \
    -device qemu-xhci \
    -device usb-kbd \
    -serial mon:stdio \
    -drive format=raw,file=fat:rw:esp-arm
```

**Key constraints:**
- `highmem=off` — **required** for HVF on Apple Silicon
- `-device ramfb` — provides a simple linear framebuffer accessible after ExitBootServices. The virtio-gpu-pci device requires a driver; ramfb does not (UEFI firmware exposes it via GOP with a real framebuffer base address)
- `-device qemu-xhci` + `-device usb-kbd` — USB keyboard for input (no PS/2 on virt)
- UEFI vars file must be a writable copy: `cp /opt/homebrew/share/qemu/edk2-arm-vars.fd .`
- EFI binary goes at `esp-arm/EFI/BOOT/BOOTAA64.EFI`

**Display alternatives:**
- `-display cocoa` — native macOS window (interactive, see the desktop)
- `-display none -serial stdio` — serial only (faster iteration)
- `-display cocoa,zoom-to-fit=on` — auto-scaling

### Build System Changes

**Keep `run.sh` for x86 (don't rename). Add `run-arm.sh` alongside.**

```toml
# .cargo/config.toml — remove default target, specify on command line
[unstable]
build-std = ["core", "alloc"]
build-std-features = ["compiler-builtins-mem"]
```

```toml
# rust-toolchain.toml
[toolchain]
channel = "nightly"
targets = ["x86_64-unknown-uefi", "aarch64-unknown-uefi"]
components = ["rust-src"]
```

```toml
# kernel/Cargo.toml
[target.'cfg(target_arch = "x86_64")'.dependencies]
x86_64 = "0.15"
# No aarch64 crate — hand-rolled system register access
```

Feature gate: `#![cfg_attr(target_arch = "x86_64", feature(abi_x86_interrupt))]`

---

## Implementation Phases (3 phases, merged from original 5)

### Phase 1: Architecture Restructure

**Goal:** Move x86 code into `arch/x86_64/`, define the portable interface, verify x86_64 still builds and boots. No new functionality.

**Tasks:**
- [ ] Create `kernel/src/arch/mod.rs` with cfg-switched re-exports
- [ ] Create `kernel/src/arch/x86_64/mod.rs`
- [ ] Move `gdt.rs`, `idt.rs`, `pic.rs`, `speaker.rs` into `arch/x86_64/`
- [ ] Extract timer code (PIT + TSC) into `arch/x86_64/timer.rs`
- [ ] Extract syscall asm + MSR setup into `arch/x86_64/syscall.rs`
- [ ] Extract context switch asm into `arch/x86_64/context.rs`
- [ ] Move `serial.rs` into `arch/x86_64/serial.rs`
- [ ] Move `paging.rs` into `arch/x86_64/paging.rs`
- [ ] cfg-gate `keyboard.rs`, `mouse.rs`, `speaker.rs` in main.rs (don't move yet)
- [ ] **Extract userspace syscall wrappers** (`user_send`, `user_recv`, etc.) behind `arch::syscall_raw()` — these contain `asm!("syscall")` which is x86-specific
- [ ] Update `ipc.rs` to use `arch::interrupt_disable/enable`
- [ ] Update `scheduler.rs` to call through `arch::*`
- [ ] Update `main.rs` boot sequence to call `arch::*` functions
- [ ] Verify x86_64 still compiles: `cargo build --target x86_64-unknown-uefi`
- [ ] Verify x86_64 still boots on QEMU

**Critical task missed in original plan:** The userspace syscall wrappers in main.rs (lines 243-298) contain x86 inline assembly. These must be extracted behind the arch interface or they won't compile for aarch64.

**Estimated effort:** ~600 lines changed (mostly moves + interface extraction)

### Phase 2: aarch64 Boot with Preemptive Scheduling

**Goal:** Boot on QEMU aarch64 with HVF, serial output, timer interrupts, context switching between kernel tasks.

**Tasks:**
- [ ] Create `kernel/src/arch/aarch64/mod.rs`
- [ ] Implement `uart.rs` — PL011 at 0x0900_0000 (MMIO read/write)
- [ ] Implement `exceptions.rs` — vector table in `global_asm!`, install via VBAR_EL1
- [ ] Implement `timer.rs` — CNTFRQ_EL0 frequency, CNTP_TVAL_EL0 countdown, CNTPCT_EL0 counter
- [ ] Implement `gic.rs` — GICD init, GICR wake + PPI enable, ICC system registers
- [ ] Implement `paging.rs` — identity map kernel (TTBR1_EL1), TCR_EL1, MAIR_EL1, enable MMU
- [ ] Implement `context.rs` — full register save/restore for IRQ path, timer-driven context switch
- [ ] Create `run-arm.sh` with the QEMU command line above
- [ ] Create UEFI vars copy in the build script
- [ ] Boot on QEMU HVF, see serial output: "FreshOS booting..."
- [ ] Verify timer interrupts at 1000 Hz
- [ ] Verify context switch between 2+ kernel tasks

**Estimated new code:** ~800 lines (exceptions, GIC, timer, UART, paging, context switch)

### Phase 3: User Mode + Full Desktop

**Goal:** EL0 tasks with SVC syscalls, per-task page tables, framebuffer compositor, the full desktop running at interactive speed.

**Tasks:**
- [ ] Implement `syscall.rs` — SVC handler via ESR_EL1 sync exception, AAPCS64 ABI
- [ ] Implement lightweight SVC save/restore (14 registers, not 34)
- [ ] Implement user-mode task spawn (fake eret frame to enter EL0)
- [ ] Implement per-task TTBR0_EL1 with ASID
- [ ] Port userspace syscall wrappers to `svc #0` + ARM register conventions
- [ ] Verify IPC between EL0 tasks
- [ ] Verify page fault on kernel memory access from EL0
- [ ] Verify UEFI GOP framebuffer works with ramfb
- [ ] Wire USB keyboard input (UEFI provides USB HID before ExitBootServices — test if it persists, otherwise use UART input initially)
- [ ] Boot the full compositor with shell and dashboard
- [ ] Measure frame rate — target >30fps
- [ ] Re-enable visual polish (gradient, alpha blending, etc.) at native speed
- [ ] Verify x86_64 still builds and boots (regression check)

**Estimated new code:** ~500 lines

---

## Acceptance Criteria

### Functional
- [ ] `cargo build --target x86_64-unknown-uefi` succeeds (no regression)
- [ ] `cargo build --target aarch64-unknown-uefi` succeeds
- [ ] aarch64 boots on QEMU HVF with serial output
- [ ] Timer interrupts fire, preemptive scheduling works
- [ ] User-mode tasks (EL0) with SVC syscalls
- [ ] Per-task page tables with memory isolation
- [ ] IPC (send/recv) works between tasks
- [ ] Compositor renders to framebuffer at >30fps
- [ ] Keyboard input works (UART or USB HID)

### Non-Functional
- [ ] Clean arch separation — minimal cfg in portable code (targeted, not scattered)
- [ ] Both architectures buildable from the same source tree
- [ ] No x86_64 crate imports in portable code
- [ ] Memory barriers present at every required location (checklist above)

---

## Risk Analysis

| Risk | Impact | Likelihood | Mitigation |
|------|--------|-----------|------------|
| ramfb not providing usable GOP | No framebuffer | Medium | Test early in Phase 2; fall back to virtio-gpu driver if needed |
| GIC init order wrong | Silent interrupt drops | Medium | Follow exact sequence: GICD → GICR wake → ICC; test incrementally |
| Memory barrier omissions | Subtle hangs, corruption | High | Follow the barrier checklist above; add barriers defensively |
| USB keyboard not working after ExitBootServices | No keyboard input | Medium | Use UART serial input as fallback; USB HID driver is future work |
| ASID exhaustion (>256 tasks) | TLB thrashing | Low | 256 is plenty for current task count; add ASID recycling later |
| HVF-specific EL1 limitation | Can't test EL2 features | Low | We don't need EL2; kernel runs at EL1 |

---

## Future Considerations

- **Raspberry Pi 4/5**: Install UEFI firmware (`pftf/RPi4`), same boot path. Device addresses differ (different UART, GIC addresses).
- **virtio-input**: Proper keyboard/mouse for the full desktop experience.
- **virtio-gpu**: PCI enumeration + virtio transport + GPU commands for dirty region updates.
- **ELF loader**: Architecture-neutral — design it now, build on whichever arch is running first.
- **Audio on ARM**: No PC speaker. Needs virtio-sound or direct audio hardware access.

---

## References

### Concrete Code References
- rpi-OS tutorials: `_arch/` module pattern, exception vector table in `global_asm!`
- hermit-os/kernel: `cfg_select!` dispatch, GIC/timer setup on aarch64
- ARM Architecture Reference Manual (ARMv8-A): exception model, GIC, page tables

### QEMU Virt Memory Map
| Address | Device |
|---------|--------|
| `0x0800_0000` | GICv3 Distributor |
| `0x080A_0000` | GICv3 Redistributor |
| `0x0900_0000` | PL011 UART |
| `0x0a00_0000` | virtio MMIO slots |
| `0x4000_0000` | RAM start (default) |

### GIC Interrupt IDs
| INTID | Device |
|-------|--------|
| 30 | Non-secure EL1 Physical Timer (PPI) |
| 33 | UART0 (SPI) |
| 48-79 | virtio devices (SPI) |
