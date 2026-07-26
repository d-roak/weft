# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

**weft** is a peer-to-peer communication fabric for agents, built on
[iroh](https://docs.rs/iroh). Nodes get a stable cryptographic identity
(`EndpointId`) and reach each other directly across NATs — no central server.

The guiding principle: **iroh does connectivity; weft does not.** Identity, NAT
traversal, hole-punching, relay fallback and address discovery are all iroh's.
weft is the thin layer on top: an agent messaging protocol, gossip-based service
discovery, and an x402 payment convention. Don't reimplement transport concerns
here — configure iroh instead.

## Layout

Cargo workspace:

```
crates/weft/          the library + the `weft` CLI binary
  src/lib.rs          Weft node: endpoint + gossip + router wiring, Config
  src/agent.rs        agent messaging protocol (ALPN weft/agent/0)
  src/discovery.rs    gossip service registry, mDNS LAN auto-peering
  src/x402.rs         payment handshake types + verification seam
  src/main.rs         CLI: daemon + client subcommands
  src/control.rs      CLI↔daemon protocol (Unix socket, JSON lines)
  examples/           agent_chat, iot_sensor, x402_relay
crates/weft-relay/    the `weft-relay` self-hosted relay server binary
docs/                 architecture, connectivity, discovery, self-hosting, use cases
skills/claude/        the `weft-plugin` Claude Code skill
```

Dependency versions live in the root `[workspace.dependencies]`; crates
reference them with `foo.workspace = true`. Add a version in one place only.

## Process model

`weft` runs as a **background daemon** owning the live node; all other CLI
commands are thin clients that talk to it over a Unix socket at
`/tmp/weft-<hash>.sock` (hashed from the `--key` path, so one key = one daemon).
`weft start` spawns `weft daemon` detached in its own process group.

Sockets live in `/tmp`, not next to the key file: Unix socket paths are capped
around 104 bytes and key paths can be deep.

## Conventions

- **Comment style.** Comments explain *why*, not *what*. Deliberate
  simplifications are marked `ponytail:` and name the upgrade path, e.g.
  `// ponytail: open relay. Add an allowlist here if you need to restrict …`.
  Keep that marker when you touch such code; remove it only when you actually
  implement the upgrade.
- **Simplicity first.** Prefer the smallest thing that works. No abstraction
  with a single implementation, no config for a value that never changes.
- **Non-trivial logic leaves a runnable check.** See the unit test in
  `x402.rs`. No test frameworks or fixtures beyond `cargo test`.
- **Docs are part of the change.** If you change the CLI, update `README.md`,
  the relevant page in `docs/`, and `skills/claude/SKILL.md` in the same pass.

## Working on this repo

```bash
cargo build --all-targets     # workspace
cargo test                    # unit tests
cargo clippy --all-targets    # must be warning-free
```

**Verify against real nodes, not just the type checker.** The meaningful test is
two daemons actually exchanging a message:

```bash
weft --key /tmp/a.json start
weft --key /tmp/b.json start
weft --key /tmp/b.json send "$(weft --key /tmp/a.json id)" "hello"
weft --key /tmp/a.json inbox
weft --key /tmp/a.json stop && weft --key /tmp/b.json stop
```

Always `stop` daemons you start, and clean up scratch key files.

### The iroh API moves fast

iroh renamed core types recently (`Node`→`Endpoint`, `NodeId`→`EndpointId`,
`NodeAddr`→`EndpointAddr`). **Do not write iroh code from memory.** Check the
vendored source of the pinned version before using an API:

```bash
ls ~/.cargo/registry/src/*/iroh-1.0.3/src/
```

Many config structs in `iroh-relay` are `#[non_exhaustive]` — construct them
with `new()` and assign fields, not struct literals.

## Infrastructure

Nodes default to n0's public relays and DNS discovery. `Config` (and the
`--relay` / `--pkarr-relay` flags, or `WEFT_RELAY` / `WEFT_PKARR_RELAY`) point
them at self-hosted infrastructure instead; `crates/weft-relay` is the relay
server. See [docs/self-hosting.md](docs/self-hosting.md). Keep n0 the default —
zero-config should keep working — and keep self-hosting a flag away.

## Release

Tagging `v*` triggers `.github/workflows/release.yml`, which builds `weft` and
`weft-relay` for linux/macOS × x86_64/aarch64 and attaches them to the release.
`install.sh` fetches those binaries. Windows is deliberately unsupported (the
daemon uses Unix sockets).

## Git

- **Never commit unless explicitly asked.**
- Conventional Commits, one line, no body: `feat(cli): …`, `fix(relay): …`.
- No AI attribution in commit messages.
