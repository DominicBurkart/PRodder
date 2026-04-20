//! Wasm-only stubs for `lib.rs` entry points. Compiled only when
//! `target_arch = "wasm32"`. None of these can be exercised by
//! `cargo test` on a native target, so this file is excluded from
//! coverage in `codecov.yml` and `tarpaulin.toml`.
//!
//! Browser/WASI runtime parity is tracked in #19; the stubs return a
//! pointer to that issue rather than panicking.

use tracing_subscriber::FmtSubscriber;

pub(crate) async fn run_drafter(_token: String) -> anyhow::Result<()> {
    Err(anyhow::anyhow!(
        "drafter execution on wasm32 not implemented; see #19"
    ))
}

pub(crate) fn init_tracing(level: tracing::Level) {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(level)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);
}

pub(crate) fn read_token() -> anyhow::Result<String> {
    Err(anyhow::anyhow!(
        "GH_TOKEN provisioning not implemented for wasm; see #19"
    ))
}
