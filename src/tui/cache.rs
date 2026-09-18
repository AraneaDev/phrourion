use crate::model::{Observation, RemoteState, Repo};
use anyhow::Result;
use std::{
    hash::{Hash, Hasher},
    path::PathBuf,
};

// The placeholder `cached()` attaches to every field before this session's
// own fetch resolves it one way or the other. Rendering code checks for this
// exact string to distinguish it from a real fetch failure, so a repo that
// hasn't actually failed this session isn't shown as errored.
pub(super) const CACHE_PLACEHOLDER: &str = "Cached; awaiting refresh";

fn cache_path(repo: &Repo) -> Option<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", repo.identity).hash(&mut hasher);
    repo.release_workflows.hash(&mut hasher);
    repo.release_labels.hash(&mut hasher);
    repo.issue_labels.hash(&mut hasher);
    Some(
        dirs::cache_dir()?
            .join("phrourion")
            .join(format!("{:x}.json", hasher.finish())),
    )
}

pub(super) fn cached(repo: &Repo) -> RemoteState {
    let state: Option<RemoteState> = cache_path(repo)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|s| serde_json::from_slice(&s).ok());
    if let Some(state) = state {
        let unavailable = RemoteState {
            default_branch: Observation::failure(CACHE_PLACEHOLDER),
            branches: Observation::failure(CACHE_PLACEHOLDER),
            prs: Observation::failure(CACHE_PLACEHOLDER),
            review_requests: Observation::failure(CACHE_PLACEHOLDER),
            issues: Observation::failure(CACHE_PLACEHOLDER),
            proposals: Observation::failure(CACHE_PLACEHOLDER),
            drafts: Observation::failure(CACHE_PLACEHOLDER),
            published: Observation::failure(CACHE_PLACEHOLDER),
            ci: Observation::failure(CACHE_PLACEHOLDER),
            publication: Observation::failure(CACHE_PLACEHOLDER),
        };
        unavailable.retain_previous(&state)
    } else {
        RemoteState::default()
    }
}

pub(super) fn save_cache(repo: &Repo, state: &RemoteState) {
    if let Some(path) = cache_path(repo) {
        let result = (|| -> Result<()> {
            use std::io::Write;
            let parent = path.parent().unwrap();
            std::fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&serde_json::to_vec(state)?)?;
            file.persist(path)?;
            Ok(())
        })();
        // Cache is optional. A failed write never hides an observed remote error.
        let _ = result;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ProviderKind, Remote};

    fn test_repo(project: &str) -> Repo {
        Repo {
            id: "t".into(),
            name: "t".into(),
            path: std::env::temp_dir(),
            remote: "origin".into(),
            identity: Remote {
                kind: ProviderKind::Github,
                host: "example.com".into(),
                project: project.into(),
            },
            enabled: true,
            release_workflows: vec![],
            release_labels: vec![],
            issue_labels: vec![],
            workspaces: vec![],
        }
    }

    #[test]
    fn cache_path_is_stable_and_save_cache_round_trips_through_cached() {
        let dir = tempfile::tempdir().unwrap();
        let previous = std::env::var("XDG_CACHE_HOME").ok();
        unsafe {
            std::env::set_var("XDG_CACHE_HOME", dir.path());
        }

        let repo_a = test_repo("org/a");
        let repo_b = test_repo("org/b");

        let path_a1 = cache_path(&repo_a).unwrap();
        let path_a2 = cache_path(&repo_a).unwrap();
        let path_b = cache_path(&repo_b).unwrap();
        assert_eq!(
            path_a1, path_a2,
            "the same repo identity must hash to the same path"
        );
        assert_ne!(path_a1, path_b, "different repos must not collide");

        // No cache file yet: falls back to a fresh default state, not a
        // panic or a leftover from elsewhere.
        let empty = cached(&repo_a);
        assert!(empty.default_branch.data.is_none());
        assert!(empty.default_branch.error.is_none());

        let saved = RemoteState {
            default_branch: Observation::success("main".to_string()),
            ..RemoteState::default()
        };
        save_cache(&repo_a, &saved);
        let loaded = cached(&repo_a);

        unsafe {
            match &previous {
                Some(v) => std::env::set_var("XDG_CACHE_HOME", v),
                None => std::env::remove_var("XDG_CACHE_HOME"),
            }
        }

        // cached() wraps the reloaded state in the placeholder error but
        // backfills the real data via retain_previous.
        assert_eq!(loaded.default_branch.data.as_deref(), Some("main"));
        assert_eq!(
            loaded.default_branch.error.as_deref(),
            Some(CACHE_PLACEHOLDER)
        );
    }
}
