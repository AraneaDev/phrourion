use crate::{
    model::{Item, Observation, RemoteState, clean},
    provider,
};
use anyhow::{Context, Result};
use serde_json::Value;

pub struct Bitbucket;

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

pub(crate) fn map_issues(value: &Value) -> Result<Vec<Item>> {
    Ok(rows(value, "issues")?
        .iter()
        .map(|issue| Item {
            title: format!("#{} {}", identifier(issue, "id"), text(issue, "title")),
            url: link(issue),
            detail: ["kind", "priority", "state"]
                .iter()
                .map(|key| text(issue, key))
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join(" | "),
        })
        .collect())
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
    issues: &Value,
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
        issues: match map_issues(issues) {
            Ok(items) => Observation::success(items),
            Err(error) => Observation::failure(error.to_string()),
        },
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
    use crate::test_support::fixture;

    #[test]
    fn bitbucket_fixtures_map_the_neutral_state() {
        let state = map_snapshot(
            &fixture("tests/fixtures/bitbucket/repository.json"),
            &fixture("tests/fixtures/bitbucket/branches.json"),
            &fixture("tests/fixtures/bitbucket/pull_requests.json"),
            &fixture("tests/fixtures/bitbucket/reviewers.json"),
            &fixture("tests/fixtures/bitbucket/issues.json"),
            &fixture("tests/fixtures/bitbucket/pipelines.json"),
        );
        assert_eq!(state.default_branch.data.as_deref(), Some("main"));
        assert_eq!(state.branches.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.prs.data.as_ref().map(Vec::len), Some(2));
        assert_eq!(state.review_requests.data.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.issues.data.as_ref().map(Vec::len), Some(1));
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
        let issues = map_issues(&value).unwrap();
        assert_eq!(issues[0].title, "#? ");
        assert_eq!(issues[0].detail, "");
    }

    #[test]
    fn invalid_collection_shapes_are_reported() {
        let error = map_branches(&serde_json::json!({}))
            .unwrap_err()
            .to_string();
        assert!(error.contains("Bitbucket branches"));
    }
}
