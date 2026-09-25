---
title: "0005 — FreshOS is reachable over MCP"
type: decision
status: proposed
date: 2026-09-25
deciders: Steve
---

# 0005 — FreshOS is reachable over MCP

- **Status:** Proposed
- **Date:** 2026-09-25
- **Deciders:** Steve

## Decision

The whole OS is reachable by agents over MCP, through one bridge service
that translates MCP into FreshOS's typed messages.

- **Message endpoints become MCP tools.** Observable state becomes MCP
  resources: the task list, the message trace, metrics, and later the
  capability graph.
- **Agent access goes through capabilities, never around them.** The bridge
  holds a capability set like any other service. Everything an agent does
  is a message, so it is visible in the flow view and can be revoked.
- **Parity:** anything a person can do through the interface is reachable
  through messages, and therefore through MCP. This matches the 198x
  family's agent-native parity rule.
- **Transport:** first, stdio over a dedicated serial channel, with a small
  shim on the development Mac connecting any MCP client to the serial port.
  TCP comes once rung 5 (networking) exists.
- **Phasing:** a read-only first version (tasks, trace, metrics) is rung 1
  tooling for the Pi 4 bring-up. Write tools wait until capabilities
  (roadmap M5) exist.

## Why

- It is the manifesto's own bridge pattern and discovery principle ("ask
  the system what can I talk to?"), applied to agents.
- It serves both twin goals (0002). An agent's actions are messages, so you
  can watch an agent operate the machine, see what it touched, and revoke
  its access. On other systems agents scrape the screen or get a shell with
  full permissions.
- Emu198x already exposes MCP, so the 198x family speaks one protocol on and
  off FreshOS.
- A read-only bridge lets an agent inspect a real Pi during bring-up: the OS
  as its own debugger, for agents too.

## Consequences

- MCP needs its own serial channel, separate from log output: a second
  serial device on QEMU and a second PL011 UART on the Pi 4.
- JSON handling needs `serde` and `serde_json` with `alloc`, without `std`.
  These are new dependencies, to be confirmed when implementation starts.
- The host shim is a small development tool in this repo. It is not part of
  the OS.

## Drift triggers

- A debug path that gives the bridge access capabilities don't grant
- Write tools before capabilities exist
- A UI feature that can't be reached through messages
- Building a TCP stack for MCP ahead of rung 5
