---
name: weft-agent-comms
description: Communicate with another agent or Claude session over weft's P2P fabric — send messages, receive them, discover peers, on localhost or LAN. Use when the user wants two Claude Code sessions (or agents) to talk to each other, exchange messages, coordinate, or set up a peer-to-peer channel between machines.
---

# Talking to another agent over weft

weft gives each session a stable `EndpointId` and a direct P2P channel to any
other node — across NATs, no server. Use it to let two Claude sessions (or any
agents) message each other on the same machine or across a LAN.

`weft` runs as a **background daemon**; you drive it with short CLI commands.

## 0. Install the binary (once)

If `weft` isn't on PATH, install the prebuilt binary — no Rust toolchain needed:

```bash
command -v weft >/dev/null || curl -fsSL https://raw.githubusercontent.com/d-roak/weft/main/install.sh | sh
```

This drops `weft` in `~/.local/bin`. If that isn't on your PATH, either run it as
`~/.local/bin/weft` or `export PATH="$HOME/.local/bin:$PATH"`. Verify:

```bash
weft --version
```

## Your identity

Each session has a persistent identity keyed by a file. Give each session its
own key so their ids differ:

```bash
weft --key session-a.json id     # prints THIS session's EndpointId
```

The id is stable across restarts — pin a peer's id once and reuse it.

## Send and receive (the agent workflow)

```bash
# 1. Start this session's background daemon
weft --key session-a.json start

# 2. Get your id to share with the other session
weft --key session-a.json id                 # -> AAAA…

# 3. Send a message to a peer (you need their EndpointId)
weft --key session-a.json send BBBB… "task: summarize the repo"
#   ← ack "received"                          (send is request/reply)

# 4. Drain messages that arrived
weft --key session-a.json inbox
#   ← message from BBBB…: "task: summarize the repo"

# 5. Check or stop the daemon
weft --key session-a.json status
weft --key session-a.json stop
```

Both sessions `start` a daemon and use `send`/`inbox`. `inbox` clears messages as
it prints them, so poll it to pick up new ones.

## Discovering peers you don't know

- **Same LAN:** nothing to do — nodes find each other over mDNS (works offline)
  and auto-join. List what's around:
  ```bash
  weft --key session-a.json services
  ```
- **Different networks:** exchange ids out of band (the user pastes the other
  session's id), or bootstrap through a known peer when starting:
  ```bash
  weft --key session-a.json start --bootstrap KNOWN_PEER_ID
  weft --key session-a.json services
  ```

## How to run this as an agent

1. Ensure `weft` is installed (step 0).
2. Pick a unique `--key` file for this session and `weft start`.
3. Get the peer's id (from the user, from `services`, or a pinned config id).
4. `weft send` to the peer; `weft inbox` to receive. `weft stop` when done.

## Notes

- `send` returns the peer's reply synchronously — use it for request/response.
- The daemon auto-acks and buffers every inbound message; `weft inbox` drains it.
- Ids are stable, so a teammate session can be addressed by the same id later.
- `--key` also selects which daemon the CLI talks to (one key = one daemon).
- More detail: `docs/use-cases/agent-sessions.md` in the repo.
