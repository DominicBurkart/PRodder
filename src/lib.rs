//! Library surface for PRodder — exposes [`config`] so integration
//! tests and benches can exercise the configuration without going
//! through the drafter's network code.
//!
//! The `drafter` module is included here so `config` can call
//! [`drafter::curl_get_user`] for `Users::resolve`; the network
//! helpers remain crate-private.

pub mod config;
pub mod drafter;
