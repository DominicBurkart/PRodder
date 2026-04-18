use std::env;

use tracing_subscriber::FmtSubscriber;

use prodder::drafter;

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
