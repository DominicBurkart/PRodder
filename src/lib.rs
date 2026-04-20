//! Library entry points for the `prodder` binary. Exists so the binary's
//! `main` reduces to a single call and all logic is unit-testable.

#[cfg(not(target_arch = "wasm32"))]
use std::env;

#[cfg(not(target_arch = "wasm32"))]
use tracing_subscriber::FmtSubscriber;

pub mod drafter;

#[cfg(target_arch = "wasm32")]
mod wasm;

/// Binary entry point. Initializes tracing, reads the token from the
/// environment (and removes it to avoid leaking into child processes
/// other than the ones we explicitly configure), and runs the drafter.
///
/// # Errors
/// Returns an error if `GH_TOKEN` is not set or if the drafter fails.
pub async fn real_main() -> anyhow::Result<()> {
    init_tracing();
    let token = read_token()?;
    run_drafter(token).await
}

#[cfg(not(target_arch = "wasm32"))]
async fn run_drafter(token: String) -> anyhow::Result<()> {
    tokio::task::spawn_blocking(move || drafter::run(&token)).await?
}

#[cfg(target_arch = "wasm32")]
async fn run_drafter(token: String) -> anyhow::Result<()> {
    wasm::run_drafter(token).await
}

#[cfg(not(target_arch = "wasm32"))]
fn init_tracing() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(max_level())
        .json()
        .finish();
    // set_global_default can only succeed once per process; ignore errors
    // so repeated calls (e.g., from tests) are harmless.
    let _ = tracing::subscriber::set_global_default(subscriber);
}

#[cfg(target_arch = "wasm32")]
fn init_tracing() {
    wasm::init_tracing(max_level());
}

#[cfg(debug_assertions)]
const fn max_level() -> tracing::Level {
    tracing::Level::DEBUG
}

#[cfg(not(debug_assertions))]
const fn max_level() -> tracing::Level {
    tracing::Level::INFO
}

#[cfg(not(target_arch = "wasm32"))]
fn read_token() -> anyhow::Result<String> {
    let token = env::var("GH_TOKEN")
        .map_err(|_| anyhow::anyhow!("GH_TOKEN not set"))?;
    // Safety: binary `main` is single-threaded at this point, and tests
    // that touch this function serialize via `ENV_LOCK`.
    unsafe { env::remove_var("GH_TOKEN") };
    Ok(token)
}

#[cfg(target_arch = "wasm32")]
fn read_token() -> anyhow::Result<String> {
    wasm::read_token()
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::{
        env, init_tracing, max_level, read_token, real_main,
    };
    use std::sync::Mutex;

    // Serialize tests that touch process-wide env vars.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn max_level_matches_build_profile() {
        let lvl = max_level();
        #[cfg(debug_assertions)]
        assert_eq!(lvl, tracing::Level::DEBUG);
        #[cfg(not(debug_assertions))]
        assert_eq!(lvl, tracing::Level::INFO);
    }

    #[test]
    fn init_tracing_is_idempotent() {
        // First call may succeed; subsequent calls must not panic.
        init_tracing();
        init_tracing();
    }

    #[test]
    fn read_token_errors_when_unset() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Safety: guarded by ENV_LOCK.
        unsafe { env::remove_var("GH_TOKEN") };
        let err = read_token().unwrap_err();
        assert!(err.to_string().contains("GH_TOKEN"));
    }

    #[test]
    fn read_token_returns_and_clears_env() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Safety: guarded by ENV_LOCK.
        unsafe { env::set_var("GH_TOKEN", "hunter2") };
        let tok = read_token().unwrap();
        assert_eq!(tok, "hunter2");
        assert!(env::var("GH_TOKEN").is_err());
    }

    // Held across an `.await` on purpose: the lock serialises
    // process-wide env-var mutation, and the call under test reads those
    // env vars, so the lock must cover the entire `real_main` future.
    // The runtime is `current_thread`, so there is no cross-thread
    // contention on the std `Mutex`.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn real_main_errors_without_token() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Safety: guarded by ENV_LOCK.
        unsafe { env::remove_var("GH_TOKEN") };
        assert!(real_main().await.is_err());
    }

    // See comment on `real_main_errors_without_token` re:
    // `await_holding_lock` — same rationale.
    #[allow(clippy::await_holding_lock)]
    #[cfg(unix)]
    #[tokio::test]
    async fn real_main_runs_with_stubbed_curl() {
        // Uses PRODDER_CURL_BIN to point drafter's curl at a stub that
        // returns an empty search. Exercises the full real_main path.
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stub = crate::drafter::tests::write_stub_script(
            r#"{"items": []}"#,
            0,
        );
        // Safety: guarded by ENV_LOCK.
        unsafe {
            env::set_var("GH_TOKEN", "x");
            env::set_var("PRODDER_CURL_BIN", &stub);
        }
        let res = real_main().await;
        // Safety: guarded by ENV_LOCK.
        unsafe { env::remove_var("PRODDER_CURL_BIN") };
        assert!(res.is_ok(), "real_main failed: {res:?}");
    }
}
