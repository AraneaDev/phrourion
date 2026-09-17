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

fn text(v: &Value, key: &str) -> String {
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

fn pr_item(v: &Value) -> Item {
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

fn issue_item(v: &Value) -> Item {
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
                        .map(issue_item)
                        .collect(),
                ),
                Err(e) => Observation::failure(e),
            };
            match releases.and_then(|v| flatten_pages(v, None)) {
                Ok(rows) => {
                    let item = |v: &Value| Item {
                        title: text(v, "tag_name"),
                        url: text(v, "html_url"),
                        detail: format!("published {}", text(v, "published_at")),
                    };
                    state.drafts = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == true)
                            .map(item)
                            .collect(),
                    );
                    state.published = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == false)
                            .map(item)
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
}
