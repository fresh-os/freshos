---
title: "feat: Virtio-GPU 2D driver with dirty-region display updates"
type: feat
date: 2026-04-02
deepened: 2026-04-02
reviewed: 2026-04-02
---

# Virtio-GPU 2D Driver

## Summary

Replace ramfb with virtio-GPU 2D. The compositor tracks dirty regions and transfers only changed pixels to the host each frame. A cursor blink moves ~1 KB instead of 1.9 MB.

Single file: `kernel/src/arch/aarch64/virtio_gpu.rs`.

## Why

Without dirty-rect tracking, virtio-GPU full-frame is *worse* than ramfb (same data, more protocol overhead). Dirty rects are the entire point — they're the feature, not polish.

---

## Phase 1: MMIO Transport + First Command

**Goal:** probe bus, init device, send GET_DISPLAY_INFO, print resolution.

### Device Probe

32 MMIO slots at `0x0a00_0000`, stride `0x200`. Scan for MagicValue `0x74726976`, Version 2, DeviceID 16 (GPU).

### Init Sequence

1. Status = 0 (reset)
2. Status |= ACKNOWLEDGE (1)
3. Status |= DRIVER (2)
4. Read DeviceFeatures page 0; write DriverFeatures = 0 (no optional features)
5. Status |= FEATURES_OK (8); re-read to confirm
6. Set up virtqueue 0: QueueSel=0, read QueueNumMax, QueueNum=16, write desc/avail/used addresses, QueueReady=1
7. Status |= DRIVER_OK (4)

### Memory Layout (1 page = 4 KiB)

```
0x000  Descriptor table   16 × 16 = 256 bytes  (align 16)
0x100  Available ring     4 + 16×2 = 36 bytes   (align 2)
0x200  Used ring          4 + 16×8 = 132 bytes   (align 4)
0x400  Command buffer     512 bytes
0x600  Response buffer    512 bytes
```

Compile-time assertion that used ring end (0x284) < command buffer start (0x400):
```rust
const QUEUE_SIZE: usize = 16;
const USED_END: usize = 0x200 + 4 + QUEUE_SIZE * 8; // 0x284
const CMD_OFFSET: usize = 0x400;
const _: () = assert!(USED_END <= CMD_OFFSET);
```

### Command/Response

2-descriptor chain per command. Desc 0: command (readable, flags=NEXT, next=1). Desc 1: response (writable, flags=WRITE).

Batching uses desc pairs 0+1 and 2+3 concurrently. After `poll_used(n)` returns, all descriptors are free for reuse. Zero flags before reuse.

### Barriers

```rust
// Barrier choice: DMB OSHST (store-only, outer-shareable) orders our
// stores relative to the device. DSB SY (used in gic.rs) is stricter
// than needed here. See ARM ARM B2.7.3.
core::arch::asm!("dmb oshst");  // after writing descs + avail ring
// ... write avail.idx ...
core::arch::asm!("dmb oshst");  // before QueueNotify MMIO write
```

### Poll Timeout

Bounded poll (100,000 iterations). Panic with diagnostic if device never responds:
```rust
for _ in 0..100_000 {
    if read_volatile(&used.idx) != last_used_idx { return Ok(()); }
    core::hint::spin_loop();
}
panic!("virtio-GPU: device not responding");
```

### Error Response Checking

Every command checks the response type. Panic with the error code on anything other than OK_NODATA / OK_DISPLAY_INFO:
```rust
let resp_type = read_volatile(resp_header.cmd_type);
if resp_type != RESP_OK_NODATA && resp_type != RESP_OK_DISPLAY_INFO {
    panic!("virtio-GPU: command {:#x} failed with {:#x}", cmd_type, resp_type);
}
```

### Test

- Probe prints: "virtio-GPU found at slot N"
- Init completes: "Status = DRIVER_OK"
- GET_DISPLAY_INFO prints: "Display: WxH"

---

## Phase 2: GPU Resources + Desktop Rendering + Dirty Rects

**Goal:** create a GPU resource, attach backing, set scanout, wire into the compositor with dirty-region tracking. This is the finish line — no separate "integration phase."

### GPU Commands

| Value | Name | Notes |
|-------|------|-------|
| 0x0100 | GET_DISPLAY_INFO | response: 16 × {rect, enabled, flags} = 408 bytes |
| 0x0101 | RESOURCE_CREATE_2D | resource_id, format (1=B8G8R8A8), width, height |
| 0x0103 | SET_SCANOUT | rect, scanout_id, resource_id |
| 0x0104 | RESOURCE_FLUSH | rect, resource_id |
| 0x0105 | TRANSFER_TO_HOST_2D | rect, offset, resource_id |
| 0x0106 | RESOURCE_ATTACH_BACKING | resource_id, nr_entries, [{addr, length, pad}...] in same descriptor |

TRANSFER_TO_HOST_2D `offset` = byte offset into backing: `(y * width + x) * 4`.

### Init Flow

1. GET_DISPLAY_INFO → width, height
2. RESOURCE_CREATE_2D(id=1, B8G8R8A8, width, height)
3. Allocate backing: `allocate_contiguous(width * height * 4 / 4096 + 1)`
4. RESOURCE_ATTACH_BACKING(id=1, backing_phys, width×height×4)
5. SET_SCANOUT(scanout=0, resource=1, rect={0,0,w,h})

### Cache Coherency Gate

**Hard gate before compositor integration.** Test with a stripe pattern:

1. Fill backing store with colour A. Transfer + flush. Verify.
2. Fill a 64-byte-aligned stripe (one cache line) with colour B.
3. Transfer + flush WITHOUT cache maintenance.
4. If stripe shows colour A → HVF doesn't handle coherency. Implement DC CIVAC.

Default implementation: DC CIVAC loop over dirty region before each transfer. Apple Silicon cache lines are 64 bytes:
```rust
fn clean_cache_range(start: u64, len: u64) {
    let mut addr = start & !63; // align to cache line
    let end = start + len;
    while addr < end {
        unsafe { core::arch::asm!("dc civac, {}", in(reg) addr, options(nomem)); }
        addr += 64;
    }
    unsafe { core::arch::asm!("dsb ish", options(nomem, nostack)); }
}
```

If HVF handles coherency transparently (test passes without DC CIVAC), skip the loop and document why.

### Public API

```rust
pub struct VirtioGpu { /* mmio_base, queue/cmd page phys, backing, width, height, stride */ }

impl VirtioGpu {
    /// Probe, init, create resource, attach backing. Returns (gpu, framebuffer).
    pub fn init() -> Option<(Self, Framebuffer)>;

    /// Transfer + flush a rectangular region. Batched into one notify.
    pub fn present_rect(&mut self, x: u32, y: u32, w: u32, h: u32);

    /// Transfer + flush the entire screen.
    pub fn present_full(&mut self);
}
```

`init()` returns a `Framebuffer` pointing at the backing store. The compositor draws directly to it. `is_bgr = true` always (B8G8R8A8_UNORM).

`present_rect` is public from day one — the compositor needs it for dirty rects.

### Dirty-Rect Tracking in the Compositor

Four fixed zones. Each zone tracks whether it was touched this frame:

```rust
struct DirtyZones {
    menu:    Option<(u32, u32, u32, u32)>,  // zone 0: menu bar
    content: Option<(u32, u32, u32, u32)>,  // zone 1: window content area
    stats:   Option<(u32, u32, u32, u32)>,  // zone 2: stats overlay
    taskbar: Option<(u32, u32, u32, u32)>,  // zone 3: taskbar
}
```

The compositor already knows what it drew. After each draw call, mark the zone:
```rust
// Drawing menu bar → mark zone 0
dirty.menu = Some((0, 0, sw as u32, MENU_H as u32));

// Drawing window content → mark zone 1 with bounding rect
dirty.content = Some((win_x as u32, content_top as u32, win_w as u32, content_h as u32));
```

End of frame: transfer + flush each dirty zone, skip clean zones:
```rust
if let Some((x, y, w, h)) = dirty.menu    { gpu.present_rect(x, y, w, h); }
if let Some((x, y, w, h)) = dirty.content { gpu.present_rect(x, y, w, h); }
if let Some((x, y, w, h)) = dirty.stats   { gpu.present_rect(x, y, w, h); }
if let Some((x, y, w, h)) = dirty.taskbar { gpu.present_rect(x, y, w, h); }
```

Typical frame (no workspace switch): only menu bar (clock tick) + content area (surface blit). Two `present_rect` calls instead of a full-frame transfer.

Workspace switch: all four zones dirty → full frame. Acceptable, happens rarely.

### Compositor Changes

```rust
// Before:
let mut bb = Framebuffer::new(bb_addr, sw, sh, sw, is_bgr);
loop {
    // ... draw to bb ...
    bb.copy_to(&mut fb);
    yield_now();
}

// After:
let (mut gpu, mut bb) = VirtioGpu::init().expect("virtio-GPU");
loop {
    let mut dirty = DirtyZones::new();
    // ... draw to bb, marking dirty zones ...
    dirty.present(&mut gpu);
    yield_now();
}
```

### Test

- Solid colour fills screen (Phase 2 baseline)
- Cache stripe test passes (coherency gate)
- Full desktop renders through virtio-GPU
- Keyboard input works end-to-end
- Dirty-region mode: type a char, observe only content zone transfers
- x86_64 still builds and boots

---

## Acceptance Criteria

### Functional
- [ ] MMIO probe finds GPU device (device ID 16)
- [ ] Feature negotiation + virtqueue setup (Status = DRIVER_OK)
- [ ] GET_DISPLAY_INFO returns valid resolution
- [ ] Resource create + attach + scanout succeeds
- [ ] TRANSFER_TO_HOST_2D + RESOURCE_FLUSH displays pixels
- [ ] Cache coherency verified (stripe test)
- [ ] Full desktop renders through virtio-GPU with dirty-rect tracking
- [ ] Dirty rects measurably reduce transfer size vs full-frame
- [ ] x86_64 target still builds and boots

### Non-Functional
- [ ] No new crate dependencies
- [ ] All MMIO via volatile read/write
- [ ] Two DMB OSHST barriers per submission batch
- [ ] Compile-time assertions on memory layout
- [ ] Poll timeout with diagnostic panic
- [ ] Error response checked on every command
- [ ] VirtioGpu is a local variable in the compositor

---

## Risk Analysis

| Risk | Likelihood | Impact | Mitigation |
|------|-----------|--------|------------|
| Cache coherency — stale pixels | Medium | High | Stripe test as hard gate; DC CIVAC default; non-cacheable fallback |
| QueueNotify doesn't trigger device | Low | High | DMB before notify; poll InterruptStatus as diagnostic |
| MMIO region mapped as Normal (not Device) | Low | High | Verify UEFI page table attrs for 0x0a00_0000 during Phase 1 |
| GET_DISPLAY_INFO unexpected format | Low | Medium | Parse carefully; hardcode 800×600 as fallback |
| Backing buffer fragmentation | Low | Medium | Allocate early (before surfaces) |

---

## References

- [Virtio 1.2 Spec](https://docs.oasis-open.org/virtio/virtio/v1.2/virtio-v1.2.html) — MMIO: §4.2, GPU: §5.7
- QEMU virt — MMIO at 0x0a000000, stride 0x200, IRQs at INTID 48+
- Existing MMIO pattern: `kernel/src/arch/aarch64/gic.rs`
- [QEMU HVF graphics #1635](https://gitlab.com/qemu-project/qemu/-/issues/1635) — ramfb issue; virtio-GPU unaffected
