//! Strongly-typed configuration loaded from `./prodder.toml`.
//!
//! All fields are optional in the file — missing fields fall back to
//! the defaults defined by [`Default`]. When [`Users`] is the default
//! (unset), [`Users::resolve`] consults the GitHub REST `GET /user`
//! endpoint using the supplied token so PRodder targets the token's
//! own user without configuration.
//!
//! Field names in [`Behaviors`] mirror the GitHub API nomenclature so
//! operators can cross-reference the documentation linked inline in
//! `prodder.example.toml`.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

/// Path of the config file, relative to the current working directory.
pub const CONFIG_PATH: &str = "prodder.toml";

/// Top-level configuration.
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub users: Users,
    pub filter: Filter,
    pub behaviors: Behaviors,
}

/// Who to target.
///
/// * `List(vec![])` means "derive from the PAT" — the PAT owner's
///   login is resolved via `GET /user` at runtime.
/// * `List(vec!["*".into()])` means "all users visible to the token".
/// * Any other list is an explicit allow-list.
///
/// The [`Default`] for this type is `["dependabot[bot]"]` so that out
/// of the box PRodder watches dependabot's automated PRs — a common
/// case in any repository that has dependabot enabled. Operators who
/// want the historical "PAT owner only" behaviour set `users = []`
/// explicitly; operators who want dependabot *and* the PAT owner set
/// `users = ["dependabot[bot]", "<their-login>"]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Users(pub Vec<String>);

/// Login of the dependabot GitHub App as it appears on PR author
/// fields. Kept as a named constant so tests and documentation agree
/// on the exact string (GitHub brackets bot logins with `[bot]`).
pub const DEPENDABOT_LOGIN: &str = "dependabot[bot]";

impl Default for Users {
    fn default() -> Self {
        Self(vec![DEPENDABOT_LOGIN.to_string()])
    }
}

impl Users {
    /// Return true when the list is empty, i.e. the caller wants the
    /// PAT owner resolved from `GET /user`. This is **not** the same
    /// as [`Users::default`] — the default is
    /// `["dependabot[bot]"]`, an explicit non-empty list.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Return true when the wildcard `"*"` is present.
    pub fn is_wildcard(&self) -> bool {
        self.0.iter().any(|u| u == "*")
    }

    /// Resolve the concrete list of usernames.
    ///
    /// When the list is empty, this calls GitHub's `GET /user`
    /// endpoint with the provided PAT and returns a single-element
    /// vector with the resulting login.
    ///
    /// When the list contains `"*"` it is returned unchanged — the
    /// caller is expected to translate the wildcard into a
    /// GitHub-search query (e.g. by omitting the `author:` clause).
    pub fn resolve(&self, pat: &str) -> anyhow::Result<Vec<String>> {
        if !self.is_empty() {
            return Ok(self.0.clone());
        }
        let login = fetch_authenticated_login(pat)
            .context("resolving users from PAT via GET /user")?;
        Ok(vec![login])
    }
}

/// Filter expression passed to the GitHub issue search API.
///
/// Accepts the same syntax as the search bar at
/// <https://github.com/pulls>. An empty string means "use the default
/// filter derived from the resolved users".
#[derive(
    Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Filter(pub String);

impl Filter {
    /// Render the filter used for the `/search/issues` call. When
    /// unset, produce `is:pr is:open author:<u1> author:<u2> ...`.
    pub fn render(&self, users: &[String]) -> String {
        if !self.0.trim().is_empty() {
            return self.0.clone();
        }
        let mut out = String::from("is:pr is:open");
        for u in users {
            if u == "*" {
                continue;
            }
            out.push_str(" author:");
            out.push_str(u);
        }
        out
    }
}

/// Toggleable drafter behaviors.
///
/// Each field is named after the corresponding GitHub API concept and
/// defaults to `true`. See `prodder.example.toml` for the GitHub API
/// documentation URL that backs each toggle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Behaviors {
    /// Demote non-draft PRs whose `check_runs` or combined
    /// `required_status_checks` report failure.
    pub required_status_checks: bool,
    /// Demote PRs whose `mergeable_state` reports a conflict
    /// (`mergeable == false`).
    pub mergeable_state: bool,
    /// When a PR is `behind` base and otherwise clean, call
    /// `PUT /repos/{o}/{r}/pulls/{n}/update-branch` so CI re-runs.
    pub update_branch: bool,
}

impl Default for Behaviors {
    fn default() -> Self {
        Self {
            required_status_checks: true,
            mergeable_state: true,
            update_branch: true,
        }
    }
}

impl Config {
    /// Load the config from [`CONFIG_PATH`] in the current working
    /// directory. When the file does not exist, return [`Default`].
    pub fn load() -> anyhow::Result<Self> {
        Self::load_from(Path::new(CONFIG_PATH))
    }

    /// Load from an explicit path. Missing file → defaults.
    pub fn load_from(path: &Path) -> anyhow::Result<Self> {
        let raw = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("reading {}", path.display())
                });
            }
        };
        toml::from_str(&raw)
            .with_context(|| format!("parsing {}", path.display()))
    }

    /// Absolute path where [`Self::load`] would look.
    pub fn default_path() -> PathBuf {
        PathBuf::from(CONFIG_PATH)
    }
}

/// Call `GET https://api.github.com/user` and return the `login`
/// field. Isolated for easy mocking — the drafter already uses
/// `curl` with the token piped in via config, so we reuse the same
/// approach to avoid a second HTTP client.
fn fetch_authenticated_login(pat: &str) -> anyhow::Result<String> {
    let body =
        crate::drafter::curl_get_user(pat).context("GET /user")?;
    let v: serde_json::Value =
        serde_json::from_slice(&body).context("parse /user")?;
    let login = v
        .get("login")
        .and_then(|v| v.as_str())
        .context("/user response missing `login`")?;
    Ok(login.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_roundtrip() {
        let cfg = Config::default();
        let s = toml::to_string(&cfg).unwrap();
        let back: Config = toml::from_str(&s).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn missing_file_is_default() {
        let tmp =
            std::env::temp_dir().join("prodder-missing-config.toml");
        let _ = std::fs::remove_file(&tmp);
        let cfg = Config::load_from(&tmp).unwrap();
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn users_wildcard_distinct_from_explicit() {
        let star = Users(vec!["*".into()]);
        let explicit = Users(vec!["octocat".into()]);
        let empty = Users(vec![]);
        assert!(star.is_wildcard());
        assert!(!explicit.is_wildcard());
        assert!(empty.is_empty());
        assert!(!star.is_empty());
        assert_ne!(star, explicit);
    }

    #[test]
    fn default_users_includes_dependabot() {
        // Issue #23: dependabot should be watched out of the box.
        let users = Users::default();
        assert!(
            users.0.iter().any(|u| u == DEPENDABOT_LOGIN),
            "Users::default() should include {DEPENDABOT_LOGIN}; \
             got {:?}",
            users.0,
        );
        // And the default is *not* the "empty → derive from PAT"
        // sentinel — that is a distinct, explicit operator choice.
        assert!(!users.is_empty());
    }

    #[test]
    fn default_users_renders_filter_with_dependabot_author() {
        // End-to-end: the default config should surface dependabot
        // as an `author:` clause in the search query.
        let cfg = Config::default();
        let rendered = cfg.filter.render(&cfg.users.0);
        assert!(
            rendered.contains(&format!("author:{DEPENDABOT_LOGIN}")),
            "default filter should target dependabot; got {rendered:?}",
        );
    }

    #[test]
    fn filter_default_uses_resolved_users() {
        let f = Filter::default();
        let users = vec!["alice".to_string(), "bob".to_string()];
        assert_eq!(
            f.render(&users),
            "is:pr is:open author:alice author:bob"
        );
    }

    #[test]
    fn filter_wildcard_user_is_skipped_in_default_render() {
        let f = Filter::default();
        let users = vec!["*".to_string()];
        assert_eq!(f.render(&users), "is:pr is:open");
    }

    #[test]
    fn filter_explicit_is_passed_through() {
        let f = Filter("is:pr is:open repo:example/x".to_string());
        assert_eq!(f.render(&[]), "is:pr is:open repo:example/x");
    }

    #[test]
    fn behavior_toggles_each_flip() {
        let mut b = Behaviors::default();
        assert!(b.required_status_checks);
        assert!(b.mergeable_state);
        assert!(b.update_branch);
        b.required_status_checks = false;
        assert_eq!(
            b,
            Behaviors {
                required_status_checks: false,
                mergeable_state: true,
                update_branch: true,
            }
        );
        b = Behaviors::default();
        b.mergeable_state = false;
        assert_eq!(
            b,
            Behaviors {
                required_status_checks: true,
                mergeable_state: false,
                update_branch: true,
            }
        );
        b = Behaviors::default();
        b.update_branch = false;
        assert_eq!(
            b,
            Behaviors {
                required_status_checks: true,
                mergeable_state: true,
                update_branch: false,
            }
        );
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let parsed: Config =
            toml::from_str("users = [\"octocat\"]\n").unwrap();
        assert_eq!(parsed.users.0, vec!["octocat".to_string()]);
        assert_eq!(parsed.filter, Filter::default());
        assert_eq!(parsed.behaviors, Behaviors::default());
    }

    #[test]
    fn unknown_field_rejected() {
        let err: Result<Config, _> =
            toml::from_str("nonsense = true\n");
        assert!(err.is_err());
    }
}
