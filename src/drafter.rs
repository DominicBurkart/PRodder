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
//! Implementation note: HTTP requests are made via `reqwest`'s blocking
//! client. The GitHub API token is sent as a bearer token in the
//! `Authorization` header, so it never appears on a command line and
//! never reaches `/proc/<pid>/cmdline`.

use anyhow::{Context, bail};
use serde_json::Value;
use tracing::{info, warn};

const SEARCH_QUERY: &str =
    "is:open is:pr author:DominicBurkart archived:false";
const OWNER_PREFIX: &str = "DominicBurkart/";
const DEFAULT_API: &str = "https://api.github.com";
const USER_AGENT: &str = "PRodder";

/// Resolve the GitHub API base URL, allowing tests to point at a local
/// server via `PRODDER_API_BASE`.
fn api_base() -> String {
    std::env::var("PRODDER_API_BASE")
        .unwrap_or_else(|_| DEFAULT_API.to_string())
}

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

/// HTTP transport abstraction. The production impl uses `reqwest`'s
/// blocking client; tests swap in a mock to exercise the
/// request/response logic without touching the network.
pub(crate) trait Transport {
    /// Perform an HTTP request and return the raw response body.
    ///
    /// # Errors
    /// Implementations return an error if the request fails to send or
    /// the server responds with a non-success status.
    fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&[u8]>,
    ) -> anyhow::Result<Vec<u8>>;
}

struct ReqwestTransport<'a> {
    token: &'a str,
    client: reqwest::blocking::Client,
}

impl<'a> ReqwestTransport<'a> {
    fn new(token: &'a str) -> Self {
        Self {
            token,
            client: reqwest::blocking::Client::new(),
        }
    }
}

impl Transport for ReqwestTransport<'_> {
    fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<&[u8]>,
    ) -> anyhow::Result<Vec<u8>> {
        send(&self.client, self.token, method, url, body)
    }
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

/// Run the drafter against the GitHub API using the supplied token.
///
/// # Errors
/// Returns an error only for unexpected internal failures. Network,
/// parse, and per-PR errors are logged and swallowed so the cycle as a
/// whole makes best-effort progress.
pub fn run(token: &str) -> anyhow::Result<()> {
    run_with(&ReqwestTransport::new(token));
    Ok(())
}

fn run_with(t: &dyn Transport) {
    let candidates = match list_candidate_prs(t) {
        Ok(v) => v,
        Err(e) => {
            warn!("drafter: failed to list candidate PRs: {e:#}");
            return;
        }
    };
    info!(
        count = candidates.len(),
        "drafter: candidate PRs collected"
    );

    for c in candidates {
        let action = match evaluate(t, &c) {
            Ok(a) => a,
            Err(e) => {
                warn!(owner = %c.owner, repo = %c.repo, number = c.number, "drafter: failed to evaluate PR: {e:#}");
                continue;
            }
        };
        match action {
            Action::Draft(reason) => {
                info!(owner = %c.owner, repo = %c.repo, number = c.number, %reason, "drafter: converting PR to draft");
                if let Err(e) = convert_to_draft(t, &c.node_id) {
                    warn!(owner = %c.owner, repo = %c.repo, number = c.number, "drafter: convert_to_draft failed: {e:#}");
                }
            }
            Action::UpdateBranch => {
                info!(owner = %c.owner, repo = %c.repo, number = c.number, "drafter: branch is behind base; pushing update");
                if let Err(e) = update_branch(
                    t,
                    &c.owner,
                    &c.repo,
                    c.number,
                    &c.head_sha,
                ) {
                    warn!(owner = %c.owner, repo = %c.repo, number = c.number, "drafter: update_branch failed: {e:#}");
                }
            }
            Action::Retry => {
                info!(owner = %c.owner, repo = %c.repo, number = c.number, "drafter: mergeability unknown, will retry next cycle");
            }
            Action::Nothing => {
                info!(owner = %c.owner, repo = %c.repo, number = c.number, "drafter: PR has no blocking non-review requirements");
            }
        }
    }
}

fn list_candidate_prs(
    t: &dyn Transport,
) -> anyhow::Result<Vec<CandidatePr>> {
    let url = format!(
        "{}/search/issues?q={}&per_page=100",
        api_base(),
        percent_encode(SEARCH_QUERY)
    );
    let body =
        t.request("GET", &url, None).context("search issues")?;
    let v: Value = serde_json::from_slice(&body)
        .context("parse search response")?;
    let items = v
        .get("items")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut out = Vec::new();
    for issue in items {
        let repo_url = issue
            .get("repository_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some((owner, repo)) = repo_from_url(repo_url) else {
            continue;
        };
        if !format!("{owner}/{repo}").starts_with(OWNER_PREFIX) {
            continue;
        }
        let Some(number) =
            issue.get("number").and_then(serde_json::Value::as_u64)
        else {
            continue;
        };

        let pr_url = format!(
            "{}/repos/{owner}/{repo}/pulls/{number}",
            api_base()
        );
        let pr_body = match t.request("GET", &pr_url, None) {
            Ok(b) => b,
            Err(e) => {
                warn!(
                    owner,
                    repo, number, "drafter: pulls.get failed: {e:#}"
                );
                continue;
            }
        };
        let pr: Value = match serde_json::from_slice(&pr_body) {
            Ok(v) => v,
            Err(e) => {
                warn!(
                    owner,
                    repo,
                    number,
                    "drafter: pr json parse failed: {e:#}"
                );
                continue;
            }
        };
        let node_id = if let Some(s) =
            pr.get("node_id").and_then(|v| v.as_str())
        {
            s.to_string()
        } else {
            warn!(
                owner,
                repo, number, "drafter: PR missing node_id; skipping"
            );
            continue;
        };
        let head_sha = if let Some(s) =
            pr.pointer("/head/sha").and_then(|v| v.as_str())
        {
            s.to_string()
        } else {
            warn!(
                owner,
                repo,
                number,
                "drafter: PR missing head.sha; skipping"
            );
            continue;
        };
        out.push(CandidatePr {
            owner,
            repo,
            number,
            node_id,
            head_sha,
        });
    }
    Ok(out)
}

fn evaluate(
    t: &dyn Transport,
    c: &CandidatePr,
) -> anyhow::Result<Action> {
    let pr_url = format!(
        "{}/repos/{}/{}/pulls/{}",
        api_base(),
        c.owner,
        c.repo,
        c.number
    );
    let pr_body =
        t.request("GET", &pr_url, None).context("pulls.get")?;
    let pr: Value =
        serde_json::from_slice(&pr_body).context("parse pr")?;
    let mergeable =
        pr.get("mergeable").and_then(serde_json::Value::as_bool);
    let mergeable_state = pr
        .get("mergeable_state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let status_url = format!(
        "{}/repos/{}/{}/commits/{}/status",
        api_base(),
        c.owner,
        c.repo,
        c.head_sha
    );
    let status_body = t
        .request("GET", &status_url, None)
        .context("combined status")?;
    let status: Value = serde_json::from_slice(&status_body)
        .context("parse status")?;
    let combined_state = status
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let checks_url = format!(
        "{}/repos/{}/{}/commits/{}/check-runs",
        api_base(),
        c.owner,
        c.repo,
        c.head_sha
    );
    let checks_body =
        t.request("GET", &checks_url, None).context("check-runs")?;
    let checks_v: Value = serde_json::from_slice(&checks_body)
        .context("parse check-runs")?;
    let mut checks = Vec::new();
    if let Some(arr) =
        checks_v.get("check_runs").and_then(|v| v.as_array())
    {
        for run in arr {
            checks.push(CheckRun {
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
            });
        }
    }

    Ok(decide(
        mergeable,
        &mergeable_state,
        &combined_state,
        &checks,
    ))
}

fn convert_to_draft(
    t: &dyn Transport,
    node_id: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "query": CONVERT_TO_DRAFT_MUTATION,
        "variables": { "id": node_id },
    });
    let body_bytes = serde_json::to_vec(&body)?;
    let resp_bytes = t
        .request(
            "POST",
            &format!("{}/graphql", api_base()),
            Some(&body_bytes),
        )
        .context("graphql convertPullRequestToDraft")?;
    let resp: Value = serde_json::from_slice(&resp_bytes)
        .context("parse graphql response")?;
    if let Some(errors) = resp.get("errors") {
        let is_empty =
            errors.as_array().is_some_and(std::vec::Vec::is_empty);
        if !is_empty {
            bail!("graphql errors: {errors}");
        }
    }
    info!(node_id, "drafter: PR converted to draft");
    Ok(())
}

/// Ask GitHub to merge the base branch into the PR's head branch.
///
/// `expected_head_sha` is passed so the request is rejected (422) if
/// the head moved between our evaluation and this call, preventing a
/// race with a concurrent push.
fn update_branch(
    t: &dyn Transport,
    owner: &str,
    repo: &str,
    number: u64,
    expected_head_sha: &str,
) -> anyhow::Result<()> {
    let body =
        serde_json::json!({ "expected_head_sha": expected_head_sha });
    let body_bytes = serde_json::to_vec(&body)?;
    let url = format!(
        "{}/repos/{owner}/{repo}/pulls/{number}/update-branch",
        api_base()
    );
    t.request("PUT", &url, Some(&body_bytes))
        .context("pulls.update-branch")?;
    info!(owner, repo, number, "drafter: update-branch requested");
    Ok(())
}

/// Send an HTTP request to the GitHub API via reqwest's blocking
/// client. The token is attached as a bearer credential, so it never
/// hits a command line.
fn send(
    client: &reqwest::blocking::Client,
    token: &str,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
) -> anyhow::Result<Vec<u8>> {
    let m = reqwest::Method::from_bytes(method.as_bytes())
        .context("invalid HTTP method")?;
    let mut req = client
        .request(m, url)
        .bearer_auth(token)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", USER_AGENT);
    if let Some(b) = body {
        req = req
            .header("Content-Type", "application/json")
            .body(b.to_vec());
    }
    let resp = req.send().context("sending request")?;
    let status = resp.status();
    let bytes = resp.bytes().context("reading response body")?;
    if !status.is_success() {
        bail!(
            "request failed ({}): {}",
            status,
            String::from_utf8_lossy(&bytes)
        );
    }
    Ok(bytes.to_vec())
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
pub(crate) mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

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

    /// `(method, url, body)` tuple recorded for each request.
    pub type RecordedCall = (String, String, Option<Vec<u8>>);

    /// In-memory transport: returns queued responses in FIFO order and
    /// records each call for later assertions.
    pub struct MockTransport {
        responses: RefCell<VecDeque<anyhow::Result<Vec<u8>>>>,
        calls: RefCell<Vec<RecordedCall>>,
    }

    impl MockTransport {
        pub const fn new() -> Self {
            Self {
                responses: RefCell::new(VecDeque::new()),
                calls: RefCell::new(Vec::new()),
            }
        }

        pub fn queue_ok(self, body: &[u8]) -> Self {
            self.responses.borrow_mut().push_back(Ok(body.to_vec()));
            self
        }

        pub fn queue_err(self, msg: &str) -> Self {
            self.responses
                .borrow_mut()
                .push_back(Err(anyhow::anyhow!(msg.to_string())));
            self
        }

        pub fn calls(&self) -> Vec<RecordedCall> {
            self.calls.borrow().clone()
        }
    }

    impl Transport for MockTransport {
        fn request(
            &self,
            method: &str,
            url: &str,
            body: Option<&[u8]>,
        ) -> anyhow::Result<Vec<u8>> {
            self.calls.borrow_mut().push((
                method.to_string(),
                url.to_string(),
                body.map(<[u8]>::to_vec),
            ));
            self.responses.borrow_mut().pop_front().expect(
                "MockTransport: no queued response for this call",
            )
        }
    }

    fn candidate() -> CandidatePr {
        CandidatePr {
            owner: "DominicBurkart".into(),
            repo: "committer".into(),
            number: 7,
            node_id: "PR_1".into(),
            head_sha: "deadbeef".into(),
        }
    }

    fn search_one_item() -> Vec<u8> {
        br#"{"items":[{
            "repository_url": "https://api.github.com/repos/DominicBurkart/committer",
            "number": 7
        }]}"#
            .to_vec()
    }

    fn pr_ok() -> Vec<u8> {
        br#"{"node_id":"PR_1","head":{"sha":"deadbeef"},"mergeable":true,"mergeable_state":"clean"}"#
            .to_vec()
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
    fn repo_from_url_rejects_empty_repo() {
        assert_eq!(
            repo_from_url("https://api.github.com/repos/owner/"),
            None
        );
    }

    #[test]
    fn percent_encode_search_query() {
        assert_eq!(
            percent_encode("is:open is:pr author:DominicBurkart"),
            "is%3Aopen%20is%3Apr%20author%3ADominicBurkart"
        );
    }

    // ----- list_candidate_prs -----

    #[test]
    fn list_candidate_prs_returns_matching() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok());
        let out = list_candidate_prs(&t).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].owner, "DominicBurkart");
        assert_eq!(out[0].repo, "committer");
        assert_eq!(out[0].number, 7);
        assert_eq!(out[0].node_id, "PR_1");
        assert_eq!(out[0].head_sha, "deadbeef");
        let calls = t.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].1.contains("/search/issues"));
        assert!(
            calls[1]
                .1
                .contains("/repos/DominicBurkart/committer/pulls/7")
        );
    }

    #[test]
    fn list_candidate_prs_skips_missing_repo_url() {
        let body = br#"{"items":[{"number":1}]}"#;
        let t = MockTransport::new().queue_ok(body);
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_skips_foreign_owner() {
        let body = br#"{"items":[{
            "repository_url": "https://api.github.com/repos/OtherUser/repo",
            "number": 1
        }]}"#;
        let t = MockTransport::new().queue_ok(body);
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_skips_missing_number() {
        let body = br#"{"items":[{
            "repository_url": "https://api.github.com/repos/DominicBurkart/committer"
        }]}"#;
        let t = MockTransport::new().queue_ok(body);
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_skips_when_pulls_get_fails() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_err("network down");
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_skips_invalid_pr_json() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(b"{not valid json");
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_skips_missing_node_id() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(br#"{"head":{"sha":"deadbeef"}}"#);
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_skips_missing_head_sha() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(br#"{"node_id":"PR_1"}"#);
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_ignores_non_array_items() {
        // `items` is absent entirely; search returned an error payload.
        let t = MockTransport::new()
            .queue_ok(br#"{"message":"rate limit"}"#);
        let out = list_candidate_prs(&t).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn list_candidate_prs_propagates_search_error() {
        let t = MockTransport::new().queue_err("search boom");
        assert!(list_candidate_prs(&t).is_err());
    }

    // ----- evaluate -----

    #[test]
    fn evaluate_returns_nothing_on_clean() {
        let t = MockTransport::new()
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(br#"{"check_runs":[]}"#);
        assert_eq!(
            evaluate(&t, &candidate()).unwrap(),
            Action::Nothing
        );
    }

    #[test]
    fn evaluate_returns_update_branch_when_behind() {
        let t = MockTransport::new()
            .queue_ok(br#"{"mergeable":true,"mergeable_state":"behind"}"#)
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(br#"{"check_runs":[{"name":"ci","status":"completed","conclusion":"success"}]}"#);
        assert_eq!(
            evaluate(&t, &candidate()).unwrap(),
            Action::UpdateBranch
        );
    }

    #[test]
    fn evaluate_returns_draft_on_failing_check() {
        let t = MockTransport::new()
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(
                br#"{"check_runs":[{"name":"ci","status":"completed","conclusion":"failure"}]}"#,
            );
        let got = evaluate(&t, &candidate()).unwrap();
        assert!(matches!(got, Action::Draft(_)));
    }

    #[test]
    fn evaluate_handles_missing_check_runs_key() {
        let t = MockTransport::new()
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(b"{}");
        assert_eq!(
            evaluate(&t, &candidate()).unwrap(),
            Action::Nothing
        );
    }

    #[test]
    fn evaluate_returns_retry_when_mergeable_unknown() {
        let t = MockTransport::new()
            .queue_ok(
                br#"{"mergeable":null,"mergeable_state":"unknown"}"#,
            )
            .queue_ok(br#"{"state":"pending"}"#)
            .queue_ok(br#"{"check_runs":[]}"#);
        assert_eq!(
            evaluate(&t, &candidate()).unwrap(),
            Action::Retry
        );
    }

    #[test]
    fn evaluate_bubbles_pr_fetch_error() {
        let t = MockTransport::new().queue_err("pr boom");
        assert!(evaluate(&t, &candidate()).is_err());
    }

    #[test]
    fn evaluate_bubbles_status_fetch_error() {
        let t = MockTransport::new()
            .queue_ok(&pr_ok())
            .queue_err("status boom");
        assert!(evaluate(&t, &candidate()).is_err());
    }

    #[test]
    fn evaluate_bubbles_checks_fetch_error() {
        let t = MockTransport::new()
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_err("checks boom");
        assert!(evaluate(&t, &candidate()).is_err());
    }

    #[test]
    fn evaluate_bubbles_pr_parse_error() {
        let t = MockTransport::new().queue_ok(b"{not json");
        assert!(evaluate(&t, &candidate()).is_err());
    }

    #[test]
    fn evaluate_bubbles_status_parse_error() {
        let t = MockTransport::new()
            .queue_ok(&pr_ok())
            .queue_ok(b"{not json");
        assert!(evaluate(&t, &candidate()).is_err());
    }

    #[test]
    fn evaluate_bubbles_checks_parse_error() {
        let t = MockTransport::new()
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(b"{not json");
        assert!(evaluate(&t, &candidate()).is_err());
    }

    // ----- convert_to_draft -----

    #[test]
    fn convert_to_draft_succeeds_on_empty_errors() {
        let t = MockTransport::new().queue_ok(br#"{"errors":[]}"#);
        convert_to_draft(&t, "PR_1").unwrap();
        let calls = t.calls();
        assert_eq!(calls[0].0, "POST");
        assert!(calls[0].1.ends_with("/graphql"));
        assert!(calls[0].2.is_some());
    }

    #[test]
    fn convert_to_draft_succeeds_without_errors_key() {
        let t = MockTransport::new().queue_ok(
            br#"{"data":{"convertPullRequestToDraft":{}}}"#,
        );
        convert_to_draft(&t, "PR_1").unwrap();
    }

    #[test]
    fn convert_to_draft_bails_on_errors() {
        let t = MockTransport::new()
            .queue_ok(br#"{"errors":[{"message":"nope"}]}"#);
        let err = convert_to_draft(&t, "PR_1").unwrap_err();
        assert!(err.to_string().contains("graphql errors"));
    }

    #[test]
    fn convert_to_draft_bubbles_transport_error() {
        let t = MockTransport::new().queue_err("transport boom");
        assert!(convert_to_draft(&t, "PR_1").is_err());
    }

    #[test]
    fn convert_to_draft_bubbles_parse_error() {
        let t = MockTransport::new().queue_ok(b"{not json");
        assert!(convert_to_draft(&t, "PR_1").is_err());
    }

    // ----- update_branch -----

    #[test]
    fn update_branch_sends_put_with_expected_head() {
        let t = MockTransport::new().queue_ok(b"{}");
        update_branch(
            &t,
            "DominicBurkart",
            "committer",
            7,
            "deadbeef",
        )
        .unwrap();
        let calls = t.calls();
        assert_eq!(calls[0].0, "PUT");
        assert!(calls[0].1.contains("/pulls/7/update-branch"));
        let body = calls[0].2.as_ref().unwrap();
        let parsed: serde_json::Value =
            serde_json::from_slice(body).unwrap();
        assert_eq!(parsed["expected_head_sha"], "deadbeef");
    }

    #[test]
    fn update_branch_bubbles_transport_error() {
        let t = MockTransport::new().queue_err("boom");
        assert!(update_branch(&t, "o", "r", 1, "sha").is_err());
    }

    // ----- run_with -----

    #[test]
    fn run_with_swallows_list_error() {
        let t = MockTransport::new().queue_err("list boom");
        run_with(&t);
    }

    #[test]
    fn run_with_no_candidates_is_ok() {
        let t = MockTransport::new().queue_ok(br#"{"items":[]}"#);
        run_with(&t);
    }

    #[test]
    fn run_with_handles_nothing_action() {
        // search + pr-for-list + pr-for-eval + status + checks
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok())
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(br#"{"check_runs":[]}"#);
        run_with(&t);
    }

    #[test]
    fn run_with_handles_retry_action() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok())
            .queue_ok(
                br#"{"mergeable":null,"mergeable_state":"unknown"}"#,
            )
            .queue_ok(br#"{"state":"pending"}"#)
            .queue_ok(br#"{"check_runs":[]}"#);
        run_with(&t);
    }

    #[test]
    fn run_with_skips_on_evaluate_error() {
        // search returns an item; the first evaluate call (pr fetch) fails.
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok())
            .queue_err("pr boom");
        run_with(&t);
    }

    #[test]
    fn run_with_drafts_then_converts() {
        let failing_checks = br#"{"check_runs":[{"name":"ci","status":"completed","conclusion":"failure"}]}"#;
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok())
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(failing_checks)
            .queue_ok(br#"{"errors":[]}"#);
        run_with(&t);
    }

    #[test]
    fn run_with_logs_when_convert_to_draft_fails() {
        let failing_checks = br#"{"check_runs":[{"name":"ci","status":"completed","conclusion":"failure"}]}"#;
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok())
            .queue_ok(&pr_ok())
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(failing_checks)
            .queue_err("graphql boom");
        run_with(&t);
    }

    #[test]
    fn run_with_updates_branch_when_behind() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok())
            .queue_ok(
                br#"{"mergeable":true,"mergeable_state":"behind"}"#,
            )
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(br#"{"check_runs":[]}"#)
            .queue_ok(b"{}");
        run_with(&t);
    }

    #[test]
    fn run_with_logs_when_update_branch_fails() {
        let t = MockTransport::new()
            .queue_ok(&search_one_item())
            .queue_ok(&pr_ok())
            .queue_ok(
                br#"{"mergeable":true,"mergeable_state":"behind"}"#,
            )
            .queue_ok(br#"{"state":"success"}"#)
            .queue_ok(br#"{"check_runs":[]}"#)
            .queue_err("update-branch boom");
        run_with(&t);
    }

    // ----- ReqwestTransport / send / run -----

    pub static ENV_LOCK: std::sync::Mutex<()> =
        std::sync::Mutex::new(());

    /// Spin up a single-shot HTTP server bound to 127.0.0.1, returning
    /// the URL prefix and a join handle whose payload is the recorded
    /// request bytes. The server reads one HTTP request, replies with
    /// `response`, and exits. Used to drive the real reqwest client
    /// without touching the network.
    pub fn spawn_one_shot_server(
        response: &'static [u8],
    ) -> (String, std::thread::JoinHandle<Vec<u8>>) {
        use std::io::{Read, Write};
        let listener =
            std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(
                    std::time::Duration::from_secs(5),
                ))
                .unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = match stream.read(&mut buf) {
                    Ok(n) if n > 0 => n,
                    _ => break,
                };
                req.extend_from_slice(&buf[..n]);
                if request_is_complete(&req) {
                    break;
                }
            }
            stream.write_all(response).unwrap();
            req
        });
        (url, handle)
    }

    fn request_is_complete(req: &[u8]) -> bool {
        let Some(headers_end) =
            req.windows(4).position(|w| w == b"\r\n\r\n")
        else {
            return false;
        };
        let header_str =
            std::str::from_utf8(&req[..headers_end]).unwrap_or("");
        let content_length = header_str
            .lines()
            .find_map(|l| {
                let lower = l.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap_or(0))
            })
            .unwrap_or(0);
        req.len() >= headers_end + 4 + content_length
    }

    pub fn ok_response(body: &str) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body.as_bytes());
        out
    }

    fn status_response(code: u16, body: &str) -> Vec<u8> {
        let mut out = format!(
            "HTTP/1.1 {code} ERR\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        out.extend_from_slice(body.as_bytes());
        out
    }

    fn leak<T: 'static>(v: T) -> &'static T {
        Box::leak(Box::new(v))
    }

    #[test]
    fn reqwest_transport_new_succeeds() {
        let t = ReqwestTransport::new("tok");
        assert_eq!(t.token, "tok");
    }

    #[test]
    fn reqwest_transport_get_round_trips() {
        let resp: &'static [u8] = leak(ok_response(r#"{"ok":true}"#));
        let (url, handle) = spawn_one_shot_server(resp);
        let t = ReqwestTransport::new("tok");
        let body =
            t.request("GET", &format!("{url}/things"), None).unwrap();
        assert_eq!(body, br#"{"ok":true}"#);
        let raw_req = handle.join().unwrap();
        let req_str = String::from_utf8_lossy(&raw_req);
        assert!(req_str.starts_with("GET /things HTTP/1.1"));
        assert!(req_str.contains("authorization: Bearer tok"));
        assert!(
            req_str.contains("accept: application/vnd.github+json")
        );
        assert!(req_str.contains("x-github-api-version: 2022-11-28"));
        assert!(
            req_str.to_lowercase().contains("user-agent: prodder")
        );
    }

    #[test]
    fn send_post_attaches_body_and_content_type() {
        let resp: &'static [u8] = leak(ok_response("{}"));
        let (url, handle) = spawn_one_shot_server(resp);
        let client = reqwest::blocking::Client::new();
        let body = send(
            &client,
            "tok",
            "POST",
            &format!("{url}/graphql"),
            Some(b"{\"q\":1}"),
        )
        .unwrap();
        assert_eq!(body, b"{}");
        let raw_req = handle.join().unwrap();
        let req_str = String::from_utf8_lossy(&raw_req);
        assert!(req_str.starts_with("POST /graphql HTTP/1.1"));
        assert!(req_str.contains("content-type: application/json"));
        assert!(req_str.ends_with("{\"q\":1}"));
    }

    #[test]
    fn send_returns_err_on_non_success_status() {
        let resp: &'static [u8] = leak(status_response(500, "boom"));
        let (url, handle) = spawn_one_shot_server(resp);
        let client = reqwest::blocking::Client::new();
        let err =
            send(&client, "tok", "GET", &format!("{url}/x"), None)
                .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("request failed"), "{msg}");
        assert!(msg.contains("500"), "{msg}");
        let _ = handle.join();
    }

    #[test]
    fn send_rejects_invalid_method() {
        let client = reqwest::blocking::Client::new();
        let err = send(
            &client,
            "tok",
            "BAD METHOD",
            "http://127.0.0.1:1",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("invalid HTTP method"));
    }

    #[test]
    fn send_bubbles_transport_error() {
        // Port 1 is reserved/unreachable on most systems; reqwest
        // surfaces a connection error which `send` wraps with
        // `sending request`.
        let client = reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        let err = send(
            &client,
            "tok",
            "GET",
            "http://127.0.0.1:1/never",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("sending request"));
    }

    #[test]
    fn run_wraps_reqwest_transport() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let resp: &'static [u8] =
            leak(ok_response(r#"{"items":[]}"#));
        let (url, handle) = spawn_one_shot_server(resp);
        // Safety: guarded by ENV_LOCK.
        unsafe { std::env::set_var("PRODDER_API_BASE", &url) };
        let res = run("tok");
        // Safety: guarded by ENV_LOCK.
        unsafe { std::env::remove_var("PRODDER_API_BASE") };
        assert!(res.is_ok(), "run failed: {res:?}");
        let _ = handle.join();
    }

    #[test]
    fn api_base_defaults_to_github() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Safety: guarded by ENV_LOCK.
        unsafe { std::env::remove_var("PRODDER_API_BASE") };
        assert_eq!(api_base(), DEFAULT_API);
    }

    #[test]
    fn api_base_honors_env_override() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Safety: guarded by ENV_LOCK.
        unsafe {
            std::env::set_var(
                "PRODDER_API_BASE",
                "http://localhost:1234",
            );
        }
        let got = api_base();
        // Safety: guarded by ENV_LOCK.
        unsafe { std::env::remove_var("PRODDER_API_BASE") };
        assert_eq!(got, "http://localhost:1234");
    }
}
