//! Hosting adapters return neutral records. No hosting-specific types reach the UI.
use crate::{
    command,
    model::{Item, Observation, ProviderKind, RemoteState, Repo},
};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{future::Future, pin::Pin, time::Duration};

pub type SnapshotFuture<'a> = Pin<Box<dyn Future<Output = RemoteState> + Send + 'a>>;

pub trait RemoteProvider: Send + Sync {
    fn snapshot<'a>(&'a self, repo: &'a Repo) -> SnapshotFuture<'a>;
}

pub fn adapter(kind: &ProviderKind) -> Box<dyn RemoteProvider> {
    match kind {
        ProviderKind::Github => Box::new(Github),
        ProviderKind::Forgejo => Box::new(crate::forgejo::Forgejo),
        _ => Box::new(Unsupported),
    }
}

struct Unsupported;
impl RemoteProvider for Unsupported {
    fn snapshot<'a>(&'a self, _: &'a Repo) -> SnapshotFuture<'a> {
        Box::pin(async {
            RemoteState {
                default_branch: Observation::unsupported(),
                branches: Observation::unsupported(),
                prs: Observation::unsupported(),
                review_requests: Observation::unsupported(),
                issues: Observation::unsupported(),
                proposals: Observation::unsupported(),
                drafts: Observation::unsupported(),
                published: Observation::unsupported(),
                ci: Observation::unsupported(),
                publication: Observation::unsupported(),
            }
        })
    }
}

pub struct Github;

const RATE_LIMIT_ATTEMPTS: u32 = 3;
const RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(1);

// gh reports both primary and secondary rate limits as plain text on stderr, no
// structured status; matching a substring is the only signal command::run exposes.
fn is_rate_limited(error: &str) -> bool {
    let lower = error.to_lowercase();
    lower.contains("rate limit") || lower.contains("http 429")
}

async fn api(repo: &Repo, endpoint: &str, pages: bool) -> Result<Value> {
    let executable = std::env::var("PHROURION_GH").unwrap_or_else(|_| "gh".into());
    let mut args = vec![
        "api",
        "--hostname",
        repo.identity.host.as_str(),
        "--method",
        "GET",
        endpoint,
    ];
    if pages {
        args.extend(["--paginate", "--slurp"]);
    }
    let mut backoff = RATE_LIMIT_BACKOFF;
    let mut attempts_left = RATE_LIMIT_ATTEMPTS;
    loop {
        attempts_left -= 1;
        match command::run(&executable, &args, &repo.path).await {
            Ok(text) => return serde_json::from_str(&text).context("Invalid provider JSON"),
            Err(e) if attempts_left > 0 && is_rate_limited(&e.to_string()) => {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn flatten_pages(value: Value, key: Option<&str>) -> Result<Vec<Value>> {
    let pages = value.as_array().context("Expected paginated response")?;
    let mut rows = Vec::new();
    for page in pages {
        let array = key
            .map_or(page, |k| &page[k])
            .as_array()
            .context("Incomplete or malformed provider page")?;
        rows.extend(array.iter().cloned());
    }
    Ok(rows)
}

pub(crate) fn text(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or_default().into()
}

pub fn release_evidence(v: &Value, labels: &[String]) -> Option<String> {
    let names: Vec<_> = v["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str())
        .collect();
    if names
        .iter()
        .any(|n| *n == "autorelease: pending" || labels.iter().any(|x| x == n))
    {
        return Some("release label".into());
    }
    if v["head"]["ref"]
        .as_str()
        .unwrap_or("")
        .starts_with("release-please--")
    {
        return Some("Release Please branch".into());
    }
    let title = text(v, "title").to_lowercase();
    if title.starts_with("chore(main): release")
        || title.starts_with("chore: release")
        || title.starts_with("release:")
    {
        return Some("candidate: title only".into());
    }
    None
}

pub(crate) fn pr_item(v: &Value) -> Item {
    Item {
        title: format!("#{} {}", v["number"], text(v, "title")),
        url: text(v, "html_url"),
        detail: format!(
            "{} -> {} | head {} | {}",
            v["head"]["ref"].as_str().unwrap_or("?"),
            v["base"]["ref"].as_str().unwrap_or("?"),
            v["head"]["sha"].as_str().unwrap_or("?"),
            if v["draft"] == true { "draft" } else { "open" }
        ),
    }
}

// GitHub's issues endpoint also returns pull requests; only the latter carry this key.
fn is_pull_request(v: &Value) -> bool {
    !v["pull_request"].is_null()
}

// An empty filter list means unfiltered; otherwise the issue must carry at least one.
pub(crate) fn matches_issue_labels(v: &Value, labels: &[String]) -> bool {
    if labels.is_empty() {
        return true;
    }
    v["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["name"].as_str())
        .any(|n| labels.iter().any(|x| x == n))
}

pub(crate) fn issue_item(v: &Value) -> Item {
    Item {
        title: format!("#{} {}", v["number"], text(v, "title")),
        url: text(v, "html_url"),
        detail: v["labels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|l| l["name"].as_str())
            .collect::<Vec<_>>()
            .join(", "),
    }
}

pub(crate) fn release_item(v: &Value) -> Item {
    Item {
        title: text(v, "tag_name"),
        url: text(v, "html_url"),
        detail: format!("published {}", text(v, "published_at")),
    }
}

fn requested_reviewer(v: &Value, login: &str) -> bool {
    !login.is_empty()
        && v["requested_reviewers"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|u| u["login"].as_str() == Some(login))
}

fn run_item(v: &Value) -> Item {
    Item {
        title: text(v, "name"),
        url: text(v, "html_url"),
        detail: format!(
            "{} {} | {}",
            text(v, "status"),
            text(v, "conclusion"),
            text(v, "head_sha")
        ),
    }
}

impl RemoteProvider for Github {
    fn snapshot<'a>(&'a self, repo: &'a Repo) -> SnapshotFuture<'a> {
        Box::pin(async move {
            let base = format!("repos/{}", repo.identity.project);
            let mut state = RemoteState::default();
            match api(repo, &base, false).await {
                Ok(v) => {
                    state.default_branch = match v["default_branch"].as_str() {
                        Some(branch) => Observation::success(branch.to_string()),
                        None => Observation::failure("Missing default branch"),
                    }
                }
                Err(e) => state.default_branch = Observation::failure(e),
            }
            let branches_url = format!("{base}/branches?per_page=100");
            let prs_url = format!("{base}/pulls?state=open&per_page=100");
            let issues_url = format!("{base}/issues?state=open&per_page=100");
            let releases_url = format!("{base}/releases?per_page=100");
            let (branches, prs, issues, releases, user) = tokio::join!(
                api(repo, &branches_url, true),
                api(repo, &prs_url, true),
                api(repo, &issues_url, true),
                api(repo, &releases_url, true),
                api(repo, "user", false)
            );
            let login = user
                .ok()
                .and_then(|v| v["login"].as_str().map(String::from))
                .unwrap_or_default();
            state.branches = match branches.and_then(|v| flatten_pages(v, None)) {
                Ok(rows) => Observation::success(
                    rows.iter()
                        .map(|v| Item {
                            title: text(v, "name"),
                            detail: v["commit"]["sha"].as_str().unwrap_or("?").into(),
                            url: String::new(),
                        })
                        .collect(),
                ),
                Err(e) => Observation::failure(e),
            };
            match prs.and_then(|v| flatten_pages(v, None)) {
                Ok(rows) => {
                    state.prs = Observation::success(rows.iter().map(pr_item).collect());
                    state.proposals = Observation::success(
                        rows.iter()
                            .filter_map(|v| {
                                release_evidence(v, &repo.release_labels).map(|evidence| {
                                    let mut item = pr_item(v);
                                    item.detail = format!("{evidence} | {}", item.detail);
                                    item
                                })
                            })
                            .collect(),
                    );
                    state.review_requests = Observation::success(
                        rows.iter()
                            .filter(|v| requested_reviewer(v, &login))
                            .map(pr_item)
                            .collect(),
                    );
                }
                Err(e) => {
                    state.prs = Observation::failure(&e);
                    state.proposals = Observation::failure(&e);
                    state.review_requests = Observation::failure(e);
                }
            }
            state.issues = match issues.and_then(|v| flatten_pages(v, None)) {
                Ok(rows) => Observation::success(
                    rows.iter()
                        .filter(|v| !is_pull_request(v))
                        .filter(|v| matches_issue_labels(v, &repo.issue_labels))
                        .map(issue_item)
                        .collect(),
                ),
                Err(e) => Observation::failure(e),
            };
            match releases.and_then(|v| flatten_pages(v, None)) {
                Ok(rows) => {
                    state.drafts = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == true)
                            .map(release_item)
                            .collect(),
                    );
                    state.published = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == false)
                            .map(release_item)
                            .collect(),
                    );
                }
                Err(e) => {
                    state.drafts = Observation::failure(&e);
                    state.published = Observation::failure(e);
                }
            }
            // Exact default-branch commit checks, including external status contexts.
            state.ci = if let Some(branch) = &state.default_branch.data {
                let branch =
                    url::form_urlencoded::byte_serialize(branch.as_bytes()).collect::<String>();
                match api(repo, &format!("{base}/commits/{branch}"), false).await {
                    Ok(commit) => {
                        let sha = text(&commit, "sha");
                        let checks = api(
                            repo,
                            &format!("{base}/commits/{sha}/check-runs?per_page=100"),
                            true,
                        )
                        .await
                        .and_then(|v| flatten_pages(v, Some("check_runs")));
                        let statuses = api(
                            repo,
                            &format!("{base}/commits/{sha}/status?per_page=100"),
                            true,
                        )
                        .await
                        .and_then(|v| flatten_pages(v, Some("statuses")));
                        match (checks, statuses) {
                            (Ok(checks), Ok(statuses)) => {
                                let mut items: Vec<_> = checks
                                    .iter()
                                    .map(|v| Item {
                                        title: text(v, "name"),
                                        url: text(v, "html_url"),
                                        detail: format!(
                                            "{} {} | {sha}",
                                            text(v, "status"),
                                            text(v, "conclusion")
                                        ),
                                    })
                                    .collect();
                                items.extend(statuses.iter().map(|v| Item {
                                    title: text(v, "context"),
                                    url: text(v, "target_url"),
                                    detail: format!("{} | {sha}", text(v, "state")),
                                }));
                                Observation::success(items)
                            }
                            (Err(e), _) | (_, Err(e)) => Observation::failure(e),
                        }
                    }
                    Err(e) => Observation::failure(e),
                }
            } else {
                Observation::failure("Default branch is unknown")
            };
            state.publication = if repo.release_workflows.is_empty() {
                Observation::unsupported()
            } else {
                let mut items = Vec::new();
                let mut error = None;
                for workflow in &repo.release_workflows {
                    let encoded = url::form_urlencoded::byte_serialize(workflow.as_bytes())
                        .collect::<String>();
                    match api(
                        repo,
                        &format!("{base}/actions/workflows/{encoded}/runs?per_page=100"),
                        true,
                    )
                    .await
                    .and_then(|v| flatten_pages(v, Some("workflow_runs")))
                    {
                        Ok(rows) => {
                            // Retain every running attempt, plus the latest completed attempt.
                            let mut completed = false;
                            for v in rows {
                                if v["status"] != "completed" {
                                    items.push(run_item(&v));
                                } else if !completed {
                                    items.push(run_item(&v));
                                    completed = true;
                                }
                            }
                        }
                        Err(e) => {
                            error = Some(e);
                            break;
                        }
                    }
                }
                match error {
                    Some(e) => Observation::failure(e),
                    None => Observation::success(items),
                }
            };
            state
        })
    }
}

pub fn repo_url(repo: &Repo) -> Option<String> {
    if repo.identity.kind == ProviderKind::Local {
        None
    } else {
        Some(format!(
            "https://{}/{}",
            repo.identity.host, repo.identity.project
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Rust's test harness runs different #[test]/#[tokio::test] fns
    // concurrently on separate threads by default, but PHROURION_GH is
    // process-global. Every test below that sets it holds this lock for
    // its whole body so no two such tests interleave.
    static GH_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    fn test_repo() -> Repo {
        Repo {
            id: "t".into(),
            name: "t".into(),
            path: std::env::temp_dir(),
            remote: "origin".into(),
            identity: crate::model::Remote {
                kind: ProviderKind::Github,
                host: "example.com".into(),
                project: "org/repo".into(),
            },
            enabled: true,
            release_workflows: vec![],
            release_labels: vec![],
            issue_labels: vec![],
        }
    }

    fn write_executable(dir: &std::path::Path, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, contents).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path
    }

    // A fake `gh` for testing api()/Github::snapshot() without a real
    // network call or a `gh` installation. api()'s args are always
    // `api --hostname HOST --method GET ENDPOINT [--paginate --slurp]`,
    // so the endpoint is always positional arg $6. `overrides` maps an
    // endpoint substring to the raw JSON gh would print for it; anything
    // else gets an empty page (`[[]]` for --paginate --slurp calls, `{}`
    // otherwise) so the rest of snapshot()'s concurrent calls succeed
    // harmlessly instead of hanging or failing the whole snapshot.
    fn fake_gh(dir: &std::path::Path, overrides: &[(&str, &str)]) -> std::path::PathBuf {
        let mut script = String::from("#!/bin/sh\nendpoint=\"$6\"\ncase \"$endpoint\" in\n");
        for (pattern, body) in overrides {
            script.push_str(&format!("  *{pattern}*) printf '%s' '{body}' ;;\n"));
        }
        script.push_str(
            "  *) if [ \"$7\" = \"--paginate\" ]; then printf '%s' '[[]]'; else printf '%s' '{}'; fi ;;\n",
        );
        script.push_str("esac\n");
        write_executable(dir, "fake-gh", &script)
    }

    // Caller must hold GH_ENV_LOCK for the duration of this call.
    async fn with_fake_gh<T>(gh: &std::path::Path, fut: impl std::future::Future<Output = T>) -> T {
        let previous = std::env::var("PHROURION_GH").ok();
        unsafe {
            std::env::set_var("PHROURION_GH", gh);
        }
        let result = fut.await;
        unsafe {
            match &previous {
                Some(v) => std::env::set_var("PHROURION_GH", v),
                None => std::env::remove_var("PHROURION_GH"),
            }
        }
        result
    }

    #[test]
    fn pagination_requires_every_page_to_have_expected_shape() {
        assert_eq!(flatten_pages(json!([[1], [2, 3]]), None).unwrap().len(), 3);
        assert!(flatten_pages(json!([[1], {"message":"denied"}]), None).is_err());
        assert!(flatten_pages(json!({"message":"denied"}), None).is_err());
    }

    #[test]
    fn rate_limit_detection_matches_ghs_stderr_phrasing_only() {
        assert!(is_rate_limited(
            "gh: API rate limit exceeded for user ID 123. (HTTP 403)"
        ));
        assert!(is_rate_limited(
            "gh: You have exceeded a secondary rate limit. (HTTP 403)"
        ));
        assert!(is_rate_limited("gh: (HTTP 429)"));
        assert!(!is_rate_limited("gh: Not Found (HTTP 404)"));
        assert!(!is_rate_limited("gh: Bad credentials (HTTP 401)"));
    }

    #[test]
    fn review_requests_match_the_authenticated_login_only() {
        let pr = json!({
            "requested_reviewers": [{"login": "octocat"}, {"login": "hubot"}],
        });
        assert!(requested_reviewer(&pr, "octocat"));
        assert!(!requested_reviewer(&pr, "someone-else"));
        assert!(!requested_reviewer(&pr, ""));
        assert!(!requested_reviewer(&json!({}), "octocat"));
    }

    #[test]
    fn issue_label_filter_is_permissive_when_empty_and_an_any_match_otherwise() {
        let issue = json!({"labels": [{"name": "bug"}, {"name": "triage"}]});
        assert!(matches_issue_labels(&issue, &[]));
        assert!(matches_issue_labels(&issue, &["bug".into()]));
        assert!(matches_issue_labels(
            &issue,
            &["unrelated".into(), "triage".into()]
        ));
        assert!(!matches_issue_labels(&issue, &["unrelated".into()]));
        assert!(!matches_issue_labels(&json!({}), &["bug".into()]));
    }

    #[test]
    fn issue_listing_excludes_pull_requests_and_joins_labels() {
        let issue = json!({
            "number": 42,
            "title": "Crash on empty registry",
            "html_url": "https://github.com/o/r/issues/42",
            "labels": [{"name": "bug"}, {"name": "triage"}],
        });
        let pr = json!({
            "number": 43,
            "title": "Fix crash",
            "pull_request": {"url": "https://api.github.com/repos/o/r/pulls/43"},
        });
        assert!(!is_pull_request(&issue));
        assert!(is_pull_request(&pr));
        let item = issue_item(&issue);
        assert_eq!(item.title, "#42 Crash on empty registry");
        assert_eq!(item.url, "https://github.com/o/r/issues/42");
        assert_eq!(item.detail, "bug, triage");
    }

    #[test]
    fn unlabeled_release_branches_are_detected_but_version_fixes_are_not() {
        assert_eq!(
            release_evidence(
                &json!({"head":{"ref":"release-please--branches--main"}}),
                &[]
            )
            .as_deref(),
            Some("Release Please branch")
        );
        assert!(release_evidence(&json!({"title":"fix: bump runtime version"}), &[]).is_none());
        assert_eq!(
            release_evidence(&json!({"title":"chore(main): release 1.2.3"}), &[]).as_deref(),
            Some("candidate: title only")
        );
    }

    #[test]
    fn release_evidence_detects_the_autorelease_pending_label() {
        assert_eq!(
            release_evidence(&json!({"labels":[{"name":"autorelease: pending"}]}), &[]).as_deref(),
            Some("release label")
        );
    }

    #[test]
    fn release_evidence_detects_a_caller_configured_release_label_but_not_an_unrelated_one() {
        let labels = ["needs-release".to_string()];
        assert_eq!(
            release_evidence(&json!({"labels":[{"name":"needs-release"}]}), &labels).as_deref(),
            Some("release label")
        );
        assert!(release_evidence(&json!({"labels":[{"name":"unrelated"}]}), &labels).is_none());
    }

    #[test]
    fn release_evidence_detects_any_of_the_three_title_prefixes() {
        assert!(release_evidence(&json!({"title": "chore: release 1.0"}), &[]).is_some());
        assert!(release_evidence(&json!({"title": "release: cut 2.0"}), &[]).is_some());
        assert!(release_evidence(&json!({"title": "unrelated change"}), &[]).is_none());
    }

    #[test]
    fn pr_item_formats_title_url_and_detail_from_a_pull_request() {
        let v = json!({
            "number": 42,
            "title": "Add feature",
            "html_url": "https://example.com/pr/42",
            "head": {"ref": "feature-x", "sha": "abcdef1"},
            "base": {"ref": "main"},
            "draft": false
        });
        let item = pr_item(&v);
        assert_eq!(item.title, "#42 Add feature");
        assert_eq!(item.url, "https://example.com/pr/42");
        assert_eq!(item.detail, "feature-x -> main | head abcdef1 | open");
    }

    #[test]
    fn release_item_formats_tag_url_and_published_detail() {
        let v = json!({
            "tag_name": "v1.2.3",
            "html_url": "https://example.com/releases/v1.2.3",
            "published_at": "2024-01-01T00:00:00Z"
        });
        let item = release_item(&v);
        assert_eq!(item.title, "v1.2.3");
        assert_eq!(item.url, "https://example.com/releases/v1.2.3");
        assert_eq!(item.detail, "published 2024-01-01T00:00:00Z");
    }

    #[test]
    fn run_item_formats_status_conclusion_and_sha() {
        let v = json!({
            "name": "CI",
            "html_url": "https://example.com/runs/1",
            "status": "completed",
            "conclusion": "success",
            "head_sha": "deadbee"
        });
        let item = run_item(&v);
        assert_eq!(item.title, "CI");
        assert_eq!(item.url, "https://example.com/runs/1");
        assert_eq!(item.detail, "completed success | deadbee");
    }

    #[tokio::test]
    async fn adapter_dispatches_github_and_forgejo_and_falls_back_to_unsupported() {
        let _forgejo_guard = crate::test_support::FORGEJO_ENV_LOCK.lock().await;
        let _gh_guard = GH_ENV_LOCK.lock().await;
        let repo = test_repo();

        // Force a fast, deterministic failure instead of depending on
        // whether a real `gh` is installed or reachable in this environment.
        let previous_gh = std::env::var("PHROURION_GH").ok();
        unsafe {
            std::env::set_var("PHROURION_GH", "/nonexistent/phrourion-test-gh-binary");
        }
        let github_state = adapter(&ProviderKind::Github).snapshot(&repo).await;
        unsafe {
            match &previous_gh {
                Some(v) => std::env::set_var("PHROURION_GH", v),
                None => std::env::remove_var("PHROURION_GH"),
            }
        }

        let previous_url = std::env::var("PHROURION_FORGEJO_TEST_URL").ok();
        unsafe {
            // Nothing listens on this local port: an instant, network- and
            // DNS-independent connection refusal (it's a literal IP).
            std::env::set_var("PHROURION_FORGEJO_TEST_URL", "http://127.0.0.1:1");
        }
        let forgejo_state = adapter(&ProviderKind::Forgejo).snapshot(&repo).await;
        unsafe {
            match &previous_url {
                Some(v) => std::env::set_var("PHROURION_FORGEJO_TEST_URL", v),
                None => std::env::remove_var("PHROURION_FORGEJO_TEST_URL"),
            }
        }

        let unsupported_state = adapter(&ProviderKind::Local).snapshot(&repo).await;

        // Unsupported never attempts anything: every field is the disabled
        // placeholder, not an error.
        assert!(!unsupported_state.default_branch.supported);

        // Github attempted (and failed to even start) a real call — a
        // *supported* feature that errored, observably distinct both from
        // "feature disabled" and from api()'s own success path.
        let github_error = github_state
            .default_branch
            .error
            .clone()
            .unwrap_or_default();
        assert!(github_state.default_branch.supported);
        assert!(
            github_error.contains("Cannot start"),
            "expected a process-spawn failure, got: {github_error}"
        );

        assert!(forgejo_state.default_branch.supported);
        assert!(forgejo_state.default_branch.error.is_some());
    }

    #[tokio::test]
    async fn api_retries_on_a_rate_limited_error_then_succeeds() {
        let _guard = GH_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let script = format!(
            "#!/bin/sh\n\
             n=$(cat '{counter}' 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo \"$n\" > '{counter}'\n\
             if [ \"$n\" -lt 2 ]; then\n\
             \x20 echo 'secondary rate limit exceeded' >&2\n\
             \x20 exit 1\n\
             fi\n\
             printf '%s' '{{\"ok\":true}}'\n",
            counter = counter.display()
        );
        let gh = write_executable(dir.path(), "fake-gh-retry", &script);
        let repo = test_repo();
        let value = with_fake_gh(&gh, api(&repo, "some/endpoint", false))
            .await
            .expect("should succeed after one retry");
        assert_eq!(value["ok"], true);
        assert_eq!(
            std::fs::read_to_string(&counter).unwrap().trim(),
            "2",
            "expected exactly one retry"
        );
    }

    #[tokio::test]
    async fn api_gives_up_after_exhausting_rate_limit_retries() {
        let _guard = GH_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let counter = dir.path().join("count");
        let script = format!(
            "#!/bin/sh\n\
             n=$(cat '{counter}' 2>/dev/null || echo 0)\n\
             n=$((n+1))\n\
             echo \"$n\" > '{counter}'\n\
             echo 'secondary rate limit exceeded' >&2\n\
             exit 1\n",
            counter = counter.display()
        );
        let gh = write_executable(dir.path(), "fake-gh-exhaust", &script);
        let repo = test_repo();
        let result = with_fake_gh(&gh, api(&repo, "some/endpoint", false)).await;
        assert!(result.is_err());
        // RATE_LIMIT_ATTEMPTS = 3: exactly 3 attempts — neither a premature
        // give-up nor an infinite retry loop.
        assert_eq!(std::fs::read_to_string(&counter).unwrap().trim(), "3");
    }

    #[tokio::test]
    async fn github_snapshot_splits_releases_into_drafts_and_published() {
        let _guard = GH_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            &[(
                "releases",
                r#"[[{"tag_name":"v1-draft","draft":true,"html_url":"u1","published_at":""},{"tag_name":"v2-published","draft":false,"html_url":"u2","published_at":"2024-01-01"}]]"#,
            )],
        );
        let repo = test_repo();
        let state = with_fake_gh(&gh, Github.snapshot(&repo)).await;

        let drafts = state.drafts.data.expect("drafts should be populated");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].title, "v1-draft");

        let published = state.published.data.expect("published should be populated");
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].title, "v2-published");
    }

    #[tokio::test]
    async fn github_snapshot_filters_pull_requests_out_of_issues() {
        let _guard = GH_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            &[(
                "issues",
                r#"[[{"number":1,"title":"real issue","html_url":"u1","labels":[]},{"number":2,"title":"a pr shaped like an issue","html_url":"u2","labels":[],"pull_request":{}}]]"#,
            )],
        );
        let repo = test_repo();
        let state = with_fake_gh(&gh, Github.snapshot(&repo)).await;

        let issues = state.issues.data.expect("issues should be populated");
        assert_eq!(issues.len(), 1);
        assert!(issues[0].title.contains("real issue"));
    }

    #[tokio::test]
    async fn github_snapshot_publication_retains_every_running_attempt_plus_latest_completed() {
        let _guard = GH_ENV_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();
        let gh = fake_gh(
            dir.path(),
            &[(
                "actions/workflows",
                r#"[{"workflow_runs":[
                    {"name":"r1","html_url":"u1","status":"in_progress","conclusion":"","head_sha":"s1"},
                    {"name":"r2","html_url":"u2","status":"completed","conclusion":"success","head_sha":"s2"},
                    {"name":"r3","html_url":"u3","status":"completed","conclusion":"failure","head_sha":"s3"}
                ]}]"#,
            )],
        );
        let mut repo = test_repo();
        repo.release_workflows = vec!["ci.yml".into()];
        let state = with_fake_gh(&gh, Github.snapshot(&repo)).await;

        let items = state
            .publication
            .data
            .expect("publication should be populated");
        assert_eq!(
            items.len(),
            2,
            "should keep the running run plus only the latest completed one"
        );
        assert_eq!(items[0].title, "r1");
        assert_eq!(items[1].title, "r2");
    }
}
