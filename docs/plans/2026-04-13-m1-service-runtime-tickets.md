---
title: "plan: milestone 1 service runtime tickets"
type: plan
date: 2026-04-13
---

# Milestone 1 Ticket Breakdown

This is the concrete ticket list for **Milestone 1: Generic Service Runtime**.

The intent is to get from "the kernel knows a few special services" to "FreshOS has one service model that `init` can observe and supervise."

## Ticket 1 — Service Descriptor Table

- Move service metadata into one kernel-owned table.
- Include:
  - service id
  - name
  - flags
  - restart period
  - spawn hook
- Export descriptor enumeration to `init`.

**Done when:**

- `init` can discover the service set without hardcoded ids in its own source.

## Ticket 2 — Service Status ABI

- Export per-service runtime state through one ABI.
- Include:
  - current task id
  - running state
  - restart count
  - exit count
  - last exit reason

**Done when:**

- `init` can ask for service status without reading kernel logs.

## Ticket 3 — Exit Reason Propagation

- Thread explicit exit reasons through:
  - clean service exit
  - syscall exit
  - synchronous fault exit
- Keep the reason attached to the service record.

**Done when:**

- `init` and future tools can distinguish faulted services from cleanly exited ones.

## Ticket 4 — Status-Based Supervision

- Remove blind periodic respawn attempts from `init`.
- Restart only after observing a real exit.
- Respect per-service restart backoff.

**Done when:**

- a supervised service restarts after exit
- a running service is not repeatedly "respawned" on a timer

## Ticket 5 — Service List Command Plumbing

- Add a kernel-to-`init` path to report the current service table and status data cleanly.
- Use it to support a future `ps` or `services` command.

**Done when:**

- there is one code path for service inspection
- no extra one-off probe path is needed

## Ticket 6 — Per-Service Logging Attribution

- Attribute kernel lifecycle logs to service name/id consistently.
- Add a small per-service ring buffer or a shared log stream with service tags.

**Done when:**

- `init` can surface recent service logs for debugging restarts

## Ticket 7 — Externalize One Real Service

- Move one consequential service onto the generic runtime path.
- Prefer the shell or dashboard before the compositor.

**Done when:**

- the service is loaded externally
- `init` starts it from the same generic service model
- restart works after both clean exit and fault

## Ticket 8 — Service Control Surface

- Add a minimal control API for:
  - start
  - restart
  - stop
  - query status
- Keep it narrow and enough for shell commands later.

**Done when:**

- a userland tool can ask `init` to restart a service without kernel special-casing

## Ticket 9 — Demo Closure

- Run one scripted demo:
  - boot
  - show services
  - kill a supervised service
  - observe exit reason
  - observe restart

**Done when:**

- Milestone 1 has a repeatable demo that proves the service runtime is real
