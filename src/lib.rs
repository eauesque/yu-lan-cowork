//! LAN Cowork — peer pairing, fleet dispatch and cross-node cowork routes.
//!
//! This crate does not depend on `yu-server`. Everything it needs from the
//! host process is reached through the `LanCoworkHost` trait, so the crate
//! builds and tests on its own and can be reviewed as a separate unit.
//!
//! Two files stay on the `yu-server` side by design:
//!
//! - `lan_cowork_host_impl.rs` — `impl LanCoworkHost for AppState`. The orphan
//!   rule forbids it here, since neither the trait's `AppState` nor several of
//!   the types it converts are owned by this crate.
//! - `lan_cowork_split_integration_tests.rs` — tests that exercise the auth
//!   middleware, `check_static_bypass` and the CSRF layer, none of which this
//!   crate owns.
//!
//! Security note for reviewers: authorization is *declared* per route but
//! *enforced* in the route body. Read the body, not the declaration. See
//! `docs/development/development_docs/LAN_COWORK_SECURITY_REVIEW.md`.
//!
//! See `docs/superpowers/plans/2026-08-06-lan-cowork-s4-crate-split.md` for
//! the split plan and the dependency-graph findings recorded against S4.

// The global seam lock is held across await on purpose: it serializes
// process-wide test state (TEST_ALLOW_LOOPBACK, TEST_DESCRIPTOR, the client
// caches) while tests run in parallel threads. This is not a temporary
// scaffold — S4e evaluated removing it (2026-08-10) and kept it deliberately.
//
// Converting TEST_LOCK to `tokio::sync::Mutex` is the lint's own suggestion but
// costs 98 call sites, 19 of which sit in sync fns (several reached through
// helpers such as `consent_test_guard`, `restart_test_guard` and two `new()`
// constructors, so the async change cascades), plus the four call sites on the
// yu-server side of the seam. That is a large edit to a 512-test suite in
// exchange for silencing a lint that is `cfg(test)`-scoped and cannot reach
// production code.
#![cfg_attr(test, allow(clippy::await_holding_lock))]

pub mod auth;
pub mod path_guard;
pub mod routes;
pub mod schema;

#[cfg(test)]
pub(crate) mod test_support;
