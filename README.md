# yu-lan-cowork

LAN peer pairing, fleet dispatch, and cross-node cowork routes — the part of
[yu_ai_manager](https://github.com/habe358/yu_ai_manager) that talks to other
machines.

It is a separate repository so that **it can be security-reviewed on its own.**
Everything here handles peer identity, pairing secrets, tokens, and remote
command dispatch; none of it needs the rest of the application to be understood.

## Status: mirror, not yet the source of truth

> yu_ai_manager's Rust migration is still in progress, and neither repository is
> public yet. Until both go public **together**:
>
> - The source of truth is `crates/lan-cowork` inside yu_ai_manager.
> - This repository is a mirror kept in step by
>   `scripts/sync-from-yu-ai-manager.sh`, which has a `--check` mode that fails
>   on drift. Do not edit `src/` here by hand — the next sync would overwrite it.
>
> When both repositories go public, yu_ai_manager switches to a tag-pinned git
> dependency on this one and the mirroring stops. This section goes away then.

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

Two files deliberately stay on the host side and are therefore **not** in this
repository:

- `impl LanCoworkHost for <HostState>` — Rust's orphan rule forbids it here.
- the integration tests that exercise the host's auth middleware, static-bypass
  table, and CSRF layer, none of which this crate owns.

## Reviewing this crate

**Read [`docs/SECURITY_REVIEW.md`](docs/SECURITY_REVIEW.md) first.** It contains
only what you cannot learn by reading the code here — the invariants that live
across the crate boundary, the primitives that fail *open*, and the deviations
that were known and accepted at separation time.

The one thing worth repeating outside that document: **authorization is enforced
in each route's body, not by a declaration.** There is no `require=` attribute to
scan for. Read the function.

## Building

```sh
cargo test
```

Requires a system OpenSSL (`libssl-dev` on Debian/Ubuntu) — `openssl` is used
for CSPRNG bytes in token and identity generation.

Dependency versions are written out explicitly in `Cargo.toml` rather than
inherited from a workspace; they are pinned to match yu_ai_manager's workspace.

## License

MIT — see [LICENSE](LICENSE).
