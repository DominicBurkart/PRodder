// `reqwest`'s transitive dependency tree pulls in multiple versions of a
// few common crates (e.g. `core-foundation`, `getrandom`, `hashbrown`,
// `windows-sys`, `wit-bindgen`). These are outside our control until the
// upstream crates align, so silence the cargo lint at the crate root.
#![allow(clippy::multiple_crate_versions)]

use std::env;

use tracing_subscriber::FmtSubscriber;

mod drafter;

fn main() -> anyhow::Result<()> {
    #[cfg(not(debug_assertions))]
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::INFO)
        .json()
        .finish();

    #[cfg(debug_assertions)]
    let subscriber = FmtSubscriber::builder()
        .with_max_level(tracing::Level::DEBUG)
        .json()
        .finish();

    tracing::subscriber::set_global_default(subscriber)
        .expect("Setting default subscriber failed");

    let token = env::var("GH_TOKEN")
        .map_err(|_| anyhow::anyhow!("GH_TOKEN not set"))?;
    // Safety: single-threaded at this point; no other threads read env.
    unsafe { env::remove_var("GH_TOKEN") };
    drafter::run(&token)
}
