//! e-sdk — programmatic access to e's coding-agent core.
//!
//! Status: scaffolded, not yet implemented. This package is a separate
//! consumer of e's library target behind an explicit, documented crate
//! boundary — see `docs/sdk.md` for the planned surface and
//! `docs/decisions/0002-rust-sdk-package.md` for why it is a package of its
//! own. Until the first release declares a semantic-versioning policy,
//! everything here is unstable.
//!
//! The session facade and the event stream will land here; the API mirrors
//! what `main.rs` does before handing off to the terminal frontend.
