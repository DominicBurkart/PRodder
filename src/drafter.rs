//! Finds all open PRs (including drafts) authored by `DominicBurkart`
//! in `DominicBurkart/*` repos, checks non-review merge requirements
//! (merge conflicts, combined status, check-runs), and either:
//!   * converts any non-draft PR with a blocking non-review requirement
//!     back to draft state, or
//!   * for PRs whose branch is behind the base
//!     (`mergeable_state == "behind"`), asks GitHub to update the branch
//!     so it stays current.
//!
//! Review-related gating is deliberately ignored — humans handle reviews.
//!
//! Implementation note: to keep the supply-chain surface small, this
//! module has no HTTP client dependency. It shells out to `curl` and
//! parses the responses with `serde_json::Value`. The GitHub API token
//! is piped into `curl` via a config file on stdin — it is never passed
//! on the command line, so it never appears in `/proc/<pid>/cmdline`.

use std::io::Write;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use serde_json::Value;
use tracing::{info, warn};

const SEARCH_QUERY: &str =
    "is:open is:pr author:DominicBurkart archived:false";
const OWNER_PREFIX: &str = "DominicBurkart/";
const API: &str = "https://api.github.com";
const USER_AGENT: &str = "PRodder";

const CONVERT_TO_DRAFT_MUTATION: &str = "mutation ConvertToDraft($id: ID!) { \
    convertPullRequestToDraft(input: { pullRequestId: $id }) { \
        pullRequest { id isDraft } } }";

#[derive(Debug, Clone)]
struct CandidatePr {
    owner: String,
    repo: String,
    number: u64,
    node_id: String,
    head_sha: String,
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum BlockDecision {
    Block(String),
    Ok,
    Unknown,
}

#[derive(Debug, PartialEq, Eq, Clone)]
enum Action {
    /// No action: PR is either not mergeable-ready or already mergeable.
    Nothing,
    /// Mergeability is not yet known; try again on the next drafter cycle.
    Retry,
    /// PR has a blocking non-review requirement; convert back to draft.
    Draft(String),
    /// PR branch is behind base — ask GitHub to update it.
    UpdateBranch,
}

/// Pure action selector — combines `classify()` with the out-of-date signal.
/// Unit-tested without any network or subprocess.
fn decide(
    mergeable: Option<bool>,
    mergeable_state: &str,
    combined_state: &str,
    checks: &[CheckRun],
) -> Action {
    match classify(mergeable, combined_state, checks) {
        BlockDecision::Block(r) => Action::Draft(r),
        BlockDecision::Unknown => Action::Retry,
        BlockDecision::Ok => {
            if mergeable_state == "behind" {
                Action::UpdateBranch
            } else {
                Action::Nothing
            }
        }
    }
}

#[derive(Debug, Clone)]
struct CheckRun {
    name: String,
    status: String,
    conclusion: Option<String>,
}

/// Pure classifier — unit-tested without any network or subprocess.
fn classify(
    mergeable: Option<bool>,
    combined_state: &str,
    checks: &[CheckRun],
) -> BlockDecision {
    if mergeable == Some(false) {
        return BlockDecision::Block("merge conflicts".into());
    }
    if combined_state == "failure" || combined_state == "error" {
        return BlockDecision::Block(format!(
            "combined status {combined_state}"
        ));
    }
    for run in checks {
        if run.status != "completed" {
            continue;
        }
        if let Some(
            "failure" | "timed_out" | "cancelled" | "action_required"
            | "stale",
        ) = run.conclusion.as_deref()
        {
            return BlockDecision::Block(format!(
                "check '{}' {}",
                run.name,
                run.conclusion.as_deref().unwrap_or("?")
            ));
        }
    }
    if mergeable.is_none() {
        return BlockDecision::Unknown;
    }
    BlockDecision::Ok
}

#[tracing::instrument(skip(token))]
pub fn run(token: &str) -> anyhow::Result<()> {
    let candidates = match list_candidate_prs(token) {
        Ok(v) => v,
        Err(e) => {
            warn!("drafter: failed to list candidate PRs: {e:#}");
            return Ok(());
        }
    };
    info!(
        count = candidates.len(),
        "drafter: candidate PRs collected"
    );

    for c in candidates {
        let action = match evaluate(token, &c) {
            Ok(a) => a,
            Err(e) => {
                warn!(
                    owner = %c.owner, repo = %c.repo, number = c.number,
                    "drafter: failed to evaluate PR: {e:#}"
                );
                continue;
            }
        };
        match action {
            Action::Draft(reason) => {
                info!(
                    owner = %c.owner, repo = %c.repo, number = c.number,
                    %reason,
                    "drafter: converting PR to draft"
                );
                if let Err(e) = convert_to_draft(token, &c.node_id) {
                    warn!(
                        owner = %c.owner, repo = %c.repo,
                        number = c.number,
                        "drafter: convert_to_draft failed: {e:#}"
                    );
                }
            }
            Action::UpdateBranch => {
                info!(
                    owner = %c.owner, repo = %c.repo, number = c.number,
                    "drafter: branch is behind base; pushing update"
                );
                if let Err(e) = update_branch(
                    token,
                    &c.owner,
                    &c.repo,
                    c.number,
                    &c.head_sha,
                ) {
                    warn!(
                        owner = %c.owner, repo = %c.repo,
                        number = c.number,
                        "drafter: update_branch failed: {e:#}"
                    );
                }
            }
            Action::Retry => {
                info!(
                    owner = %c.owner, repo = %c.repo, number = c.number,
                    "drafter: mergeability unknown, will retry next cycle"
                );
            }
            Action::Nothing => {
                info!(
                    owner = %c.owner, repo = %c.repo, number = c.number,
                    "drafter: PR has no blocking non-review requirements"
                );
            }
        }
    }
    Ok(())
}

/// Owner/repo/number triple resolved from a search-API issue payload.
#[derive(Debug, PartialEq, Eq, Clone)]
struct IssueRef {
    owner: String,
    repo: String,
    number: u64,
}

/// Parse a search-API issue payload into an [`IssueRef`], applying the
/// [`OWNER_PREFIX`] filter. Returns `None` when the payload cannot be
/// identified as a PR in a `DominicBurkart/*` repo.
fn parse_issue_ref(issue: &Value) -> Option<IssueRef> {
    let repo_url = issue
        .get("repository_url")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let (owner, repo) = repo_from_url(repo_url)?;
    if !format!("{owner}/{repo}").starts_with(OWNER_PREFIX) {
        return None;
    }
    let number =
        issue.get("number").and_then(serde_json::Value::as_u64)?;
    Some(IssueRef {
        owner,
        repo,
        number,
    })
}

/// Build a [`CandidatePr`] from a PR JSON payload. Returns `None` when
/// the payload lacks the required `node_id` or `head.sha` fields.
fn parse_candidate_pr(
    issue: &IssueRef,
    pr: &Value,
) -> Option<CandidatePr> {
    let node_id =
        pr.get("node_id").and_then(|v| v.as_str())?.to_string();
    let head_sha = pr
        .pointer("/head/sha")
        .and_then(|v| v.as_str())?
        .to_string();
    Some(CandidatePr {
        owner: issue.owner.clone(),
        repo: issue.repo.clone(),
        number: issue.number,
        node_id,
        head_sha,
    })
}

fn list_candidate_prs(
    token: &str,
) -> anyhow::Result<Vec<CandidatePr>> {
    let url = format!(
        "{API}/search/issues?q={}&per_page=100",
        percent_encode(SEARCH_QUERY)
    );
    let body =
        curl(token, "GET", &url, None).context("search issues")?;
    let v: Value = serde_json::from_slice(&body)
        .context("parse search response")?;
    let items = v
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::new();
    for issue in items {
        let Some(issue_ref) = parse_issue_ref(&issue) else {
            continue;
        };

        let pr_url = format!(
            "{API}/repos/{}/{}/pulls/{}",
            issue_ref.owner, issue_ref.repo, issue_ref.number
        );
        let pr_body = match curl(token, "GET", &pr_url, None) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    owner = %issue_ref.owner,
                    repo = %issue_ref.repo,
                    number = issue_ref.number,
                    "drafter: pulls.get failed: {e:#}"
                );
                continue;
            }
        };
        let pr: Value = match serde_json::from_slice(&pr_body) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    owner = %issue_ref.owner,
                    repo = %issue_ref.repo,
                    number = issue_ref.number,
                    "drafter: pr json parse failed: {e:#}"
                );
                continue;
            }
        };
        if let Some(candidate) = parse_candidate_pr(&issue_ref, &pr) {
            out.push(candidate);
        } else {
            warn!(
                owner = %issue_ref.owner,
                repo = %issue_ref.repo,
                number = issue_ref.number,
                "drafter: PR missing node_id or head.sha; skipping"
            );
        }
    }
    Ok(out)
}

/// Extract the `(mergeable, mergeable_state)` pair from a PR payload.
fn parse_mergeability(pr: &Value) -> (Option<bool>, String) {
    let mergeable =
        pr.get("mergeable").and_then(serde_json::Value::as_bool);
    let mergeable_state = pr
        .get("mergeable_state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (mergeable, mergeable_state)
}

/// Extract the `state` field from a combined-status payload.
fn parse_combined_state(status: &Value) -> String {
    status
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// Extract check-run entries from a `/check-runs` payload.
fn parse_check_runs(checks_v: &Value) -> Vec<CheckRun> {
    let Some(arr) =
        checks_v.get("check_runs").and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    arr.iter()
        .map(|run| CheckRun {
            name: run
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("?")
                .to_string(),
            status: run
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            conclusion: run
                .get("conclusion")
                .and_then(|v| v.as_str())
                .map(String::from),
        })
        .collect()
}

fn evaluate(token: &str, c: &CandidatePr) -> anyhow::Result<Action> {
    let pr_url = format!(
        "{API}/repos/{}/{}/pulls/{}",
        c.owner, c.repo, c.number
    );
    let pr_body =
        curl(token, "GET", &pr_url, None).context("pulls.get")?;
    let pr: Value =
        serde_json::from_slice(&pr_body).context("parse pr")?;
    let (mergeable, mergeable_state) = parse_mergeability(&pr);

    let status_url = format!(
        "{API}/repos/{}/{}/commits/{}/status",
        c.owner, c.repo, c.head_sha
    );
    let status_body = curl(token, "GET", &status_url, None)
        .context("combined status")?;
    let status: Value = serde_json::from_slice(&status_body)
        .context("parse status")?;
    let combined_state = parse_combined_state(&status);

    let checks_url = format!(
        "{API}/repos/{}/{}/commits/{}/check-runs",
        c.owner, c.repo, c.head_sha
    );
    let checks_body = curl(token, "GET", &checks_url, None)
        .context("check-runs")?;
    let checks_v: Value = serde_json::from_slice(&checks_body)
        .context("parse check-runs")?;
    let checks = parse_check_runs(&checks_v);

    Ok(decide(
        mergeable,
        &mergeable_state,
        &combined_state,
        &checks,
    ))
}

fn convert_to_draft(
    token: &str,
    node_id: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "query": CONVERT_TO_DRAFT_MUTATION,
        "variables": { "id": node_id },
    });
    let body_bytes = serde_json::to_vec(&body)?;
    let resp_bytes = curl(
        token,
        "POST",
        &format!("{API}/graphql"),
        Some(&body_bytes),
    )
    .context("graphql convertPullRequestToDraft")?;
    let resp: Value = serde_json::from_slice(&resp_bytes)
        .context("parse graphql response")?;
    if graphql_has_errors(&resp) {
        bail!("graphql errors: {}", &resp["errors"]);
    }
    info!(node_id, "drafter: PR converted to draft");
    Ok(())
}

/// Return `true` when a GraphQL response carries a non-empty `errors`
/// array. A missing `errors` key or an empty array both mean success.
fn graphql_has_errors(resp: &Value) -> bool {
    let Some(errors) = resp.get("errors") else {
        return false;
    };
    !errors.as_array().is_some_and(std::vec::Vec::is_empty)
}

/// Ask GitHub to merge the base branch into the PR's head branch.
///
/// `expected_head_sha` is passed so the request is rejected (422) if
/// the head moved between our evaluation and this call, preventing a
/// race with a concurrent push.
fn update_branch(
    token: &str,
    owner: &str,
    repo: &str,
    number: u64,
    expected_head_sha: &str,
) -> anyhow::Result<()> {
    let body =
        serde_json::json!({ "expected_head_sha": expected_head_sha });
    let body_bytes = serde_json::to_vec(&body)?;
    let url = format!(
        "{API}/repos/{owner}/{repo}/pulls/{number}/update-branch"
    );
    curl(token, "PUT", &url, Some(&body_bytes))
        .context("pulls.update-branch")?;
    info!(owner, repo, number, "drafter: update-branch requested");
    Ok(())
}

/// Invoke `curl` against the GitHub API.
///
/// The bearer token is piped in via a curl config file on stdin so it
/// never appears in argv / `/proc/<pid>/cmdline`. Request bodies are
/// likewise placed in the config as `data-binary = "..."` with
/// backslash escapes for `\` and `"`.
fn curl(
    token: &str,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let mut config = String::new();
    config.push_str(&format!(
        "header = \"Authorization: Bearer {}\"\n",
        cfg_escape(token)
    ));
    config.push_str(
        "header = \"Accept: application/vnd.github+json\"\n",
    );
    config
        .push_str("header = \"X-GitHub-Api-Version: 2022-11-28\"\n");
    config.push_str(&format!("user-agent = \"{USER_AGENT}\"\n"));
    config
        .push_str(&format!("request = \"{}\"\n", cfg_escape(method)));
    config.push_str(&format!("url = \"{}\"\n", cfg_escape(url)));
    if let Some(b) = body {
        let body_str = std::str::from_utf8(b)
            .context("non-utf8 request body")?;
        config.push_str(
            "header = \"Content-Type: application/json\"\n",
        );
        config.push_str(&format!(
            "data-binary = \"{}\"\n",
            cfg_escape(body_str)
        ));
    }

    let mut child = Command::new("curl")
        .args([
            "--silent",
            "--show-error",
            "--fail-with-body",
            "--location",
            "--config",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning curl")?;
    child
        .stdin
        .as_mut()
        .expect("stdin piped")
        .write_all(config.as_bytes())
        .context("writing curl config")?;
    let out = child.wait_with_output().context("waiting for curl")?;
    if !out.status.success() {
        bail!(
            "curl failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(out.stdout)
}

/// Escape a string for use inside a `"..."` value in a curl config
/// file. Only `\` and `"` need escaping.
fn cfg_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

/// Percent-encode a query-parameter value using the unreserved set
/// from RFC 3986.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Extract ("owner", "repo") from a GitHub REST repository URL such
/// as `https://api.github.com/repos/OWNER/REPO`.
fn repo_from_url(url: &str) -> Option<(String, String)> {
    let tail = url.rsplit("/repos/").next()?;
    let mut parts = tail.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.trim_end_matches('/').to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(
        name: &str,
        status: &str,
        conclusion: Option<&str>,
    ) -> CheckRun {
        CheckRun {
            name: name.into(),
            status: status.into(),
            conclusion: conclusion.map(std::convert::Into::into),
        }
    }

    #[test]
    fn conflict_blocks() {
        assert!(matches!(
            classify(Some(false), "success", &[]),
            BlockDecision::Block(_)
        ));
    }

    #[test]
    fn failing_check_blocks() {
        let checks = vec![check("ci", "completed", Some("failure"))];
        assert!(matches!(
            classify(Some(true), "success", &checks),
            BlockDecision::Block(_)
        ));
    }

    #[test]
    fn failing_combined_status_blocks() {
        assert!(matches!(
            classify(Some(true), "failure", &[]),
            BlockDecision::Block(_)
        ));
    }

    #[test]
    fn all_green_is_ok() {
        let checks = vec![
            check("ci", "completed", Some("success")),
            check("lint", "completed", Some("neutral")),
        ];
        assert_eq!(
            classify(Some(true), "success", &checks),
            BlockDecision::Ok
        );
    }

    #[test]
    fn in_progress_check_is_not_blocking() {
        let checks = vec![check("ci", "in_progress", None)];
        assert_eq!(
            classify(Some(true), "pending", &checks),
            BlockDecision::Ok
        );
    }

    #[test]
    fn unknown_mergeable_is_unknown() {
        assert_eq!(
            classify(None, "success", &[]),
            BlockDecision::Unknown
        );
    }

    #[test]
    fn decide_behind_updates_branch() {
        let checks = vec![check("ci", "completed", Some("success"))];
        assert_eq!(
            decide(Some(true), "behind", "success", &checks),
            Action::UpdateBranch
        );
    }

    #[test]
    fn decide_behind_on_draft_updates_branch() {
        assert_eq!(
            decide(Some(true), "behind", "success", &[]),
            Action::UpdateBranch
        );
    }

    #[test]
    fn decide_clean_does_nothing() {
        assert_eq!(
            decide(Some(true), "clean", "success", &[]),
            Action::Nothing
        );
    }

    #[test]
    fn decide_failing_check_drafts_even_if_behind() {
        let checks = vec![check("ci", "completed", Some("failure"))];
        assert!(matches!(
            decide(Some(true), "behind", "success", &checks),
            Action::Draft(_)
        ));
    }

    #[test]
    fn decide_unknown_mergeable_retries() {
        assert_eq!(
            decide(None, "unknown", "success", &[]),
            Action::Retry
        );
    }

    #[test]
    fn decide_blocked_state_does_nothing() {
        assert_eq!(
            decide(Some(true), "blocked", "success", &[]),
            Action::Nothing
        );
    }

    #[test]
    fn timed_out_check_blocks() {
        let checks =
            vec![check("ci", "completed", Some("timed_out"))];
        assert!(matches!(
            classify(Some(true), "success", &checks),
            BlockDecision::Block(_)
        ));
    }

    #[test]
    fn repo_from_url_parses_api_style() {
        assert_eq!(
            repo_from_url(
                "https://api.github.com/repos/DominicBurkart/committer"
            ),
            Some(("DominicBurkart".into(), "committer".into()))
        );
    }

    #[test]
    fn repo_from_url_rejects_junk() {
        assert_eq!(repo_from_url("not a url"), None);
    }

    #[test]
    fn percent_encode_search_query() {
        assert_eq!(
            percent_encode("is:open is:pr author:DominicBurkart"),
            "is%3Aopen%20is%3Apr%20author%3ADominicBurkart"
        );
    }

    #[test]
    fn cfg_escape_quotes_and_backslashes() {
        assert_eq!(cfg_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn cfg_escape_plain_string_unchanged() {
        assert_eq!(
            cfg_escape("Bearer ghp_abc123"),
            "Bearer ghp_abc123"
        );
    }

    fn issue_ref(owner: &str, repo: &str, number: u64) -> IssueRef {
        IssueRef {
            owner: owner.into(),
            repo: repo.into(),
            number,
        }
    }

    #[test]
    fn parse_issue_ref_accepts_owner_prefix() {
        let issue = serde_json::json!({
            "repository_url": "https://api.github.com/repos/DominicBurkart/committer",
            "number": 42,
        });
        assert_eq!(
            parse_issue_ref(&issue),
            Some(issue_ref("DominicBurkart", "committer", 42))
        );
    }

    #[test]
    fn parse_issue_ref_rejects_other_owner() {
        let issue = serde_json::json!({
            "repository_url": "https://api.github.com/repos/other/repo",
            "number": 1,
        });
        assert_eq!(parse_issue_ref(&issue), None);
    }

    #[test]
    fn parse_issue_ref_rejects_missing_repo_url() {
        let issue = serde_json::json!({ "number": 1 });
        assert_eq!(parse_issue_ref(&issue), None);
    }

    #[test]
    fn parse_issue_ref_rejects_missing_number() {
        let issue = serde_json::json!({
            "repository_url": "https://api.github.com/repos/DominicBurkart/committer",
        });
        assert_eq!(parse_issue_ref(&issue), None);
    }

    #[test]
    fn parse_candidate_pr_extracts_node_id_and_sha() {
        let issue = issue_ref("DominicBurkart", "committer", 7);
        let pr = serde_json::json!({
            "node_id": "PR_kwDO",
            "head": { "sha": "deadbeef" },
        });
        let candidate =
            parse_candidate_pr(&issue, &pr).expect("candidate");
        assert_eq!(candidate.owner, "DominicBurkart");
        assert_eq!(candidate.repo, "committer");
        assert_eq!(candidate.number, 7);
        assert_eq!(candidate.node_id, "PR_kwDO");
        assert_eq!(candidate.head_sha, "deadbeef");
    }

    #[test]
    fn parse_candidate_pr_missing_node_id_is_none() {
        let issue = issue_ref("DominicBurkart", "r", 1);
        let pr = serde_json::json!({ "head": { "sha": "abc" } });
        assert!(parse_candidate_pr(&issue, &pr).is_none());
    }

    #[test]
    fn parse_candidate_pr_missing_head_sha_is_none() {
        let issue = issue_ref("DominicBurkart", "r", 1);
        let pr = serde_json::json!({ "node_id": "PR_x" });
        assert!(parse_candidate_pr(&issue, &pr).is_none());
    }

    #[test]
    fn parse_mergeability_reads_fields() {
        let pr = serde_json::json!({
            "mergeable": true,
            "mergeable_state": "clean",
        });
        let (m, state) = parse_mergeability(&pr);
        assert_eq!(m, Some(true));
        assert_eq!(state, "clean");
    }

    #[test]
    fn parse_mergeability_handles_missing_fields() {
        let pr = serde_json::json!({});
        let (m, state) = parse_mergeability(&pr);
        assert_eq!(m, None);
        assert_eq!(state, "");
    }

    #[test]
    fn parse_mergeability_false_and_behind() {
        let pr = serde_json::json!({
            "mergeable": false,
            "mergeable_state": "behind",
        });
        let (m, state) = parse_mergeability(&pr);
        assert_eq!(m, Some(false));
        assert_eq!(state, "behind");
    }

    #[test]
    fn parse_combined_state_extracts_state() {
        let status = serde_json::json!({ "state": "success" });
        assert_eq!(parse_combined_state(&status), "success");
    }

    #[test]
    fn parse_combined_state_missing_is_empty() {
        assert_eq!(parse_combined_state(&serde_json::json!({})), "");
    }

    #[test]
    fn parse_check_runs_extracts_all_fields() {
        let body = serde_json::json!({
            "check_runs": [
                {
                    "name": "lint",
                    "status": "completed",
                    "conclusion": "success",
                },
                {
                    "name": "test",
                    "status": "in_progress",
                    "conclusion": null,
                },
            ]
        });
        let runs = parse_check_runs(&body);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].name, "lint");
        assert_eq!(runs[0].status, "completed");
        assert_eq!(runs[0].conclusion.as_deref(), Some("success"));
        assert_eq!(runs[1].name, "test");
        assert_eq!(runs[1].status, "in_progress");
        assert_eq!(runs[1].conclusion, None);
    }

    #[test]
    fn parse_check_runs_missing_array_is_empty() {
        assert!(parse_check_runs(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn parse_check_runs_defaults_missing_fields() {
        let body = serde_json::json!({
            "check_runs": [ {} ],
        });
        let runs = parse_check_runs(&body);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].name, "?");
        assert_eq!(runs[0].status, "");
        assert_eq!(runs[0].conclusion, None);
    }

    #[test]
    fn graphql_has_errors_missing_key_is_false() {
        assert!(!graphql_has_errors(&serde_json::json!({
            "data": { "convertPullRequestToDraft": {} }
        })));
    }

    #[test]
    fn graphql_has_errors_empty_array_is_false() {
        assert!(!graphql_has_errors(
            &serde_json::json!({ "errors": [] })
        ));
    }

    #[test]
    fn graphql_has_errors_non_empty_is_true() {
        assert!(graphql_has_errors(&serde_json::json!({
            "errors": [ { "message": "nope" } ]
        })));
    }

    #[test]
    fn graphql_has_errors_non_array_is_true() {
        // A non-array `errors` value is still considered an error;
        // `is_some_and(Vec::is_empty)` returns false when the value
        // isn't an array.
        assert!(graphql_has_errors(&serde_json::json!({
            "errors": "something went wrong"
        })));
    }
}
