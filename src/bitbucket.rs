use crate::{
    model::{Item, Observation, RemoteState, clean},
    provider,
    provider::{RemoteProvider, SnapshotFuture},
    provider_http::{Auth, HttpClient},
};
use anyhow::{Context, Result, bail};
use serde_json::Value;

const DEFAULT_BASE_URL: &str = "https://api.bitbucket.org/2.0/";
const TOKEN_ENV: &str = "PHROURION_BITBUCKET_TOKEN";
const BASE_URL_ENV: &str = "PHROURION_BITBUCKET_BASE_URL";

#[cfg(test)]
pub(crate) static BITBUCKET_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct Bitbucket;

fn http_client() -> Result<HttpClient> {
    let base_url = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.into());
    let auth = std::env::var(TOKEN_ENV)
        .ok()
        .filter(|token| !token.is_empty())
        .map(Auth::Bearer)
        .unwrap_or(Auth::None);
    HttpClient::new(&base_url, auth)
}

fn encoded_project(project: &str) -> Result<String> {
    let mut parts = project.split('/');
    let workspace = parts.next().filter(|value| !value.is_empty());
    let repository = parts.next().filter(|value| !value.is_empty());
    if workspace.is_none() || repository.is_none() || parts.next().is_some() {
        bail!("Bitbucket project must be workspace/repository")
    }
    Ok(project
        .split('/')
        .map(|part| url::form_urlencoded::byte_serialize(part.as_bytes()).collect::<String>())
        .collect::<Vec<_>>()
        .join("/"))
}

async fn paginated(client: &HttpClient, path: &str) -> Result<Value> {
    Ok(Value::Array(
        client
            .paginate_json(path, |page| {
                let values = page
                    .get("values")
                    .and_then(Value::as_array)
                    .context("Missing Bitbucket values array")?
                    .clone();
                let next = page.get("next").and_then(Value::as_str).map(str::to_owned);
                Ok((values, next))
            })
            .await?,
    ))
}

fn failed_snapshot(error: anyhow::Error) -> RemoteState {
    let error = error.to_string();
    RemoteState {
        default_branch: Observation::failure(&error),
        branches: Observation::failure(&error),
        prs: Observation::failure(&error),
        review_requests: Observation::failure(&error),
        issues: Observation::failure(&error),
        proposals: Observation::failure(&error),
        drafts: Observation::failure(&error),
        published: Observation::unsupported(),
        ci: Observation::failure(error),
        publication: Observation::unsupported(),
    }
}

fn observe_response<T>(
    response: Result<Value>,
    mapper: impl FnOnce(&Value) -> Result<T>,
) -> Observation<T> {
    match response.and_then(|value| mapper(&value)) {
        Ok(value) => Observation::success(value),
        Err(error) => Observation::failure(error.to_string()),
    }
}

fn map_http_snapshot(
    repository: Result<Value>,
    branches: Result<Value>,
    pull_requests: Result<Value>,
    reviewers: Result<Value>,
    pipelines: Result<Value>,
) -> RemoteState {
    let (prs, drafts) = match &pull_requests {
        Ok(value) => match map_pull_requests(value) {
            Ok(mapped) => (
                Observation::success(mapped.prs),
                Observation::success(mapped.drafts),
            ),
            Err(error) => {
                let error = error.to_string();
                (Observation::failure(&error), Observation::failure(error))
            }
        },
        Err(error) => {
            let error = error.to_string();
            (Observation::failure(&error), Observation::failure(error))
        }
    };
    let review_requests = match (pull_requests, reviewers) {
        (Ok(pull_requests), Ok(reviewers)) => {
            match matching_reviewer_requests(&pull_requests, &reviewers) {
                Ok(items) => Observation::success(items),
                Err(error) => Observation::failure(error.to_string()),
            }
        }
        (Err(error), _) | (_, Err(error)) => Observation::failure(error.to_string()),
    };
    RemoteState {
        default_branch: observe_response(repository, map_repository),
        branches: observe_response(branches, map_branches),
        prs,
        review_requests,
        issues: Observation::unsupported(),
        proposals: Observation::success(Vec::new()),
        drafts,
        published: Observation::unsupported(),
        ci: observe_response(pipelines, map_pipelines),
        publication: Observation::unsupported(),
    }
}

impl RemoteProvider for Bitbucket {
    fn snapshot<'a>(&'a self, repo: &'a crate::model::Repo) -> SnapshotFuture<'a> {
        Box::pin(async move {
            let client = match http_client() {
                Ok(client) => client,
                Err(error) => return failed_snapshot(error),
            };
            let project = match encoded_project(&repo.identity.project) {
                Ok(project) => project,
                Err(error) => return failed_snapshot(error),
            };
            let repository_path = format!("repositories/{project}");
            let branches_path = format!("{repository_path}/refs/branches");
            let pull_requests_path =
                format!("{repository_path}/pullrequests?state=OPEN&fields=%2Bvalues.reviewers");
            let pipelines_path = format!("{repository_path}/pipelines");
            let (repository, branches, pull_requests, reviewers, pipelines) = tokio::join!(
                client.get_json_value(&repository_path),
                paginated(&client, &branches_path),
                paginated(&client, &pull_requests_path),
                client.get_json_value("user"),
                paginated(&client, &pipelines_path),
            );
            map_http_snapshot(repository, branches, pull_requests, reviewers, pipelines)
        })
    }
}

fn text(value: &Value, key: &str) -> String {
    clean(&provider::text(value, key))
}

fn nested_text(value: &Value, path: &[&str]) -> String {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
        .and_then(Value::as_str)
        .map(clean)
        .unwrap_or_default()
}

fn identifier(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| match value {
            Value::Number(number) => Some(number.to_string()),
            Value::String(text) => Some(clean(text)),
            _ => None,
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "?".into())
}

fn rows<'a>(value: &'a Value, endpoint: &str) -> Result<&'a [Value]> {
    value
        .get("values")
        .and_then(Value::as_array)
        .or_else(|| value.as_array())
        .map(Vec::as_slice)
        .with_context(|| format!("Expected Bitbucket {endpoint} values array"))
}

fn reviewer_rows(value: &Value) -> Result<Vec<&Value>> {
    match value {
        Value::Array(values) => Ok(values.iter().collect()),
        Value::Object(_) => Ok(vec![value]),
        _ => Err(anyhow::anyhow!(
            "Expected Bitbucket reviewers array or object"
        )),
    }
}

fn link(value: &Value) -> String {
    nested_text(value, &["links", "html", "href"])
}

pub(crate) fn map_repository(value: &Value) -> Result<String> {
    value
        .get("mainbranch")
        .and_then(|branch| branch.get("name"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(clean)
        .context("Missing Bitbucket default branch")
}

pub(crate) fn map_branches(value: &Value) -> Result<Vec<Item>> {
    Ok(rows(value, "branches")?
        .iter()
        .map(|branch| Item {
            title: text(branch, "name"),
            url: link(branch),
            detail: nested_text(branch, &["target", "hash"]),
        })
        .map(|mut item| {
            if item.detail.is_empty() {
                item.detail = "?".into();
            }
            item
        })
        .collect())
}

struct PullRequestMappings {
    prs: Vec<Item>,
    drafts: Vec<Item>,
}

fn reviewer_name(value: &Value) -> String {
    let nickname = text(value, "nickname");
    let display_name = text(value, "display_name");
    match (nickname.is_empty(), display_name.is_empty()) {
        (false, false) => format!("{nickname} ({display_name})"),
        (false, true) => nickname,
        (true, false) => display_name,
        (true, true) => "?".into(),
    }
}

fn reviewer_key(value: &Value) -> Option<String> {
    value
        .get("uuid")
        .and_then(Value::as_str)
        .or_else(|| value.get("nickname").and_then(Value::as_str))
        .map(clean)
        .filter(|key| !key.is_empty())
}

fn map_pull_requests(value: &Value) -> Result<PullRequestMappings> {
    let mut prs = Vec::new();
    let mut drafts = Vec::new();
    for request in rows(value, "pull requests")? {
        let reviewers = request["reviewers"].as_array().cloned().unwrap_or_default();
        let reviewer_names = if reviewers.is_empty() {
            "none".into()
        } else {
            reviewers
                .iter()
                .map(reviewer_name)
                .collect::<Vec<_>>()
                .join(", ")
        };
        let item = Item {
            title: format!("#{} {}", identifier(request, "id"), text(request, "title")),
            url: link(request),
            detail: format!(
                "{} -> {} | head {} | {} | reviewers {reviewer_names}",
                nested_text(request, &["source", "branch", "name"]),
                nested_text(request, &["destination", "branch", "name"]),
                nested_text(request, &["source", "commit", "hash"]),
                if request["draft"] == true {
                    "draft"
                } else {
                    "open"
                },
            ),
        };
        if request["draft"] == true {
            drafts.push(item.clone());
        }
        prs.push(item);
    }
    Ok(PullRequestMappings { prs, drafts })
}

pub(crate) fn map_pipelines(value: &Value) -> Result<Vec<Item>> {
    Ok(rows(value, "pipelines")?
        .iter()
        .map(|pipeline| Item {
            title: format!("pipeline #{}", identifier(pipeline, "build_number")),
            url: link(pipeline),
            detail: format!(
                "{} {} | {} | {}",
                nested_text(pipeline, &["state", "name"]),
                nested_text(pipeline, &["state", "result", "name"]),
                nested_text(pipeline, &["target", "ref_name"]),
                nested_text(pipeline, &["target", "commit", "hash"]),
            )
            .trim()
            .to_string(),
        })
        .collect())
}

fn matching_reviewer_requests(value: &Value, reviewers: &Value) -> Result<Vec<Item>> {
    let keys: Vec<_> = reviewer_rows(reviewers)?
        .iter()
        .filter_map(|reviewer| reviewer_key(reviewer))
        .collect();
    Ok(rows(value, "pull requests")?
        .iter()
        .filter(|request| {
            request["reviewers"].as_array().is_some_and(|assigned| {
                assigned
                    .iter()
                    .filter_map(reviewer_key)
                    .any(|key| keys.contains(&key))
            })
        })
        .map(|request| Item {
            title: format!("#{} {}", identifier(request, "id"), text(request, "title")),
            url: link(request),
            detail: nested_text(request, &["state"]),
        })
        .collect())
}

pub(crate) fn map_snapshot(
    repository: &Value,
    branches: &Value,
    pull_requests: &Value,
    reviewers: &Value,
    pipelines: &Value,
) -> RemoteState {
    let mappings = map_pull_requests(pull_requests);
    let (prs, drafts) = match mappings {
        Ok(mappings) => (
            Observation::success(mappings.prs),
            Observation::success(mappings.drafts),
        ),
        Err(error) => (
            Observation::failure(error.to_string()),
            Observation::failure(error.to_string()),
        ),
    };
    let review_requests = match matching_reviewer_requests(pull_requests, reviewers) {
        Ok(items) => Observation::success(items),
        Err(error) => Observation::failure(error.to_string()),
    };
    RemoteState {
        default_branch: match map_repository(repository) {
            Ok(branch) => Observation::success(branch),
            Err(error) => Observation::failure(error.to_string()),
        },
        branches: match map_branches(branches) {
            Ok(items) => Observation::success(items),
            Err(error) => Observation::failure(error.to_string()),
        },
        prs,
        review_requests,
        issues: Observation::unsupported(),
        proposals: Observation::success(Vec::new()),
        drafts,
        published: Observation::unsupported(),
        ci: match map_pipelines(pipelines) {
            Ok(items) => Observation::success(items),
            Err(error) => Observation::failure(error.to_string()),
        },
        publication: Observation::unsupported(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{ProviderKind, Remote, Repo},
        provider::RemoteProvider,
        test_support::{Request, Response, fixture, response, test_server},
    };
    use std::sync::{Arc, Mutex};

    struct EnvVar {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVar {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    fn test_repo() -> Repo {
        Repo {
            id: "bitbucket".into(),
            name: "widgets".into(),
            path: std::env::temp_dir(),
            remote: "origin".into(),
            identity: Remote {
                kind: ProviderKind::Bitbucket,
                host: "bitbucket.org".into(),
                project: "acme/widgets".into(),
            },
            enabled: true,
            release_workflows: vec![],
            release_labels: vec![],
            issue_labels: vec![],
            workspaces: vec![],
        }
    }

    fn request_target(request: &Request) -> String {
        match request.query() {
            Some(query) => format!("{}?{query}", request.url_path()),
            None => request.url_path().to_string(),
        }
    }

    fn fixture_response(request: &Request) -> Response {
        let fixture_path = match request.url_path() {
            "/2.0/repositories/acme/widgets" => "tests/fixtures/bitbucket/repository.json",
            "/2.0/repositories/acme/widgets/refs/branches" => {
                "tests/fixtures/bitbucket/branches.json"
            }
            "/2.0/repositories/acme/widgets/pullrequests" => {
                "tests/fixtures/bitbucket/pull_requests.json"
            }
            "/2.0/repositories/acme/widgets/pipelines" => "tests/fixtures/bitbucket/pipelines.json",
            "/2.0/user" => "tests/fixtures/bitbucket/reviewers.json",
            _ => return response(404, r#"{"message":"unexpected endpoint"}"#),
        };
        response(200, &fixture(fixture_path).to_string())
    }

    #[test]
    fn bitbucket_fixtures_map_the_neutral_state() {
        let state = map_snapshot(
            &fixture("tests/fixtures/bitbucket/repository.json"),
            &fixture("tests/fixtures/bitbucket/branches.json"),
            &fixture("tests/fixtures/bitbucket/pull_requests.json"),
            &fixture("tests/fixtures/bitbucket/reviewers.json"),
            &fixture("tests/fixtures/bitbucket/pipelines.json"),
        );
        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.branches.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.review_requests.data.as_ref().map(Vec::len), Some(1));
        assert!(!state.issues.supported);
        assert_eq!(state.drafts.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.ci.data.as_ref().map(Vec::len), Some(1));
        assert!(!state.published.supported);
        assert!(!state.publication.supported);
    }

    #[test]
    fn bitbucket_items_preserve_urls_and_use_safe_optional_values() {
        let branches = map_branches(&fixture("tests/fixtures/bitbucket/branches.json")).unwrap();
        assert_eq!(branches[0].title, "main");
        assert_eq!(
            branches[0].detail,
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
        let prs =
            map_pull_requests(&fixture("tests/fixtures/bitbucket/pull_requests.json")).unwrap();
        assert_eq!(prs.prs[0].title, "#7 Add widget support");
        assert!(prs.prs[0].detail.contains("feature/widget -> main"));
        assert!(prs.prs[0].detail.contains("reviewer (Rhea Viewer)"));
        assert_eq!(prs.drafts[0].title, "#8 Try alternate widget spacing");
    }

    #[test]
    fn malformed_or_missing_optional_values_do_not_panic() {
        let value = serde_json::json!({"values": [{}]});
        let branches = map_branches(&value).unwrap();
        assert_eq!(branches[0].title, "");
        assert_eq!(branches[0].detail, "?");
    }

    #[test]
    fn invalid_collection_shapes_are_reported() {
        let error = map_branches(&serde_json::json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Bitbucket branches"));
    }

    #[tokio::test]
    async fn http_snapshot_fetches_cloud_endpoints_with_bearer_auth() {
        let _lock = BITBUCKET_ENV_LOCK.lock().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let server = test_server(move |request| {
            handler_seen.lock().unwrap().push((
                request_target(request),
                request.header("authorization").map(str::to_string),
            ));
            fixture_response(request)
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/2.0/", server.root_url()));
        let _token = EnvVar::set(TOKEN_ENV, "bitbucket-test-token");

        let state = Bitbucket.snapshot(&test_repo()).await;

        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.review_requests.data.as_ref().map(Vec::len), Some(1));
        assert!(!state.issues.supported);
        assert_eq!(state.ci.data.as_ref().map(Vec::len), Some(1));
        assert!(!state.published.supported);

        let actual = seen.lock().unwrap().clone();
        assert_eq!(actual.len(), 5);
        assert!(
            actual
                .iter()
                .all(|(_, auth)| { auth.as_deref() == Some("Bearer bitbucket-test-token") })
        );
        assert!(actual.iter().any(|(target, _)| {
            target == "/2.0/repositories/acme/widgets/pullrequests?state=OPEN&fields=%2Bvalues.reviewers"
        }));
    }

    #[tokio::test]
    async fn http_snapshot_preserves_siblings_when_an_endpoint_fails() {
        let _lock = BITBUCKET_ENV_LOCK.lock().await;
        let server = test_server(|request| {
            if request.query() == Some("page=2") {
                return response(
                    200,
                    r#"{"values":[{"name":"feature","target":{"hash":"b"}}]}"#,
                );
            }
            if request.url_path().ends_with("/refs/branches") {
                return response(
                    200,
                    r#"{"values":[{"name":"main","target":{"hash":"a"}}],"next":"/2.0/repositories/acme/widgets/refs/branches?page=2"}"#,
                );
            }
            fixture_response(request)
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/2.0/", server.root_url()));
        let _token = EnvVar::set(TOKEN_ENV, "bitbucket-test-token");

        let state = Bitbucket.snapshot(&test_repo()).await;

        assert_eq!(state.branches.data.as_ref().map(Vec::len), Some(2));
        assert!(!state.issues.supported);
        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
    }

    #[tokio::test]
    async fn http_snapshot_without_token_does_not_send_authorization() {
        let _lock = BITBUCKET_ENV_LOCK.lock().await;
        let saw_authorization = Arc::new(Mutex::new(false));
        let handler_saw_authorization = Arc::clone(&saw_authorization);
        let server = test_server(move |request| {
            if request.header("authorization").is_some() {
                *handler_saw_authorization.lock().unwrap() = true;
            }
            if request.url_path() == "/2.0/user" {
                response(401, r#"{"message":"unauthorized"}"#)
            } else {
                fixture_response(request)
            }
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/2.0/", server.root_url()));
        let _token = EnvVar::remove(TOKEN_ENV);

        let state = Bitbucket.snapshot(&test_repo()).await;

        assert!(!*saw_authorization.lock().unwrap());
        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert!(state.review_requests.error.is_some());
    }
}
