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
