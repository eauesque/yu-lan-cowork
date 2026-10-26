# yu-lan-cowork

LAN peer pairing, fleet dispatch, and cross-node cowork routes — the part of
[yu_ai_manager](https://github.com/eauesque/yu_ai_manager) that talks to other
machines.

It is a separate repository so that **it can be security-reviewed on its own.**
Everything here handles peer identity, pairing secrets, tokens, and remote
command dispatch; none of it needs the rest of the application to be understood.

## Status: this repository is the source of truth

The crate used to live inside yu_ai_manager as `crates/lan-cowork` and was
mirrored here. That is no longer so: on 2026-08-11 it moved out, and this
repository is now where it is developed. yu_ai_manager consumes it as a
rev-pinned git dependency.

Development happens in a private repository and is squash-published here on
each release, so the history is one commit per publish rather than per change.
Consumers pin **this** repository, so a fresh clone or a CI runner builds it
without credentials.

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

## Security compatibility

Token renewal requires an existing, non-revoked inbound token record created by
pairing. A discovered or registered public key alone does not authorize renewal.
Unauthenticated registration inserts new peers only; it cannot replace an
existing key or endpoint. Endpoint changes use signed discovery or pairing.

Requests carrying `X-Peer-Nonce` append the nonce and a newline to the existing
canonical signed message, after the body-hash line. Both sender and verifier
must use this format. Older peers that sign only the original message must be
updated before using nonce-required routes (including fleet operations and
token renewal). There is no fallback to unsigned nonces. Nonce-free requests
retain their existing wire format. Use `sign_peer_headers` for nonce-bearing
requests; `make_signature_headers` is the legacy nonce-free helper.

Collection metadata now includes `file_ids` for each collection. Imports create
only explicitly listed membership, and only when `include_favorites` is true.
Older metadata containing collection names alone creates no membership.

Deploy these changes to every peer, including Python implementations, before
resuming fleet operations. Review existing credentials and revoke/re-pair any
peer whose original pairing cannot be confirmed; this update does not identify
credentials issued through the previous renewal behavior.

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
