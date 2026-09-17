use phrourion::{git, registry};
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;

fn git_cmd(path: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(path)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{:?}: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().into()
}

struct Fixture {
    _dir: TempDir,
    a: PathBuf,
    b: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        git_cmd(
            dir.path(),
            &["init", "--bare", "--initial-branch=main", "remote.git"],
        );
        git_cmd(dir.path(), &["clone", "remote.git", "a"]);
        let a = dir.path().join("a");
        git_cmd(&a, &["commit", "--allow-empty", "-m", "initial"]);
        git_cmd(&a, &["push", "-u", "origin", "main"]);
        git_cmd(dir.path(), &["clone", "remote.git", "b"]);
        let b = dir.path().join("b");
        Self { _dir: dir, a, b }
    }
    fn advance(&self) {
        std::fs::write(self.a.join("new.txt"), "remote content").unwrap();
        git_cmd(&self.a, &["add", "new.txt"]);
        git_cmd(&self.a, &["commit", "-m", "feat: new content"]);
        git_cmd(&self.a, &["push"]);
    }
    // Simulates a GitHub PR: a commit reachable only via refs/pull/{number}/head,
    // the same special ref GitHub itself publishes on every open pull request.
    fn open_pr(&self, number: u64) -> String {
        git_cmd(&self.a, &["checkout", "-b", &format!("topic-{number}")]);
        std::fs::write(self.a.join(format!("pr{number}.txt")), "pr content").unwrap();
        git_cmd(&self.a, &["add", "."]);
        git_cmd(&self.a, &["commit", "-m", &format!("feat: pr {number}")]);
        let sha = git_cmd(&self.a, &["rev-parse", "HEAD"]);
        git_cmd(
            &self.a,
            &["push", "origin", &format!("HEAD:refs/pull/{number}/head")],
        );
        git_cmd(&self.a, &["checkout", "main"]);
        git_cmd(&self.a, &["branch", "-D", &format!("topic-{number}")]);
        sha
    }
}

#[tokio::test]
async fn pulls_only_registered_checkout_to_previewed_commit() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.advance();
    let original_a = git_cmd(&f.a, &["rev-parse", "HEAD"]);
    let preview = git::preview(&repo).await.unwrap();
    assert_eq!(preview.before.behind, 1);
    assert!(!f.b.join("new.txt").exists());
    git::apply(&preview).await.unwrap();
    assert_eq!(git_cmd(&f.b, &["rev-parse", "HEAD"]), preview.target);
    assert_eq!(git_cmd(&f.a, &["rev-parse", "HEAD"]), original_a);
    assert_eq!(
        std::fs::read_to_string(f.b.join("new.txt")).unwrap(),
        "remote content"
    );
}

#[tokio::test]
async fn dirty_and_changed_previews_are_refused() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.advance();
    let preview = git::preview(&repo).await.unwrap();
    std::fs::write(f.b.join("precious.txt"), "keep me").unwrap();
    assert!(git::apply(&preview).await.is_err());
    assert!(git::preview(&repo).await.is_err());
    assert_eq!(
        std::fs::read_to_string(f.b.join("precious.txt")).unwrap(),
        "keep me"
    );
    assert!(!f.b.join("new.txt").exists());
}

#[tokio::test]
async fn divergence_and_detached_head_are_refused() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.advance();
    git_cmd(&f.b, &["commit", "--allow-empty", "-m", "local"]);
    assert!(
        git::preview(&repo)
            .await
            .unwrap_err()
            .to_string()
            .contains("diverged")
    );
    git_cmd(&f.b, &["checkout", "--detach"]);
    assert!(
        git::preview(&repo)
            .await
            .unwrap_err()
            .to_string()
            .contains("detached")
    );
}

#[tokio::test]
async fn branch_change_invalidates_preview() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.advance();
    let preview = git::preview(&repo).await.unwrap();
    git_cmd(
        &f.b,
        &["checkout", "-b", "feature", "--track", "origin/main"],
    );
    assert!(git::apply(&preview).await.is_err());
    assert_eq!(git_cmd(&f.b, &["branch", "--show-current"]), "feature");
}

#[tokio::test]
async fn remote_change_invalidates_preview() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.advance();
    let preview = git::preview(&repo).await.unwrap();
    git_cmd(
        &f.b,
        &["remote", "set-url", "origin", "/different/remote.git"],
    );
    assert!(
        git::apply(&preview)
            .await
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
}

#[tokio::test]
async fn subdirectories_resolve_and_registry_removal_preserves_checkout() {
    let f = Fixture::new();
    std::fs::create_dir(f.b.join("sub")).unwrap();
    let repo = registry::entry(&f.b.join("sub"), None, None).await.unwrap();
    assert_eq!(repo.path, f.b.canonicalize().unwrap());
    let config = f._dir.path().join("config/repos.toml");
    registry::add(&config, repo.clone()).unwrap();
    assert!(registry::add(&config, repo.clone()).is_err());
    registry::remove(&config, &repo.id).unwrap();
    assert!(registry::load(&config).unwrap().repos.is_empty());
    assert!(f.b.join(".git").exists());
}

#[tokio::test]
async fn feature_branch_pull_keeps_feature_branch_selected() {
    let f = Fixture::new();
    git_cmd(&f.a, &["checkout", "-b", "feature"]);
    git_cmd(&f.a, &["push", "-u", "origin", "feature"]);
    git_cmd(&f.b, &["fetch"]);
    git_cmd(&f.b, &["checkout", "--track", "origin/feature"]);
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.advance();
    let preview = git::preview(&repo).await.unwrap();
    git::apply(&preview).await.unwrap();
    assert_eq!(git_cmd(&f.b, &["branch", "--show-current"]), "feature");
    assert_ne!(git_cmd(&f.b, &["rev-parse", "main"]), preview.target);
}

#[tokio::test]
async fn renamed_files_and_untracked_names_do_not_confuse_status_parser() {
    let f = Fixture::new();
    f.advance();
    git_cmd(&f.b, &["pull", "--ff-only"]);
    git_cmd(&f.b, &["mv", "new.txt", "renamed file.txt"]);
    std::fs::write(f.b.join("question.txt"), "?").unwrap();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    let state = git::snapshot(&repo).await.unwrap();
    assert_eq!(state.staged, 1);
    assert_eq!(state.untracked, 1);
    assert_eq!(state.changes.len(), 2);
}

#[tokio::test]
async fn staged_and_unstaged_modifications_are_counted_independently() {
    let f = Fixture::new();
    std::fs::write(f.b.join("tracked.txt"), "original").unwrap();
    git_cmd(&f.b, &["add", "tracked.txt"]);
    git_cmd(&f.b, &["commit", "-m", "add tracked file"]);
    git_cmd(&f.b, &["push"]);
    let repo = registry::entry(&f.b, None, None).await.unwrap();

    // A change staged with `git add` but not yet touched again in the
    // working tree: staged, not modified.
    std::fs::write(f.b.join("tracked.txt"), "staged change").unwrap();
    git_cmd(&f.b, &["add", "tracked.txt"]);
    let state = git::snapshot(&repo).await.unwrap();
    assert_eq!(state.staged, 1);
    assert_eq!(state.modified, 0);

    // The same file edited again without re-staging: both staged (from the
    // prior `add`) and modified (the new, unstaged edit).
    std::fs::write(f.b.join("tracked.txt"), "unstaged change").unwrap();
    let state = git::snapshot(&repo).await.unwrap();
    assert_eq!(state.staged, 1);
    assert_eq!(state.modified, 1);
}

#[tokio::test]
async fn checkout_fetches_pr_head_into_a_new_local_branch() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    let sha = f.open_pr(7);
    let preview = git::checkout_preview(&repo, 7).await.unwrap();
    assert_eq!(preview.branch, "pr/7");
    assert_eq!(preview.target, sha);
    assert_eq!(git_cmd(&f.b, &["branch", "--show-current"]), "main");
    git::checkout_apply(&preview).await.unwrap();
    assert_eq!(git_cmd(&f.b, &["branch", "--show-current"]), "pr/7");
    assert_eq!(git_cmd(&f.b, &["rev-parse", "HEAD"]), sha);
    assert_eq!(
        std::fs::read_to_string(f.b.join("pr7.txt")).unwrap(),
        "pr content"
    );
}

#[tokio::test]
async fn checkout_is_refused_when_tree_is_dirty() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.open_pr(7);
    std::fs::write(f.b.join("precious.txt"), "keep me").unwrap();
    assert!(
        git::checkout_preview(&repo, 7)
            .await
            .unwrap_err()
            .to_string()
            .contains("uncommitted")
    );
    assert_eq!(git_cmd(&f.b, &["branch", "--show-current"]), "main");
}

#[tokio::test]
async fn checkout_is_refused_when_the_local_branch_name_is_taken() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.open_pr(7);
    git_cmd(&f.b, &["branch", "pr/7"]);
    assert!(
        git::checkout_preview(&repo, 7)
            .await
            .unwrap_err()
            .to_string()
            .contains("already exists")
    );
}

#[tokio::test]
async fn checkout_apply_is_refused_if_the_tree_changed_since_preview() {
    let f = Fixture::new();
    let repo = registry::entry(&f.b, None, None).await.unwrap();
    f.open_pr(7);
    let preview = git::checkout_preview(&repo, 7).await.unwrap();
    std::fs::write(f.b.join("precious.txt"), "keep me").unwrap();
    assert!(git::checkout_apply(&preview).await.is_err());
    assert_eq!(git_cmd(&f.b, &["branch", "--show-current"]), "main");
}

#[test]
fn remote_urls_strip_credentials_and_support_nested_namespaces() {
    use phrourion::model::ProviderKind;
    let remote =
        registry::parse_remote("https://secret:password@gitlab.com/team/sub/repo.git", None)
            .unwrap();
    assert_eq!(remote.project, "team/sub/repo");
    assert_eq!(remote.host, "gitlab.com");
    assert_eq!(remote.kind, ProviderKind::Gitlab);
    let ssh = registry::parse_remote("git@github.com:AraneaDev/phrourion.git", None).unwrap();
    assert_eq!(ssh.project, "AraneaDev/phrourion");
    assert!(registry::parse_remote("ssh://git@forge.internal/team/repo", None).is_err());
    assert!(
        registry::parse_remote(
            "ssh://git@forge.internal/team/repo",
            Some(ProviderKind::Forgejo)
        )
        .is_ok()
    );
}
