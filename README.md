# yu-lan-cowork

LAN peer pairing, fleet dispatch, and cross-node cowork routes: host-agnostic
axum routes for machines that discover each other on a local network, pair with
a short-lived PIN, and then dispatch work between themselves over signed HTTP.

It is a separate crate so that **it can be security-reviewed on its own.**
Everything here handles peer identity, pairing secrets, tokens, and remote
command dispatch; none of it needs the rest of the application to be understood.

## What it is

The crate has no dependency on the host application. Everything it needs from
the process it runs in is reached through one trait:

```rust
pub trait LanCoworkHost: Send + Sync {
    // database handles, project paths, SSE fan-out, session authorization, ...
}
```

The host implements `LanCoworkHost`; this crate hands back
`Router<LanCoworkState>` values that merge into the host's axum router. That
seam is the whole contract — see `src/routes/lan_cowork_host.rs`.

Two things deliberately stay on the host side and are therefore **not** in this
repository:

- `impl LanCoworkHost for <HostState>` — Rust's orphan rule forbids it here.
- the integration tests that exercise the host's auth middleware, static-bypass
  table, and CSRF layer, none of which this crate owns.

## Reviewing this crate

**Authorization is enforced in each route's body, not by a declaration.** There
is no `require=` attribute to scan for — read the function. A route that looks
unguarded from its signature may gate on `require_session`, on peer token
verification, or on both; a route that looks guarded may be relying on the
host's middleware for part of it.

Two properties are worth knowing before you read:

- **Peer-to-peer traffic is plain HTTP.** Requests are signed and replay-guarded,
  so they are authenticated and tamper-evident, but they are not confidential.
  This is a deliberate choice for a LAN-local protocol, not an oversight.
- **The nonce requirement is an allowlist** (`NONCE_REQUIRED_SUFFIXES` and
  `NONCE_REQUIRED_PREFIXES` in `src/auth/peer_transport.rs`). A path that matches
  neither is accepted without a nonce. When adding a route that mutates state,
  add it to the allowlist too.

Wire formats are pinned byte-for-byte by the vectors under `tests/vectors/`,
which unit tests inside `src/` read with `include_str!`. **A failing vector test
means the wire format changed** — which is almost never what you wanted.

## Building

```sh
cargo test                                   # full suite (tests live in src/)
cargo clippy --all-targets -- -D warnings    # what CI enforces
cargo fmt --all -- --check
```

Requires a system OpenSSL (`libssl-dev` and `pkg-config` on Debian/Ubuntu) —
`openssl` supplies the CSPRNG bytes used for token and identity generation.

Dependency versions are written out explicitly in `Cargo.toml` rather than
inherited from a workspace.

## License

MIT — see [LICENSE](LICENSE).
