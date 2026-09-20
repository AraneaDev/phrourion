use crate::{
    model::{Item, Observation, RemoteState, Repo, clean},
    provider,
};
use anyhow::{Context, Result, bail};
use serde_json::Value;
use std::collections::HashSet;

pub struct Gitlab;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        model::{ProviderKind, Remote, Repo},
        test_support::fixture,
    };
    use serde_json::json;

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
}
