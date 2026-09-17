//! Forgejo/Gitea-compatible hosting adapter. HTTP-based: no gh-equivalent CLI
//! exists for Forgejo, so this talks to the REST API directly.

use crate::{
    model::{Item, Observation, RemoteState, Repo},
    provider::{self, RemoteProvider, SnapshotFuture},
};
use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::{fmt, sync::OnceLock, time::Duration};

// Carries the HTTP status alongside the message so callers can distinguish a
// disabled repo feature (404 on /pulls, /releases, /issues) from a real
// failure, without parsing the Display string back apart.
#[derive(Debug)]
struct HttpError {
    status: StatusCode,
    message: String,
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.status, self.message)
    }
}

impl std::error::Error for HttpError {}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<HttpError>()
        .is_some_and(|e| e.status == StatusCode::NOT_FOUND)
}

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
    Err(HttpError { status, message }.into())
}

// `Ok(None)` means the endpoint 404'd on the first page — Forgejo/Gitea
// returns that for a repo feature the owner has turned off (e.g.
// has_pull_requests/has_releases/has_issues: false), not for "no items
// exist" (that's an empty `Ok(Some(vec![]))`). Only checked on page 1: a
// 404 after a successful earlier page would be a real, separate failure,
// not a disabled-feature signal.
pub(crate) async fn paginated(
    host: &str,
    path: &str,
    query: &[(&str, &str)],
) -> Result<Option<Vec<Value>>> {
    let mut rows = Vec::new();
    let mut page: u32 = 1;
    loop {
        let page_str = page.to_string();
        let mut full_query: Vec<(&str, &str)> = query.to_vec();
        full_query.push(("page", &page_str));
        full_query.push(("limit", "50"));
        let value = match get(host, path, &full_query).await {
            Ok(v) => v,
            Err(e) if page == 1 && is_not_found(&e) => return Ok(None),
            Err(e) => return Err(e),
        };
        let batch = value.as_array().context("Expected a JSON array")?.clone();
        let count = batch.len();
        rows.extend(batch);
        if count < 50 {
            break;
        }
        page += 1;
    }
    Ok(Some(rows))
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
                Ok(Some(rows)) => Observation::success(
                    rows.iter()
                        .map(|v| Item {
                            title: provider::text(v, "name"),
                            detail: v["commit"]["id"].as_str().unwrap_or("?").into(),
                            url: String::new(),
                        })
                        .collect(),
                ),
                Ok(None) => Observation::unsupported(),
                Err(e) => Observation::failure(format!("{e:?}")),
            };

            match pulls {
                Ok(Some(rows)) => {
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
                // Pull requests are a repo feature Forgejo/Gitea lets an owner
                // turn off (has_pull_requests: false) — the API 404s the
                // endpoint entirely rather than returning an empty list.
                Ok(None) => {
                    state.prs = Observation::unsupported();
                    state.proposals = Observation::unsupported();
                }
                Err(e) => {
                    let message = format!("{e:?}");
                    state.prs = Observation::failure(message.clone());
                    state.proposals = Observation::failure(message);
                }
            }

            state.issues = match issues {
                Ok(Some(rows)) => Observation::success(
                    rows.iter()
                        .filter(|v| provider::matches_issue_labels(v, &repo.issue_labels))
                        .map(provider::issue_item)
                        .collect(),
                ),
                // Issues are also a togglable repo feature (has_issues: false).
                Ok(None) => Observation::unsupported(),
                Err(e) => Observation::failure(format!("{e:?}")),
            };

            match releases {
                Ok(Some(rows)) => {
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
                // Releases are also togglable (has_releases: false).
                Ok(None) => {
                    state.drafts = Observation::unsupported();
                    state.published = Observation::unsupported();
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
    use std::io::{Read, Write};

    // Rust's test harness runs #[test]/#[tokio::test] fns concurrently on
    // separate threads by default, but PHROURION_FORGEJO_TEST_URL and any
    // PHROURION_TOKEN_* var are process-global. Every test below that reads
    // or writes one of those vars holds this lock for its whole body so no
    // two such tests interleave (mirrors tests/forgejo_workflow.rs's
    // LIVE_TEST_LOCK for the same reason). Shared with src/provider.rs's
    // tests, which also set PHROURION_FORGEJO_TEST_URL.
    use crate::test_support::FORGEJO_ENV_LOCK as ENV_LOCK;

    struct MockResponse {
        status: u16,
        body: String,
    }

    // Serves one canned response per accepted connection, in order, on a
    // background thread. Returns the server's base URL and a channel that
    // yields each request's raw bytes (headers included) as it arrives, so
    // a test can assert on what was actually sent (e.g. an Authorization
    // header) without a mocking dependency.
    fn serve_mock(responses: Vec<MockResponse>) -> (String, std::sync::mpsc::Receiver<Vec<u8>>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for resp in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 8192];
                let mut request = Vec::new();
                loop {
                    let n = stream.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let _ = tx.send(request);
                let reason = match resp.status {
                    200 => "OK",
                    404 => "Not Found",
                    _ => "Error",
                };
                let payload = format!(
                    "HTTP/1.1 {} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    resp.status,
                    resp.body.len(),
                    resp.body
                );
                let _ = stream.write_all(payload.as_bytes());
                let _ = stream.flush();
            }
        });
        (format!("http://{addr}"), rx)
    }

    // Caller must hold ENV_LOCK for the duration of this call. Awaits `fut`
    // (not merely constructs it) before restoring the previous value, so the
    // override is actually in effect while the request runs.
    async fn with_test_url<T>(url: &str, fut: impl std::future::Future<Output = T>) -> T {
        let previous = std::env::var("PHROURION_FORGEJO_TEST_URL").ok();
        unsafe {
            std::env::set_var("PHROURION_FORGEJO_TEST_URL", url);
        }
        let result = fut.await;
        unsafe {
            match &previous {
                Some(v) => std::env::set_var("PHROURION_FORGEJO_TEST_URL", v),
                None => std::env::remove_var("PHROURION_FORGEJO_TEST_URL"),
            }
        }
        result
    }

    #[test]
    fn client_is_cached_across_calls() {
        assert!(std::ptr::eq(client(), client()));
    }

    #[test]
    fn http_error_display_includes_status_and_message() {
        let err = HttpError {
            status: StatusCode::NOT_FOUND,
            message: "nope".into(),
        };
        assert_eq!(
            err.to_string(),
            format!("{}: {}", StatusCode::NOT_FOUND, "nope")
        );
    }

    #[tokio::test]
    async fn get_returns_parsed_json_on_success_via_a_local_mock_server() {
        let _guard = ENV_LOCK.lock().await;
        let (base, _rx) = serve_mock(vec![MockResponse {
            status: 200,
            body: r#"{"hello":"world"}"#.into(),
        }]);
        let value = with_test_url(&base, get("mock", "anything", &[]))
            .await
            .expect("mock server returned 200 with valid JSON");
        assert_eq!(value["hello"], "world");
    }

    #[tokio::test]
    async fn paginated_collects_a_single_short_page_via_a_local_mock_server() {
        let _guard = ENV_LOCK.lock().await;
        let (base, _rx) = serve_mock(vec![MockResponse {
            status: 200,
            body: r#"[{"id":1},{"id":2}]"#.into(),
        }]);
        let rows = with_test_url(&base, paginated("mock", "items", &[]))
            .await
            .expect("page should parse")
            .expect("feature is not reported disabled");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["id"], 1);
        assert_eq!(rows[1]["id"], 2);
    }

    #[tokio::test]
    async fn paginated_returns_none_when_the_first_page_404s() {
        let _guard = ENV_LOCK.lock().await;
        let (base, _rx) = serve_mock(vec![MockResponse {
            status: 404,
            body: r#"{"message":"Not Found"}"#.into(),
        }]);
        let result = with_test_url(&base, paginated("mock", "disabled-feature", &[])).await;
        assert!(result.unwrap().is_none());
    }

    #[tokio::test]
    async fn paginated_propagates_a_404_on_a_later_page_as_a_real_error() {
        let _guard = ENV_LOCK.lock().await;
        // A full 50-item first page forces a second request; the 404 there
        // must NOT be read as "feature disabled" (that's only true on page
        // 1) — it's a genuine failure partway through pagination.
        let full_page: String = format!(
            "[{}]",
            (0..50)
                .map(|i| format!(r#"{{"id":{i}}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let (base, _rx) = serve_mock(vec![
            MockResponse {
                status: 200,
                body: full_page,
            },
            MockResponse {
                status: 404,
                body: r#"{"message":"Not Found"}"#.into(),
            },
        ]);
        let result = with_test_url(&base, paginated("mock", "items", &[])).await;
        assert!(
            result.is_err(),
            "a 404 past page 1 should propagate as Err, not Ok(None)"
        );
    }

    #[tokio::test]
    async fn paginated_continues_past_a_full_page_and_advances_the_page_number() {
        let _guard = ENV_LOCK.lock().await;
        let full_page: String = format!(
            "[{}]",
            (0..50)
                .map(|i| format!(r#"{{"id":{i}}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        let (base, rx) = serve_mock(vec![
            MockResponse {
                status: 200,
                body: full_page,
            },
            MockResponse {
                status: 200,
                body: r#"[{"id":9001},{"id":9002},{"id":9003}]"#.into(),
            },
        ]);
        let rows = with_test_url(&base, paginated("mock", "items", &[]))
            .await
            .expect("both pages should parse")
            .expect("feature is not reported disabled");

        // Exactly 50 on page 1 must NOT be treated as the short/last page
        // (`< 50`, not `<= 50`) — it has to fetch page 2 and append it.
        assert_eq!(
            rows.len(),
            53,
            "a full 50-item page must fetch another page, not stop at exactly 50"
        );
        assert_eq!(rows[50]["id"], 9001);

        let _first_request = rx.recv().unwrap();
        let second_request = String::from_utf8_lossy(&rx.recv().unwrap()).to_lowercase();
        assert!(
            second_request.contains("page=2"),
            "expected the second request to ask for page 2: {second_request}"
        );
    }

    #[tokio::test]
    async fn get_sends_the_bearer_token_only_when_it_is_non_empty() {
        let _guard = ENV_LOCK.lock().await;
        let host = "mock";
        let var = env_token_var(host);
        let previous_token = std::env::var(&var).ok();
        let (base, rx) = serve_mock(vec![
            MockResponse {
                status: 200,
                body: "{}".into(),
            },
            MockResponse {
                status: 200,
                body: "{}".into(),
            },
        ]);

        unsafe {
            std::env::set_var(&var, "s3cr3t");
        }
        with_test_url(&base, get(host, "with-token", &[]))
            .await
            .unwrap();
        let with_token = String::from_utf8_lossy(&rx.recv().unwrap()).to_lowercase();

        unsafe {
            std::env::set_var(&var, "");
        }
        with_test_url(&base, get(host, "without-token", &[]))
            .await
            .unwrap();
        let without_token = String::from_utf8_lossy(&rx.recv().unwrap()).to_lowercase();

        unsafe {
            match &previous_token {
                Some(v) => std::env::set_var(&var, v),
                None => std::env::remove_var(&var),
            }
        }

        assert!(with_token.contains("authorization: token s3cr3t"));
        assert!(!without_token.contains("authorization"));
    }

    #[test]
    fn not_found_is_detected_only_for_a_404_http_error() {
        let not_found: anyhow::Error = HttpError {
            status: StatusCode::NOT_FOUND,
            message: "The target couldn't be found.".into(),
        }
        .into();
        assert!(is_not_found(&not_found));

        let server_error: anyhow::Error = HttpError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "boom".into(),
        }
        .into();
        assert!(!is_not_found(&server_error));

        let other: anyhow::Error = anyhow::anyhow!("Forgejo request failed");
        assert!(!is_not_found(&other));
    }

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

    #[tokio::test]
    async fn base_url_defaults_to_https_and_honors_the_test_override() {
        // Rust's test harness still runs #[test]/#[tokio::test] fns on
        // separate threads by default, and every other test in this module
        // that touches PHROURION_FORGEJO_TEST_URL holds ENV_LOCK for its
        // whole body — so this one must too, or the two race.
        let _guard = ENV_LOCK.lock().await;
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
