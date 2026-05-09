//! Library entry points for the `prodder` binary. Exists so the binary's
//! `main` reduces to a single call and all logic is unit-testable.

// reqwest's transitive dependency tree pulls in two versions of
// `getrandom` and `windows-sys`. Neither is something we can resolve
// without upstream changes, so allow the clippy lint at the crate root.
#![allow(clippy::multiple_crate_versions)]

use std::env;

use tracing_subscriber::FmtSubscriber;

pub mod drafter;

/// Binary entry point. Initializes tracing, reads the token from the
/// environment (and removes it to avoid leaking into child processes
/// other than the ones we explicitly configure), and runs the drafter.
///
/// # Errors
/// Returns an error if `GH_TOKEN` is not set or if the drafter fails.
pub fn real_main() -> anyhow::Result<()> {
    init_tracing();
    let token = read_token()?;
    drafter::run(&token)
}

fn init_tracing() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(max_level())
        .json()
        .finish();
    // set_global_default can only succeed once per process; ignore errors
    // so repeated calls (e.g., from tests) are harmless.
    let _ = tracing::subscriber::set_global_default(subscriber);
}

#[cfg(debug_assertions)]
const fn max_level() -> tracing::Level {
    tracing::Level::DEBUG
}

#[cfg(not(debug_assertions))]
const fn max_level() -> tracing::Level {
    tracing::Level::INFO
}

fn read_token() -> anyhow::Result<String> {
    let token = env::var("GH_TOKEN")
        .map_err(|_| anyhow::anyhow!("GH_TOKEN not set"))?;
    // Safety: binary `main` is single-threaded at this point, and tests
    // that touch this function serialize via `ENV_LOCK`.
    unsafe { env::remove_var("GH_TOKEN") };
    Ok(token)
}

#[cfg(test)]
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

    #[test]
    fn real_main_errors_without_token() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Safety: guarded by ENV_LOCK.
        unsafe { env::remove_var("GH_TOKEN") };
        assert!(real_main().is_err());
    }

    #[test]
    fn real_main_runs_with_stubbed_api() {
        // Points drafter at a one-shot HTTP server that returns an
        // empty search result, exercising the full real_main path
        // through the reqwest transport.
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The drafter test module owns a separate ENV_LOCK; both
        // guard `PRODDER_API_BASE`. Acquire that one too so a
        // concurrent drafter test doesn't tear our env var out from
        // under us.
        let _drafter_guard = crate::drafter::tests::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let body: &'static [u8] = Box::leak(
            crate::drafter::tests::ok_response(r#"{"items":[]}"#)
                .into_boxed_slice(),
        );
        let (url, handle) =
            crate::drafter::tests::spawn_one_shot_server(body);
        // Safety: guarded by ENV_LOCK.
        unsafe {
            env::set_var("GH_TOKEN", "x");
            env::set_var("PRODDER_API_BASE", &url);
        }
        let res = real_main();
        // Safety: guarded by ENV_LOCK.
        unsafe { env::remove_var("PRODDER_API_BASE") };
        assert!(res.is_ok(), "real_main failed: {res:?}");
        let _ = handle.join();
    }
}
