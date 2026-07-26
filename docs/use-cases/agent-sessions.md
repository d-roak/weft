# Use case: two agent sessions communicating (localhost & LAN)

Give two agents — e.g. two Claude Code sessions — a channel to talk over. Each
session runs a weft node with a **persistent identity**; they exchange messages
by `EndpointId`. On one machine they connect over loopback; on a LAN they
hole-punch a direct path. No server, no ports opened.

The runnable example is [`examples/agent_chat.rs`](../../examples/agent_chat.rs).
It keeps a persistent key (so each agent's id is stable) and adopts whoever
messages it as its peer, so only one side needs the other's id to start.

## A) Two sessions on the same machine (localhost)

**Session 1**
```bash
cargo run --example agent_chat -- --key agent1.json
#   you are  AAAA…          ← copy this id
```

**Session 2**
```bash
cargo run --example agent_chat -- --key agent2.json --peer AAAA…
```

Type in either terminal; the line shows up in the other. Session 1 learns
session 2's id from the first message it receives, so both can talk freely.

For **Claude Code sessions**, each session just runs its command in the shell.
Share session 1's printed id into session 2's invocation (paste it, or keep a
known id per agent since the identity is persistent).

## B) Two machines on a LAN

Same commands, one per machine:

```bash
# machine 1
cargo run --example agent_chat -- --key agent.json
#   you are  BBBB…

# machine 2
cargo run --example agent_chat -- --key agent.json --peer BBBB…
```

iroh discovers each node from its id and hole-punches a **direct** connection
across the LAN — traffic stays on the local network once punched; the public
relay is only a fallback while connecting. Nothing about the commands changes
between localhost and LAN — the `EndpointId` is the whole address.

**Works offline too.** weft nodes run mDNS on the local network: they advertise
themselves and resolve each other's addresses over the LAN with no internet, no
relay, and no `--bootstrap`. On top of that, every mDNS-discovered node is added
to gossip's known peers automatically, so a LAN fabric self-assembles — start
the nodes and they find each other. (mDNS is best-effort; some networks block
multicast, in which case fall back to n0 DNS discovery, which needs internet.)

## Mailbox pattern (better for agents that poll)

Interactive stdin suits a human; an agent often prefers to **send one-shot and
read a log**. The core CLI already supports this — run a node that prints every
inbound message, redirect it to a file, and read the file:

```bash
# session 1: run a node; its inbox is appended to inbox.log
weft --key agent1.json up > inbox.log 2>&1 &
weft --key agent1.json id            # -> AAAA…

# session 2: fire a message to session 1
weft --key agent2.json send AAAA… "task: summarize the repo"

# session 1 (or its agent): read what arrived
cat inbox.log
#   ← message from BBBB…: "task: summarize the repo"
```

`weft send` is request/reply, so session 2 also gets session 1's ack back
immediately. Build a back-and-forth by having each side run `up` (to receive)
and `send` (to speak).

## How an agent drives this

1. **Start a node** (`weft up` or `agent_chat`) in the background; capture its
   id with `weft id`.
2. **Learn the peer's id** — from config (ids are persistent, so they're stable
   and can be pinned), from a bootstrap/discovery lookup
   ([service-discovery.md](../service-discovery.md)), or pasted by the user.
3. **Send** with `weft send <peer> <text>` and **receive** by reading the node's
   inbox output.

Because identities persist, an agent can be addressed by the same id across
restarts — pin a teammate's id once and keep messaging it.
