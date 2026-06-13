# FreshOS v1: The First Proof

## The Test

Every decision about v1 is measured against one question:

**Does this make the architecture perceptible?**

If a feature makes the user *see* the architecture working, it's in. If it's a consequence that falls out later, it's out. v1 exists to prove a single claim: that an OS can be built where the system is visibly alive, the interaction feels fundamentally different, and the architecture makes both of those things inevitable.

---

## v1 Definitely Is

**A Rust microkernel on x86_64/UEFI** that boots on real hardware and runs in QEMU.

**Typed message-passing IPC** between isolated userspace processes. This is the spine. Every other feature is a consequence of this working and being fast.

**A GPU-accelerated compositor** that owns the frame. Deterministic frame scheduling. The compositor never misses. If an app is slow, the compositor presents the last good frame. The user never waits for the system.

**Spatial workspaces.** Zoom out to see every workspace running simultaneously, live, in real-time. Every workspace is a fully independent environment, all rendering concurrently, all visible at once. Zoom in to focus on one. This isn't switching desktops — it's changing altitude. You fly up, see the entire system alive, and dive into what you want. This is the first thing a user sees and the first thing that feels different.

**Live message-flow visualisation.** Every message between every process, rendered in real-time as a living diagram. Not a developer tool. Not hidden in a menu. A workspace you can drag down and look at, showing the system thinking. This is the signature feature — the thing that makes architecture perceptible.

**A visible latency contract.** Input-to-photon time displayed in microseconds, always visible, never exceeding the guarantee. The system proves its own performance to the user. This is the trust mechanism.

**Capability-based process isolation.** Processes hold capabilities to message channels. No capability, no access, no visibility. Security is structural and inspectable — you can see the capability graph in the introspection view.

**A userspace keyboard and mouse driver** communicating through typed messages. The kernel doesn't know what a keyboard is. It just delivered a message. This is the proof that the microkernel pattern is real.

**A basic userspace storage driver.** Enough to load and persist data. Not a full filesystem — the minimum viable path to "the system can remember things."

**One scripting integration.** A lightweight runtime (Lua, Rhai, or similar) with native access to the message layer. Enough to write three lines that glue two services together. Proves pervasive scripting is real, not theoretical.

**One or two tiny native services** that demonstrate statefulness — a notepad, a system monitor, a clock. Small enough to build in a day. Real enough to prove that native apps work, receive messages, persist state, and show up in the introspection view.

---

## v1 Definitely Is Not

**Not an emulation platform.** Emu198x convergence is the long horizon. v1 has no emulators, no vintage CPU cores, no cross-architecture bridging. That's later.

**Not a server OS.** No orchestration, no clustering, no multi-machine message routing. The server edition validates the architecture's generality. v1 validates the architecture.

**Not a smart home hub.** No Zigbee, no Z-Wave, no device bridges, no physical automation. The physical world features are consequences of the message layer working. v1 proves the message layer works.

**Not an automotive platform.** Obviously.

**Not an email client.** No IMAP bridge, no CalDAV bridge, no contacts, no real-world data integration. Those are bridge services that sit on top of a working message layer. v1 builds the message layer.

**Not a web browser.** No Servo, no sandboxed Linux VM, no web content rendering of any kind.

**Not a real-time data platform.** No market feeds, no energy monitoring, no telemetry dashboards. The architecture supports all of this. v1 doesn't need to prove it yet.

**Not a daily driver.** v1 is a proof of concept. You boot it, you experience it, you understand what FreshOS is. Then you reboot into your actual OS and get on with your day.

**Not feature-complete.** No acoustic identity yet. No semantic clipboard. No universal undo. No time-travel replay. No hot reload. No app fragments. These are all consequences of the architecture, and they'll arrive when the architecture is solid. They are not v1.

---

## The Single Demo Moment

Spatial workspaces. Live message-flow visualisation. Visible latency contract.

Three things. One screen. Two seconds to understand.

Someone sees FreshOS for the first time. They zoom out and see every workspace alive simultaneously — each one rendering, each one independent, the GPU compositing all of them without effort. They dive into the introspection workspace and see every message in the system flowing in real-time — input events, compositor frames, service heartbeats, all visible, all alive. The latency counter sits in the corner, rock-steady, proving the system keeps its promises.

That's FreshOS. Everything else is what happens after that moment lands.

---

## Success Criteria

v1 is done when:

- The kernel boots on real hardware (not just QEMU)
- Two or more userspace processes exchange typed messages
- IPC round-trip is measurably sub-microsecond
- The compositor renders spatial workspaces with GPU acceleration
- Zooming out shows all workspaces running live simultaneously
- The message-flow visualisation renders live system activity
- The latency counter is visible and stays within the guarantee
- A three-line script can connect two services through the message layer
- A non-technical person watching the demo says "how does that work?" and the answer is obvious once explained

That last criterion is the only one that actually matters.

---

*FreshOS v1 Scope — March 2026*
*Steve*
