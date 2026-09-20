use crate::{
    model::{Item, Observation, RemoteState, Repo, clean},
    provider::{self, RemoteProvider, SnapshotFuture},
    provider_http::{Auth, HttpClient},
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashSet;

const DEFAULT_BASE_URL: &str = "https://gitlab.com/api/v4/";
const TOKEN_ENV: &str = "PHROURION_GITLAB_TOKEN";
const BASE_URL_ENV: &str = "PHROURION_GITLAB_BASE_URL";
const PAGE_SIZE: usize = 100;
const MAX_PAGES: usize = 1000;

#[cfg(test)]
pub(crate) static GITLAB_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct Gitlab;

fn http_client() -> Result<HttpClient> {
    let base_url = std::env::var(BASE_URL_ENV).unwrap_or_else(|_| DEFAULT_BASE_URL.into());
    let auth = std::env::var(TOKEN_ENV)
        .ok()
        .filter(|token| !token.is_empty())
        .map(Auth::Bearer)
        .unwrap_or(Auth::None);
    HttpClient::new(&base_url, auth)
}

fn encoded_project(project: &str) -> String {
    url::form_urlencoded::byte_serialize(project.as_bytes()).collect()
}

fn paginated_endpoint(path: &str, query: &[(&str, &str)], page: usize) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    serializer.extend_pairs(query.iter().copied());
    serializer.append_pair("per_page", &PAGE_SIZE.to_string());
    serializer.append_pair("page", &page.to_string());
    format!("{path}?{}", serializer.finish())
}

async fn paginated(client: &HttpClient, path: &str, query: &[(&str, &str)]) -> Result<Value> {
    let mut rows = Vec::new();
    let mut page = 1;
    loop {
        if page > MAX_PAGES {
            bail!("GitLab pagination exceeded {MAX_PAGES} pages for endpoint '{path}'");
        }
        let endpoint = paginated_endpoint(path, query, page);
        let value = client.get_json_value(&endpoint).await?;
        let batch = value
            .as_array()
            .with_context(|| format!("Expected GitLab array from endpoint '{endpoint}'"))?;
        let count = batch.len();
        rows.extend(batch.iter().cloned());
        if count < PAGE_SIZE {
            return Ok(Value::Array(rows));
        }
        page += 1;
    }
}

pub(crate) fn map_project(value: &Value) -> Result<String> {
    required_text(
        value,
        "path_with_namespace",
        "Missing GitLab project identity",
    )?;
    let default_branch = required_text(value, "default_branch", "Missing GitLab default branch")?;
    Ok(default_branch.to_string())
}

fn required_text<'a>(value: &'a Value, key: &str, message: &str) -> Result<&'a str> {
    match value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        Some(text) => Ok(text),
        None => bail!(message.to_string()),
    }
}

fn rows<'a>(value: &'a Value, endpoint: &str) -> Result<&'a [Value]> {
    value
        .as_array()
        .map(Vec::as_slice)
        .with_context(|| format!("Expected GitLab {endpoint} array"))
}

fn text(value: &Value, key: &str) -> String {
    clean(&provider::text(value, key))
}

fn identifier(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(|value| match value {
            Value::Number(number) => Some(number.to_string()),
            Value::String(text) => Some(clean(text)),
            _ => None,
        })
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "?".into())
}

fn labels(value: &Value) -> Vec<String> {
    value["labels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(clean)
        .collect()
}

pub(crate) fn map_branches(value: &Value) -> Result<Vec<Item>> {
    Ok(rows(value, "branches")?
        .iter()
        .map(|branch| Item {
            title: text(branch, "name"),
            url: text(branch, "web_url"),
            detail: branch["commit"]["id"]
                .as_str()
                .map(clean)
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| "?".into()),
        })
        .collect())
}

fn reviewer_name(value: &Value) -> String {
    let username = text(value, "username");
    let name = text(value, "name");
    match (username.is_empty(), name.is_empty()) {
        (false, false) => format!("{username} ({name})"),
        (false, true) => username,
        (true, false) => name,
        (true, true) => "?".into(),
    }
}

fn merge_request_item(value: &Value) -> Item {
    let labels = labels(value);
    let labels = if labels.is_empty() {
        "none".into()
    } else {
        labels.join(", ")
    };
    let reviewers: Vec<_> = value["reviewers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(reviewer_name)
        .collect();
    let reviewers = if reviewers.is_empty() {
        "none".into()
    } else {
        reviewers.join(", ")
    };
    Item {
        title: format!("!{} {}", identifier(value, "iid"), text(value, "title")),
        url: text(value, "web_url"),
        detail: format!(
            "{} -> {} | head {} | {} | labels {labels} | reviewers {reviewers}",
            optional_text(value, "source_branch", "?"),
            optional_text(value, "target_branch", "?"),
            optional_text(value, "sha", "?"),
            if value["draft"] == true {
                "draft"
            } else {
                "open"
            },
        ),
    }
}

fn optional_text(value: &Value, key: &str, fallback: &str) -> String {
    let value = text(value, key);
    if value.is_empty() {
        fallback.into()
    } else {
        value
    }
}

fn release_evidence(value: &Value, configured_labels: &[String]) -> Option<&'static str> {
    let labels = labels(value);
    if labels.iter().any(|label| {
        label == "autorelease: pending" || configured_labels.iter().any(|wanted| wanted == label)
    }) {
        return Some("release label");
    }
    if value["source_branch"]
        .as_str()
        .unwrap_or_default()
        .starts_with("release-please--")
    {
        return Some("Release Please branch");
    }
    let title = text(value, "title").to_lowercase();
    if title.starts_with("chore(main): release")
        || title.starts_with("chore: release")
        || title.starts_with("release:")
    {
        return Some("candidate: title only");
    }
    None
}

fn reviewer_keys(value: &Value) -> Result<HashSet<String>> {
    let reviewers: Vec<_> = match value {
        Value::Array(reviewers) => reviewers.iter().collect(),
        Value::Object(_) => vec![value],
        _ => bail!("Expected GitLab reviewer object or array"),
    };
    Ok(reviewers
        .into_iter()
        .flat_map(|reviewer| {
            [
                reviewer.get("id").map(Value::to_string),
                reviewer.get("username").and_then(Value::as_str).map(clean),
            ]
        })
        .flatten()
        .filter(|key| !key.is_empty())
        .collect())
}

fn has_matching_reviewer(value: &Value, reviewer_keys: &HashSet<String>) -> bool {
    value["reviewers"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|reviewer| {
            reviewer
                .get("id")
                .map(Value::to_string)
                .is_some_and(|id| reviewer_keys.contains(&id))
                || reviewer
                    .get("username")
                    .and_then(Value::as_str)
                    .is_some_and(|username| reviewer_keys.contains(username))
        })
}

struct MergeRequestMappings {
    prs: Vec<Item>,
    proposals: Vec<Item>,
    drafts: Vec<Item>,
}

fn map_merge_requests(value: &Value, release_labels: &[String]) -> Result<MergeRequestMappings> {
    let mut mapped = MergeRequestMappings {
        prs: Vec::new(),
        proposals: Vec::new(),
        drafts: Vec::new(),
    };
    for merge_request in rows(value, "merge requests")?.iter().filter(|value| {
        value["state"]
            .as_str()
            .is_none_or(|state| state == "opened")
    }) {
        let item = merge_request_item(merge_request);
        mapped.prs.push(item.clone());
        if merge_request["draft"] == true {
            mapped.drafts.push(item.clone());
        }
        if let Some(evidence) = release_evidence(merge_request, release_labels) {
            let mut proposal = item;
            proposal.detail = format!("{evidence} | {}", proposal.detail);
            mapped.proposals.push(proposal);
        }
    }
    Ok(mapped)
}

fn map_review_requests(value: &Value, reviewer: &Value) -> Result<Vec<Item>> {
    let reviewer_keys = reviewer_keys(reviewer)?;
    Ok(rows(value, "merge requests")?
        .iter()
        .filter(|value| {
            value["state"]
                .as_str()
                .is_none_or(|state| state == "opened")
        })
        .filter(|merge_request| has_matching_reviewer(merge_request, &reviewer_keys))
        .map(merge_request_item)
        .collect())
}

fn matches_labels(value: &Value, configured_labels: &[String]) -> bool {
    configured_labels.is_empty()
        || labels(value)
            .iter()
            .any(|label| configured_labels.iter().any(|wanted| wanted == label))
}

pub(crate) fn map_issues(value: &Value, issue_labels: &[String]) -> Result<Vec<Item>> {
    Ok(rows(value, "issues")?
        .iter()
        .filter(|issue| matches_labels(issue, issue_labels))
        .map(|issue| Item {
            title: format!("#{} {}", identifier(issue, "iid"), text(issue, "title")),
            url: text(issue, "web_url"),
            detail: labels(issue).join(", "),
        })
        .collect())
}

pub(crate) fn map_releases(value: &Value) -> Result<Vec<Item>> {
    Ok(rows(value, "releases")?
        .iter()
        .map(|release| Item {
            title: text(release, "tag_name"),
            url: text(&release["_links"], "self"),
            detail: format!("published {}", text(release, "released_at")),
        })
        .collect())
}

pub(crate) fn map_pipelines(value: &Value) -> Result<Vec<Item>> {
    Ok(rows(value, "pipelines")?
        .iter()
        .map(|pipeline| Item {
            title: format!("pipeline #{}", identifier(pipeline, "id")),
            url: text(pipeline, "web_url"),
            detail: format!(
                "{} | {} | {}",
                text(pipeline, "status"),
                text(pipeline, "ref"),
                optional_text(pipeline, "sha", "?")
            ),
        })
        .collect())
}

fn observe<T>(result: Result<T>) -> Observation<T> {
    match result {
        Ok(data) => Observation::success(data),
        Err(error) => Observation::failure(error),
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn map_snapshot(
    repo: &Repo,
    project: &Value,
    branches: &Value,
    merge_requests: &Value,
    issues: &Value,
    releases: &Value,
    reviewers: &Value,
    pipelines: &Value,
) -> RemoteState {
    let (prs, proposals, drafts) = match map_merge_requests(merge_requests, &repo.release_labels) {
        Ok(mapped) => (
            Observation::success(mapped.prs),
            Observation::success(mapped.proposals),
            Observation::success(mapped.drafts),
        ),
        Err(error) => {
            let error = error.to_string();
            (
                Observation::failure(&error),
                Observation::failure(&error),
                Observation::failure(error),
            )
        }
    };
    RemoteState {
        default_branch: observe(map_project(project)),
        branches: observe(map_branches(branches)),
        prs,
        review_requests: observe(map_review_requests(merge_requests, reviewers)),
        issues: observe(map_issues(issues, &repo.issue_labels)),
        proposals,
        drafts,
        published: observe(map_releases(releases)),
        ci: observe(map_pipelines(pipelines)),
        publication: Observation::unsupported(),
    }
}

fn observe_response<T>(
    response: Result<Value>,
    mapper: impl FnOnce(&Value) -> Result<T>,
) -> Observation<T> {
    observe(response.and_then(|value| mapper(&value)))
}

#[allow(clippy::too_many_arguments)]
fn map_http_snapshot(
    repo: &Repo,
    project: Result<Value>,
    branches: Result<Value>,
    merge_requests: Result<Value>,
    issues: Result<Value>,
    releases: Result<Value>,
    reviewer: Result<Value>,
    pipelines: Result<Value>,
) -> RemoteState {
    let (prs, review_requests, proposals, drafts) = match merge_requests {
        Ok(merge_requests) => match map_merge_requests(&merge_requests, &repo.release_labels) {
            Ok(mapped) => {
                let review_requests = match reviewer {
                    Ok(reviewer) => observe(map_review_requests(&merge_requests, &reviewer)),
                    Err(error) => Observation::failure(error),
                };
                (
                    Observation::success(mapped.prs),
                    review_requests,
                    Observation::success(mapped.proposals),
                    Observation::success(mapped.drafts),
                )
            }
            Err(error) => {
                let error = error.to_string();
                (
                    Observation::failure(&error),
                    Observation::failure(&error),
                    Observation::failure(&error),
                    Observation::failure(error),
                )
            }
        },
        Err(error) => {
            let error = error.to_string();
            (
                Observation::failure(&error),
                Observation::failure(&error),
                Observation::failure(&error),
                Observation::failure(error),
            )
        }
    };

    RemoteState {
        default_branch: observe_response(project, map_project),
        branches: observe_response(branches, map_branches),
        prs,
        review_requests,
        issues: observe_response(issues, |value| map_issues(value, &repo.issue_labels)),
        proposals,
        drafts,
        published: observe_response(releases, map_releases),
        ci: observe_response(pipelines, map_pipelines),
        publication: Observation::unsupported(),
    }
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
        published: Observation::failure(&error),
        ci: Observation::failure(error),
        publication: Observation::unsupported(),
    }
}

impl RemoteProvider for Gitlab {
    fn snapshot<'a>(&'a self, repo: &'a Repo) -> SnapshotFuture<'a> {
        Box::pin(async move {
            let client = match http_client() {
                Ok(client) => client,
                Err(error) => return failed_snapshot(error),
            };
            let project = encoded_project(&repo.identity.project);
            let project_path = format!("projects/{project}");
            let branches_path = format!("{project_path}/repository/branches");
            let merge_requests_path = format!("{project_path}/merge_requests");
            let issues_path = format!("{project_path}/issues");
            let releases_path = format!("{project_path}/releases");
            let pipelines_path = format!("{project_path}/pipelines");

            let (project, branches, merge_requests, issues, releases, reviewer, pipelines) = tokio::join!(
                client.get_json_value(&project_path),
                paginated(&client, &branches_path, &[]),
                paginated(&client, &merge_requests_path, &[("state", "opened")]),
                paginated(&client, &issues_path, &[("state", "opened")]),
                paginated(&client, &releases_path, &[]),
                client.get_json_value("user"),
                paginated(&client, &pipelines_path, &[]),
            );

            map_http_snapshot(
                repo,
                project,
                branches,
                merge_requests,
                issues,
                releases,
                reviewer,
                pipelines,
            )
        })
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
    use serde_json::json;
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

    fn http_repo(project: &str) -> Repo {
        Repo {
            identity: Remote {
                kind: ProviderKind::Gitlab,
                host: "gitlab.com".into(),
                project: project.into(),
            },
            ..test_repo()
        }
    }

    fn fixture_response(request: &Request, project: &str) -> Response {
        let path = request.url_path();
        let fixture_path = if path == format!("/api/v4/projects/{project}") {
            "tests/fixtures/gitlab/project.json"
        } else if path == format!("/api/v4/projects/{project}/repository/branches") {
            "tests/fixtures/gitlab/branches.json"
        } else if path == format!("/api/v4/projects/{project}/merge_requests") {
            "tests/fixtures/gitlab/merge_requests.json"
        } else if path == format!("/api/v4/projects/{project}/issues") {
            "tests/fixtures/gitlab/issues.json"
        } else if path == format!("/api/v4/projects/{project}/releases") {
            "tests/fixtures/gitlab/releases.json"
        } else if path == "/api/v4/user" {
            "tests/fixtures/gitlab/reviewers.json"
        } else if path == format!("/api/v4/projects/{project}/pipelines") {
            "tests/fixtures/gitlab/pipelines.json"
        } else {
            return response(404, r#"{"message":"unexpected endpoint"}"#);
        };
        response(200, &fixture(fixture_path).to_string())
    }

    fn request_target(request: &Request) -> String {
        match request.query() {
            Some(query) => format!("{}?{query}", request.url_path()),
            None => request.url_path().to_string(),
        }
    }

    fn test_repo() -> Repo {
        Repo {
            id: "gitlab".into(),
            name: "widgets".into(),
            path: std::env::temp_dir(),
            remote: "origin".into(),
            identity: Remote {
                kind: ProviderKind::Gitlab,
                host: "gitlab.example".into(),
                project: "acme/widgets".into(),
            },
            enabled: true,
            release_workflows: vec![],
            release_labels: vec!["release".into()],
            issue_labels: vec!["bug".into()],
            workspaces: vec![],
        }
    }

    fn complete_snapshot() -> crate::model::RemoteState {
        map_snapshot(
            &test_repo(),
            &fixture("tests/fixtures/gitlab/project.json"),
            &fixture("tests/fixtures/gitlab/branches.json"),
            &fixture("tests/fixtures/gitlab/merge_requests.json"),
            &fixture("tests/fixtures/gitlab/issues.json"),
            &fixture("tests/fixtures/gitlab/releases.json"),
            &fixture("tests/fixtures/gitlab/reviewers.json"),
            &fixture("tests/fixtures/gitlab/pipelines.json"),
        )
    }

    #[test]
    fn project_maps_default_branch_and_urls() {
        let state = complete_snapshot();

        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        let branches = state.branches.data.expect("branches should be observed");
        assert_eq!(branches.len(), 2);
        assert_eq!(branches[0].title, "main");
        assert_eq!(
            branches[0].url,
            "https://gitlab.example/acme/widgets/-/tree/main"
        );
        assert_eq!(
            branches[0].detail,
            "1111111111111111111111111111111111111111"
        );

        let issues = state.issues.data.expect("issues should be observed");
        assert_eq!(issues[0].title, "#12 Widget alignment is incorrect");
        assert_eq!(
            issues[0].url,
            "https://gitlab.example/acme/widgets/-/issues/12"
        );
        assert_eq!(issues[0].detail, "bug, ui");

        let published = state.published.data.expect("releases should be observed");
        assert_eq!(published[0].title, "v1.1.0");
        assert_eq!(
            published[0].url,
            "https://gitlab.example/acme/widgets/-/releases/v1.1.0"
        );
        assert_eq!(published[0].detail, "published 2026-09-01T12:00:00Z");

        let ci = state.ci.data.expect("pipelines should be observed");
        assert_eq!(ci[0].title, "pipeline #301");
        assert_eq!(
            ci[0].url,
            "https://gitlab.example/acme/widgets/-/pipelines/301"
        );
        assert_eq!(
            ci[0].detail,
            "success | main | 1111111111111111111111111111111111111111"
        );
        assert!(!state.publication.supported);
    }

    #[test]
    fn merge_requests_map_open_drafts_reviewers_and_release_evidence() {
        let state = complete_snapshot();
        let prs = state.prs.data.expect("merge requests should be observed");
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].title, "!7 Add widget support");
        assert_eq!(
            prs[0].url,
            "https://gitlab.example/acme/widgets/-/merge_requests/7"
        );
        assert_eq!(
            prs[0].detail,
            "feature/widget -> main | head 2222222222222222222222222222222222222222 | open | labels backend | reviewers reviewer (Rhea Viewer)"
        );
        assert_eq!(
            prs[1].detail,
            "release-please--branches--main--components--widgets -> main | head 3333333333333333333333333333333333333333 | draft | labels release | reviewers none"
        );

        let drafts = state.drafts.data.expect("drafts should be observed");
        assert_eq!(drafts.len(), 1);
        assert_eq!(drafts[0].title, "!8 Release: widgets 1.2.0");

        let proposals = state.proposals.data.expect("proposals should be observed");
        assert_eq!(proposals.len(), 1);
        assert_eq!(proposals[0].title, "!8 Release: widgets 1.2.0");
        assert_eq!(
            proposals[0].detail,
            "release label | release-please--branches--main--components--widgets -> main | head 3333333333333333333333333333333333333333 | draft | labels release | reviewers none"
        );

        let reviews = state
            .review_requests
            .data
            .expect("review requests should be observed");
        assert_eq!(reviews.len(), 1);
        assert_eq!(reviews[0].title, "!7 Add widget support");
        assert!(reviews[0].detail.contains("reviewer (Rhea Viewer)"));
    }

    #[test]
    fn authenticated_user_object_selects_matching_review_requests() {
        let merge_requests = fixture("tests/fixtures/gitlab/merge_requests.json");
        let authenticated_user = json!({
            "id": 501,
            "username": "reviewer",
            "name": "Rhea Viewer",
            "web_url": "https://gitlab.example/reviewer"
        });

        let mapped = map_review_requests(&merge_requests, &authenticated_user)
            .expect("GitLab GET /user object should map");

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].title, "!7 Add widget support");
    }

    #[test]
    fn missing_optional_fields_do_not_fail_the_snapshot() {
        let value = fixture("tests/fixtures/gitlab/missing_optional_fields.json");
        let state = map_snapshot(
            &test_repo(),
            &value["project"],
            &value["branches"],
            &value["merge_requests"],
            &value["issues"],
            &value["releases"],
            &value["reviewers"],
            &value["pipelines"],
        );

        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.branches.data.as_ref().unwrap()[0].detail, "?");
        assert_eq!(state.prs.data.as_ref().unwrap()[0].title, "!? ");
        assert_eq!(
            state.prs.data.as_ref().unwrap()[0].detail,
            "? -> ? | head ? | open | labels none | reviewers none"
        );
        assert!(state.review_requests.data.as_ref().unwrap().is_empty());
        assert!(state.proposals.data.as_ref().unwrap().is_empty());
        assert!(state.drafts.data.as_ref().unwrap().is_empty());
        assert_eq!(state.published.data.as_ref().unwrap()[0].url, "");
        assert_eq!(state.ci.data.as_ref().unwrap()[0].title, "pipeline #?");
        assert!(!state.publication.supported);
    }

    #[test]
    fn malformed_endpoint_fails_only_its_observation() {
        let state = map_snapshot(
            &test_repo(),
            &fixture("tests/fixtures/gitlab/project.json"),
            &json!({"message": "malformed branches"}),
            &fixture("tests/fixtures/gitlab/merge_requests.json"),
            &fixture("tests/fixtures/gitlab/issues.json"),
            &fixture("tests/fixtures/gitlab/releases.json"),
            &fixture("tests/fixtures/gitlab/reviewers.json"),
            &fixture("tests/fixtures/gitlab/pipelines.json"),
        );

        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(
            state.branches.error.as_deref(),
            Some("Expected GitLab branches array")
        );
        assert!(state.branches.data.is_none());
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.issues.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.published.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.ci.data.as_ref().map(Vec::len), Some(1));
        assert!(!state.publication.supported);
    }

    #[test]
    fn malformed_required_project_fields_return_an_error() {
        let malformed = fixture("tests/fixtures/gitlab/malformed_project.json");
        assert_eq!(
            map_project(&malformed).unwrap_err().to_string(),
            "Missing GitLab project identity"
        );
        assert_eq!(
            map_project(&json!({"path_with_namespace": "acme/widgets"}))
                .unwrap_err()
                .to_string(),
            "Missing GitLab default branch"
        );
    }

    #[tokio::test]
    async fn http_snapshot_fetches_every_endpoint_with_auth_and_nested_project_encoding() {
        let _lock = GITLAB_ENV_LOCK.lock().await;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let handler_seen = Arc::clone(&seen);
        let server = test_server(move |request| {
            handler_seen.lock().unwrap().push((
                request_target(request),
                request.header("authorization").map(str::to_string),
            ));
            fixture_response(request, "group%2Fsubgroup%2Frepo")
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/api/v4/", server.root_url()));
        let _token = EnvVar::set(TOKEN_ENV, "gitlab-test-token");

        let state = Gitlab.snapshot(&http_repo("group/subgroup/repo")).await;

        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.branches.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.review_requests.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.issues.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.published.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.ci.data.as_ref().map(Vec::len), Some(1));
        assert!(!state.publication.supported);

        let mut actual = seen.lock().unwrap().clone();
        actual.sort();
        let mut expected = [
            "/api/v4/projects/group%2Fsubgroup%2Frepo".to_string(),
            "/api/v4/projects/group%2Fsubgroup%2Frepo/issues?state=opened&per_page=100&page=1"
                .to_string(),
            "/api/v4/projects/group%2Fsubgroup%2Frepo/merge_requests?state=opened&per_page=100&page=1"
                .to_string(),
            "/api/v4/projects/group%2Fsubgroup%2Frepo/pipelines?per_page=100&page=1"
                .to_string(),
            "/api/v4/projects/group%2Fsubgroup%2Frepo/releases?per_page=100&page=1"
                .to_string(),
            "/api/v4/projects/group%2Fsubgroup%2Frepo/repository/branches?per_page=100&page=1"
                .to_string(),
            "/api/v4/user".to_string(),
        ];
        expected.sort();
        assert_eq!(
            actual.iter().map(|(target, _)| target).collect::<Vec<_>>(),
            expected.iter().collect::<Vec<_>>()
        );
        assert!(
            actual
                .iter()
                .all(|(_, auth)| { auth.as_deref() == Some("Bearer gitlab-test-token") })
        );
    }

    #[tokio::test]
    async fn http_snapshot_paginates_full_gitlab_array_pages() {
        let _lock = GITLAB_ENV_LOCK.lock().await;
        let server = test_server(|request| {
            if request.url_path().ends_with("/repository/branches") {
                return match request.query() {
                    Some("per_page=100&page=1") => response(
                        200,
                        &serde_json::to_string(
                            &(0..100)
                                .map(|index| json!({"name": format!("branch-{index}")}))
                                .collect::<Vec<_>>(),
                        )
                        .unwrap(),
                    ),
                    Some("per_page=100&page=2") => {
                        response(200, r#"[{"name":"last-branch","commit":{"id":"last"}}]"#)
                    }
                    query => panic!("unexpected branches query: {query:?}"),
                };
            }
            fixture_response(request, "acme%2Fwidgets")
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/api/v4/", server.root_url()));
        let _token = EnvVar::set(TOKEN_ENV, "gitlab-test-token");

        let state = Gitlab.snapshot(&http_repo("acme/widgets")).await;

        let branches = state.branches.data.expect("branches should be paginated");
        assert_eq!(branches.len(), 101);
        assert_eq!(branches.last().unwrap().title, "last-branch");
    }

    #[tokio::test]
    async fn http_snapshot_preserves_siblings_when_one_endpoint_fails() {
        let _lock = GITLAB_ENV_LOCK.lock().await;
        let server = test_server(|request| {
            if request.url_path().ends_with("/issues") {
                response(500, r#"{"message":"issues unavailable"}"#)
            } else {
                fixture_response(request, "acme%2Fwidgets")
            }
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/api/v4/", server.root_url()));
        let _token = EnvVar::set(TOKEN_ENV, "gitlab-test-token");

        let state = Gitlab.snapshot(&http_repo("acme/widgets")).await;

        assert!(state.issues.data.is_none());
        assert!(
            state
                .issues
                .error
                .as_deref()
                .is_some_and(|error| error.contains("500") && error.contains("issues unavailable"))
        );
        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.branches.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.published.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.ci.data.as_ref().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn http_snapshot_isolates_malformed_json_to_its_endpoint() {
        let _lock = GITLAB_ENV_LOCK.lock().await;
        let server = test_server(|request| {
            if request.url_path().ends_with("/pipelines") {
                response(200, "not-json")
            } else {
                fixture_response(request, "acme%2Fwidgets")
            }
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/api/v4/", server.root_url()));
        let _token = EnvVar::set(TOKEN_ENV, "gitlab-test-token");

        let state = Gitlab.snapshot(&http_repo("acme/widgets")).await;

        assert!(state.ci.data.is_none());
        assert!(
            state
                .ci
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Invalid JSON") && error.contains("pipelines"))
        );
        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.issues.data.as_ref().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn http_snapshot_without_token_keeps_public_endpoints_available() {
        let _lock = GITLAB_ENV_LOCK.lock().await;
        let saw_authorization = Arc::new(Mutex::new(false));
        let handler_saw_authorization = Arc::clone(&saw_authorization);
        let server = test_server(move |request| {
            if request.header("authorization").is_some() {
                *handler_saw_authorization.lock().unwrap() = true;
            }
            if request.url_path() == "/api/v4/user" {
                response(401, r#"{"message":"401 Unauthorized"}"#)
            } else {
                fixture_response(request, "acme%2Fwidgets")
            }
        })
        .await;
        let _base = EnvVar::set(BASE_URL_ENV, &format!("{}/api/v4/", server.root_url()));
        let _token = EnvVar::remove(TOKEN_ENV);

        let state = Gitlab.snapshot(&http_repo("acme/widgets")).await;

        assert!(!*saw_authorization.lock().unwrap());
        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
        assert!(state.review_requests.error.is_some());
        assert!(!state.publication.supported);
    }
}
