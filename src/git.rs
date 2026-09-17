use crate::{command, model::Repo, registry};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    path::Path,
};

pub async fn run(path: &Path, args: &[&str]) -> Result<String> {
    command::run("git", args, path).await
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalState {
    pub branch: String,
    pub head: String,
    pub upstream: String,
    pub ahead: usize,
    pub behind: usize,
    pub staged: usize,
    pub modified: usize,
    pub untracked: usize,
    pub conflicts: usize,
    pub changes: Vec<String>,
    pub branches: Vec<String>,
    pub operation: bool,
}

impl LocalState {
    pub fn dirty(&self) -> bool {
        self.staged + self.modified + self.untracked + self.conflicts > 0
    }
    pub fn sync(&self) -> String {
        if self.upstream.is_empty() {
            "no upstream".into()
        } else if self.ahead > 0 && self.behind > 0 {
            format!("diverged +{} -{}", self.ahead, self.behind)
        } else if self.ahead > 0 {
            format!("ahead {}", self.ahead)
        } else if self.behind > 0 {
            format!("behind {}", self.behind)
        } else {
            "current".into()
        }
    }
}

pub async fn snapshot(repo: &Repo) -> Result<LocalState> {
    let root = run(&repo.path, &["rev-parse", "--show-toplevel"]).await?;
    if std::fs::canonicalize(root)? != repo.path {
        bail!("Checkout path changed; repair the registry entry");
    }
    let text = run(
        &repo.path,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ],
    )
    .await?;
    let mut s = LocalState::default();
    let mut records = text.split('\0');
    while let Some(line) = records.next() {
        if let Some(v) = line.strip_prefix("# branch.head ") {
            s.branch = v.into();
        } else if let Some(v) = line.strip_prefix("# branch.oid ") {
            s.head = v.into();
        } else if let Some(v) = line.strip_prefix("# branch.upstream ") {
            s.upstream = v.into();
        } else if let Some(v) = line.strip_prefix("# branch.ab ") {
            let mut parts = v.split_whitespace();
            s.ahead = parts
                .next()
                .unwrap_or("+0")
                .trim_start_matches('+')
                .parse()?;
            s.behind = parts
                .next()
                .unwrap_or("-0")
                .trim_start_matches('-')
                .parse()?;
        } else if line.starts_with("? ") {
            s.untracked += 1;
            s.changes.push(line.into());
        } else if line.starts_with("u ") {
            s.conflicts += 1;
            s.changes.push(line.into());
        } else if line.starts_with("1 ") || line.starts_with("2 ") {
            let xy = line.as_bytes();
            if xy.get(2) != Some(&b'.') {
                s.staged += 1;
            }
            if xy.get(3) != Some(&b'.') {
                s.modified += 1;
            }
            s.changes.push(line.into());
            if line.starts_with("2 ") {
                records.next();
            }
        }
    }
    s.branches = run(
        &repo.path,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(upstream:short) %(upstream:track)",
            "refs/heads",
            "refs/remotes",
        ],
    )
    .await?
    .lines()
    .map(str::to_owned)
    .collect();
    for name in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
        "BISECT_LOG",
    ] {
        let p = run(&repo.path, &["rev-parse", "--git-path", name]).await?;
        if repo.path.join(p).exists() {
            s.operation = true;
        }
    }
    Ok(s)
}

async fn identity(repo: &Repo) -> Result<()> {
    let url = run(&repo.path, &["remote", "get-url", &repo.remote]).await?;
    let current = registry::parse_remote(&url, Some(repo.identity.kind.clone()))?;
    if current != repo.identity {
        bail!("Remote identity changed; remove and re-add this checkout");
    }
    Ok(())
}

async fn lock(repo: &Repo) -> Result<File> {
    let common = run(&repo.path, &["rev-parse", "--git-common-dir"]).await?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(repo.path.join(common).join("phrourion.lock"))?;
    file.try_lock_exclusive()
        .context("Another Phrourion action is using this repository")?;
    Ok(file)
}

pub async fn fetch(repo: &Repo) -> Result<()> {
    let _lock = lock(repo).await?;
    identity(repo).await?;
    run(&repo.path, &["fetch", "--", &repo.remote]).await?;
    Ok(())
}

#[derive(Clone, Debug)]
pub struct PullPreview {
    pub repo: Repo,
    pub before: LocalState,
    pub target: String,
    pub tracking_remote: String,
    pub tracking_ref: String,
}

fn eligible(s: &LocalState) -> Result<()> {
    if s.dirty() {
        bail!("Working tree has uncommitted or untracked changes");
    }
    if s.operation {
        bail!("A Git operation is in progress");
    }
    if s.branch == "(detached)" || s.branch.is_empty() {
        bail!("HEAD is detached");
    }
    if s.upstream.is_empty() {
        bail!("Current branch has no upstream");
    }
    if s.ahead > 0 && s.behind > 0 {
        bail!("Branch has diverged; fast-forward is impossible");
    }
    Ok(())
}

// True if the checkout moved to a different commit or branch between two
// snapshots taken around a fetch — the operation planned against the first
// snapshot is no longer safe to apply to the second.
fn changed_during_fetch(initial: &LocalState, before: &LocalState) -> bool {
    initial.head != before.head || initial.branch != before.branch
}

async fn tracking(repo: &Repo, branch: &str) -> Result<(String, String)> {
    let remote = run(
        &repo.path,
        &["config", "--get", &format!("branch.{branch}.remote")],
    )
    .await?;
    let reference = run(
        &repo.path,
        &["config", "--get", &format!("branch.{branch}.merge")],
    )
    .await?;
    if remote != repo.remote {
        bail!(
            "Branch upstream uses {remote}, but registered remote is {}",
            repo.remote
        );
    }
    Ok((remote, reference))
}

pub async fn preview(repo: &Repo) -> Result<PullPreview> {
    let _lock = lock(repo).await?;
    identity(repo).await?;
    let initial = snapshot(repo).await?;
    eligible(&initial)?;
    let (tracking_remote, tracking_ref) = tracking(repo, &initial.branch).await?;
    run(&repo.path, &["fetch", "--", &tracking_remote]).await?;
    let before = snapshot(repo).await?;
    eligible(&before)?;
    if changed_during_fetch(&initial, &before) {
        bail!("Checkout changed during fetch; try again");
    }
    if before.behind == 0 {
        bail!("No incoming commits; checkout needs no pull");
    }
    let target = run(
        &repo.path,
        &["rev-parse", "--verify", "@{upstream}^{commit}"],
    )
    .await?;
    Ok(PullPreview {
        repo: repo.clone(),
        before,
        target,
        tracking_remote,
        tracking_ref,
    })
}

#[derive(Clone, Debug)]
pub struct CheckoutPreview {
    pub repo: Repo,
    pub before: LocalState,
    pub number: u64,
    pub branch: String,
    pub target: String,
}

fn checkout_eligible(s: &LocalState) -> Result<()> {
    if s.dirty() {
        bail!("Working tree has uncommitted or untracked changes");
    }
    if s.operation {
        bail!("A Git operation is in progress");
    }
    Ok(())
}

fn has_local_branch(s: &LocalState, branch: &str) -> bool {
    s.branches
        .iter()
        .any(|b| b.split_whitespace().next() == Some(branch))
}

pub async fn checkout_preview(repo: &Repo, number: u64) -> Result<CheckoutPreview> {
    let _lock = lock(repo).await?;
    identity(repo).await?;
    let initial = snapshot(repo).await?;
    checkout_eligible(&initial)?;
    let branch = format!("pr/{number}");
    if has_local_branch(&initial, &branch) {
        bail!("Local branch {branch} already exists");
    }
    run(
        &repo.path,
        &[
            "fetch",
            "--",
            &repo.remote,
            &format!("refs/pull/{number}/head"),
        ],
    )
    .await?;
    let target = run(
        &repo.path,
        &["rev-parse", "--verify", "FETCH_HEAD^{commit}"],
    )
    .await?;
    let before = snapshot(repo).await?;
    checkout_eligible(&before)?;
    if changed_during_fetch(&initial, &before) {
        bail!("Checkout changed during fetch; try again");
    }
    Ok(CheckoutPreview {
        repo: repo.clone(),
        before,
        number,
        branch,
        target,
    })
}

pub async fn checkout_apply(preview: &CheckoutPreview) -> Result<String> {
    let repo = &preview.repo;
    let _lock = lock(repo).await?;
    identity(repo).await?;
    let current = snapshot(repo).await?;
    checkout_eligible(&current)?;
    if current != preview.before {
        bail!("Checkout changed since preview; request a new checkout preview");
    }
    if has_local_branch(&current, &preview.branch) {
        bail!("Local branch {} already exists", preview.branch);
    }
    run(
        &repo.path,
        &["checkout", "-b", &preview.branch, &preview.target],
    )
    .await?;
    Ok(format!(
        "{}\ncheckout PR #{} -> {} ({})",
        repo.path.display(),
        preview.number,
        preview.branch,
        &preview.target[..7.min(preview.target.len())]
    ))
}

pub async fn apply(preview: &PullPreview) -> Result<String> {
    let repo = &preview.repo;
    let _lock = lock(repo).await?;
    identity(repo).await?;
    let current = snapshot(repo).await?;
    eligible(&current)?;
    let tracking = tracking(repo, &current.branch).await?;
    if current != preview.before
        || tracking
            != (
                preview.tracking_remote.clone(),
                preview.tracking_ref.clone(),
            )
    {
        bail!("Checkout changed since preview; request a new pull preview");
    }
    let upstream = run(
        &repo.path,
        &["rev-parse", "--verify", "@{upstream}^{commit}"],
    )
    .await?;
    if upstream != preview.target {
        bail!("Upstream changed since preview; try again");
    }
    let output = run(
        &repo.path,
        &[
            "-c",
            "merge.autoStash=false",
            "merge",
            "--ff-only",
            "--no-edit",
            &preview.target,
        ],
    )
    .await?;
    let after = run(&repo.path, &["rev-parse", "HEAD"]).await?;
    Ok(format!(
        "{}\n{} -> {}\n{}",
        repo.path.display(),
        preview.before.head,
        after,
        output
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dirty_is_true_when_any_single_field_is_nonzero() {
        assert!(!LocalState::default().dirty());
        assert!(
            LocalState {
                staged: 1,
                ..LocalState::default()
            }
            .dirty()
        );
        assert!(
            LocalState {
                modified: 1,
                ..LocalState::default()
            }
            .dirty()
        );
        assert!(
            LocalState {
                untracked: 1,
                ..LocalState::default()
            }
            .dirty()
        );
        assert!(
            LocalState {
                conflicts: 1,
                ..LocalState::default()
            }
            .dirty()
        );
    }

    #[test]
    fn sync_reports_no_upstream_before_diverged_ahead_behind_or_current() {
        assert_eq!(LocalState::default().sync(), "no upstream");
        let with_upstream = |ahead, behind| LocalState {
            upstream: "origin/main".into(),
            ahead,
            behind,
            ..LocalState::default()
        };
        assert_eq!(with_upstream(0, 0).sync(), "current");
        assert_eq!(with_upstream(1, 0).sync(), "ahead 1");
        assert_eq!(with_upstream(0, 1).sync(), "behind 1");
        assert_eq!(with_upstream(1, 1).sync(), "diverged +1 -1");
    }

    #[test]
    fn eligible_refuses_only_when_both_ahead_and_behind_are_nonzero() {
        let base = LocalState {
            branch: "main".into(),
            upstream: "origin/main".into(),
            ..LocalState::default()
        };
        assert!(eligible(&base).is_ok());
        assert!(
            eligible(&LocalState {
                ahead: 1,
                ..base.clone()
            })
            .is_ok()
        );
        assert!(
            eligible(&LocalState {
                behind: 1,
                ..base.clone()
            })
            .is_ok()
        );
        assert!(
            eligible(&LocalState {
                ahead: 1,
                behind: 1,
                ..base
            })
            .is_err()
        );
    }

    #[test]
    fn changed_during_fetch_flags_a_moved_head_or_a_switched_branch() {
        let base = LocalState {
            head: "abc123".into(),
            branch: "main".into(),
            ..LocalState::default()
        };
        assert!(!changed_during_fetch(&base, &base));
        assert!(changed_during_fetch(
            &base,
            &LocalState {
                head: "def456".into(),
                ..base.clone()
            }
        ));
        assert!(changed_during_fetch(
            &base,
            &LocalState {
                branch: "other".into(),
                ..base.clone()
            }
        ));
        assert!(changed_during_fetch(
            &base,
            &LocalState {
                head: "def456".into(),
                branch: "other".into(),
                ..base.clone()
            }
        ));
    }
}
