//! Forgejo/Gitea-compatible hosting adapter. HTTP-based: no gh-equivalent CLI
//! exists for Forgejo, so this talks to the REST API directly.

use crate::{
    model::{Item, Observation, RemoteState, Repo},
    provider::{self, RemoteProvider, SnapshotFuture},
};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;
use std::{sync::OnceLock, time::Duration};

pub fn env_token_var(host: &str) -> String {
    let normalized: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PHROURION_TOKEN_{normalized}")
}

pub(crate) fn base_url(host: &str) -> String {
    let root =
        std::env::var("PHROURION_FORGEJO_TEST_URL").unwrap_or_else(|_| format!("https://{host}"));
    format!("{root}/api/v1")
}

pub(crate) fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        // A hung host (dead reverse proxy, half-open connection) must not be
        // able to block a request forever: each remote snapshot runs under a
        // shared 4-permit semaphore (see src/tui.rs), so an indefinitely
        // pending request would permanently occupy one of those slots. Mirror
        // the 45-second budget `src/command.rs::run` gives `gh` subprocesses.
        Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
            .unwrap_or_default()
    })
}

pub(crate) async fn get(host: &str, path: &str, query: &[(&str, &str)]) -> Result<Value> {
    let url = format!("{}/{path}", base_url(host));
    let mut request = client().get(&url).query(query);
    // An exported-but-empty token (`export PHROURION_TOKEN_X=""`) must fall
    // back to unauthenticated, not send a broken `Authorization: token `
    // header that will likely 401.
    if let Some(token) = std::env::var(env_token_var(host))
        .ok()
        .filter(|t| !t.is_empty())
    {
        request = request.header("Authorization", format!("token {token}"));
    }
    let response = request.send().await.context("Forgejo request failed")?;
    let status = response.status();
    let body = response
        .text()
        .await
        .context("Failed to read Forgejo response body")?;
    if status.is_success() {
        return serde_json::from_str(&body)
            .with_context(|| format!("Invalid Forgejo JSON ({status})"));
    }
    // A non-2xx response isn't guaranteed to be JSON at all (a misconfigured
    // reverse proxy can return an HTML error page, or plain text). Surface as
    // much of the actual body as possible instead of a generic parse error,
    // since this is exactly the detail someone needs when a newly registered
    // host isn't working.
    let message = serde_json::from_str::<Value>(&body)
        .ok()
        .and_then(|v| v["message"].as_str().map(str::to_string))
        .unwrap_or_else(|| {
            let snippet: String = body.chars().take(200).collect();
            if snippet.is_empty() {
                "<empty body>".to_string()
            } else {
                snippet
            }
        });
    bail!("{status}: {message}");
}

pub(crate) async fn paginated(
    host: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut page: u32 = 1;
    loop {
        let page_str = page.to_string();
        let mut full_query: Vec<(&str, &str)> = query.to_vec();
        full_query.push(("page", &page_str));
        full_query.push(("limit", "50"));
        let value = get(host, path, &full_query).await?;
        let batch = value.as_array().context("Expected a JSON array")?.clone();
        let count = batch.len();
        rows.extend(batch);
        if count < 50 {
            break;
        }
        page += 1;
    }
    Ok(rows)
}

pub struct Forgejo;

impl RemoteProvider for Forgejo {
    fn snapshot<'a>(&'a self, repo: &'a Repo) -> SnapshotFuture<'a> {
        Box::pin(async move {
            let host = repo.identity.host.as_str();
            let project = repo.identity.project.as_str();
            let mut state = RemoteState::default();

            match get(host, &format!("repos/{project}"), &[]).await {
                Ok(v) => {
                    state.default_branch = match v["default_branch"].as_str() {
                        Some(branch) => Observation::success(branch.to_string()),
                        None => Observation::failure("Missing default branch"),
                    }
                }
                // `Observation::failure` only keeps the top-level Display of
                // an anyhow::Error, which for a `.context(...)`-wrapped error
                // is just the context string. Format with `{e:?}` instead to
                // preserve the full "caused by" chain, including the actual
                // underlying reqwest/transport detail.
                Err(e) => state.default_branch = Observation::failure(format!("{e:?}")),
            }

            let branches_path = format!("repos/{project}/branches");
            let pulls_path = format!("repos/{project}/pulls");
            let issues_path = format!("repos/{project}/issues");
            let releases_path = format!("repos/{project}/releases");
            let (branches, pulls, issues, releases) = tokio::join!(
                paginated(host, &branches_path, &[]),
                paginated(host, &pulls_path, &[("state", "open")]),
                paginated(host, &issues_path, &[("state", "open"), ("type", "issues")]),
                paginated(host, &releases_path, &[]),
            );

            state.branches = match branches {
                Ok(rows) => Observation::success(
                    rows.iter()
                        .map(|v| Item {
                            title: provider::text(v, "name"),
                            detail: v["commit"]["id"].as_str().unwrap_or("?").into(),
                            url: String::new(),
                        })
                        .collect(),
                ),
                Err(e) => Observation::failure(format!("{e:?}")),
            };

            match pulls {
                Ok(rows) => {
                    state.prs = Observation::success(rows.iter().map(provider::pr_item).collect());
                    state.proposals = Observation::success(
                        rows.iter()
                            .filter_map(|v| {
                                provider::release_evidence(v, &repo.release_labels).map(
                                    |evidence| {
                                        let mut item = provider::pr_item(v);
                                        item.detail = format!("{evidence} | {}", item.detail);
                                        item
                                    },
                                )
                            })
                            .collect(),
                    );
                }
                Err(e) => {
                    let message = format!("{e:?}");
                    state.prs = Observation::failure(message.clone());
                    state.proposals = Observation::failure(message);
                }
            }

            state.issues = match issues {
                Ok(rows) => Observation::success(
                    rows.iter()
                        .filter(|v| provider::matches_issue_labels(v, &repo.issue_labels))
                        .map(provider::issue_item)
                        .collect(),
                ),
                Err(e) => Observation::failure(format!("{e:?}")),
            };

            match releases {
                Ok(rows) => {
                    state.drafts = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == true)
                            .map(provider::release_item)
                            .collect(),
                    );
                    state.published = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == false)
                            .map(provider::release_item)
                            .collect(),
                    );
                }
                Err(e) => {
                    let message = format!("{e:?}");
                    state.drafts = Observation::failure(message.clone());
                    state.published = Observation::failure(message);
                }
            }

            // Combined commit-status for the default branch: Forgejo exposes
            // this as a single aggregated call (unlike GitHub's separate
            // check-runs and statuses endpoints), covering both native CI
            // and external status contexts reported against that commit.
            state.ci = if let Some(branch) = &state.default_branch.data {
                let encoded =
                    url::form_urlencoded::byte_serialize(branch.as_bytes()).collect::<String>();
                match get(
                    host,
                    &format!("repos/{project}/commits/{encoded}/status"),
                    &[],
                )
                .await
                {
                    Ok(v) => {
                        let sha = provider::text(&v, "sha");
                        let items = v["statuses"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|s| Item {
                                title: provider::text(s, "context"),
                                url: provider::text(s, "target_url"),
                                detail: format!("{} | {sha}", provider::text(s, "state")),
                            })
                            .collect();
                        Observation::success(items)
                    }
                    Err(e) => Observation::failure(format!("{e:?}")),
                }
            } else {
                Observation::failure("Default branch is unknown")
            };

            state.review_requests = Observation::unsupported();
            state.publication = Observation::unsupported();

            state
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_token_var_uppercases_the_host_and_replaces_non_alphanumerics() {
        assert_eq!(
            env_token_var("codeberg.org"),
            "PHROURION_TOKEN_CODEBERG_ORG"
        );
        assert_eq!(
            env_token_var("forge.example.com:3000"),
            "PHROURION_TOKEN_FORGE_EXAMPLE_COM_3000"
        );
    }

    #[test]
    fn base_url_defaults_to_https_and_honors_the_test_override() {
        // SAFETY: no other test in this binary reads PHROURION_FORGEJO_TEST_URL,
        // so concurrent execution with other tests doesn't race on this var.
        // Rust's test harness still runs #[test] fns on separate threads by
        // default, so save and restore any prior value rather than assuming
        // none exists, for consistency with tests/forgejo_workflow.rs's
        // equivalent handling of PHROURION_TOKEN_LOCALHOST.
        let previous = std::env::var("PHROURION_FORGEJO_TEST_URL").ok();
        unsafe {
            std::env::remove_var("PHROURION_FORGEJO_TEST_URL");
        }
        assert_eq!(base_url("codeberg.org"), "https://codeberg.org/api/v1");
        unsafe {
            std::env::set_var("PHROURION_FORGEJO_TEST_URL", "http://localhost:3000");
        }
        assert_eq!(base_url("codeberg.org"), "http://localhost:3000/api/v1");
        unsafe {
            match &previous {
                Some(value) => std::env::set_var("PHROURION_FORGEJO_TEST_URL", value),
                None => std::env::remove_var("PHROURION_FORGEJO_TEST_URL"),
            }
        }
    }
}
