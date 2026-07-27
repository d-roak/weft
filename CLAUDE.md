# CLAUDE.md

**weft** — P2P communication fabric for agents, built on [iroh](https://docs.rs/iroh).
Nodes get a stable identity (`EndpointId`) and reach each other across NATs, no server.

- **iroh does connectivity; weft does not.** Identity, NAT traversal, relay fallback
  and address discovery are iroh's. Don't reimplement transport — configure iroh.

## Layout

- `crates/weft/` — library + `weft` CLI (`lib.rs` node/Config, `agent.rs` messaging,
  `discovery.rs` gossip + mDNS, `x402.rs` payments, `main.rs` CLI, `control.rs` daemon IPC)
- `crates/weft-bootstrap/` — `weft-bootstrap` long-lived gossip seed peer
- `crates/weft-relay/` — `weft-relay` self-hosted relay server
- `docs/`, `skills/weft/` — docs and the `/weft` skill (published as a Claude Code
  plugin; manifests in `.claude-plugin/`)
- Dep versions live once in root `[workspace.dependencies]`; crates use `foo.workspace = true`

## Process model

- `weft` is a **background daemon**; other CLI commands are thin clients over a Unix socket
- Socket is `/tmp/weft-<hash>.sock`, hashed from `--key` (one key = one daemon)
- Sockets go in `/tmp`, not next to the key — Unix socket paths cap at ~104 bytes

## Gotchas

- **Never write iroh code from memory** — it renamed core types (`Node`→`Endpoint`,
  `NodeId`→`EndpointId`). Check `~/.cargo/registry/src/*/iroh-1.0.3/src/` first
- `iroh-relay` config structs are `#[non_exhaustive]` — use `new()` + field assignment
- Keep n0's public relays/discovery the default; self-hosting stays one flag away
  (`--relay` / `--pkarr-relay`, see [docs/self-hosting.md](docs/self-hosting.md))

## Conventions

- Comments explain *why*. `ponytail:` marks a deliberate shortcut + its upgrade path — keep it
- Simplest thing that works: no single-impl abstractions, no config for constants
- Non-trivial logic leaves one runnable check (see `x402.rs`); no test frameworks
- Changing the CLI means updating `README.md`, `docs/`, and `skills/weft/SKILL.md` too

## Checks

```bash
cargo build --all-targets && cargo test && cargo clippy --all-targets  # must be warning-free
```

- **Verify with real nodes, not just the type checker:** `start` two daemons with different
  `--key`s, `send` between them, check `inbox`, then `stop` both and clean up key files

## Git

- **Never commit unless explicitly asked**
- Conventional Commits, one-line subject, no body
- Claude's commits carry one trailer: `Assisted-by: Claude <noreply@anthropic.com>`
- **Never write `Co-Authored-By` anywhere** — commits, PR bodies, issues. Same for
  "Generated with Claude"
