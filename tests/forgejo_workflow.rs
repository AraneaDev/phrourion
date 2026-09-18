use phrourion::{
    forgejo::Forgejo,
    model::{ProviderKind, Remote, Repo},
    provider::RemoteProvider,
};

fn test_repo(project: &str) -> Repo {
    Repo {
        id: "forgejo-test".into(),
        name: "forgejo-test".into(),
        path: std::env::temp_dir(),
        remote: "origin".into(),
        identity: Remote {
            kind: ProviderKind::Forgejo,
            host: "localhost".into(),
            project: project.into(),
        },
        enabled: true,
        release_workflows: vec![],
        release_labels: vec![],
        issue_labels: vec![],
        workspaces: vec![],
    }
}

macro_rules! require_forgejo {
    () => {
        if std::env::var("PHROURION_FORGEJO_TEST_URL").is_err() {
            eprintln!("skipping: PHROURION_FORGEJO_TEST_URL is unset (run scripts/forgejo-dev.sh)");
            return;
        }
    };
}

// Rust's test harness runs different #[test] fns concurrently on separate
// threads by default, but `PHROURION_TOKEN_LOCALHOST` is process-global.
// `snapshot_works_unauthenticated_for_a_readable_repo` below temporarily
// unsets it, which would otherwise race with the other tests' authenticated
// requests. Serialize all three so only one is ever mid-flight.
static LIVE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn snapshot_reports_the_seeded_pr_issue_and_branch() {
    require_forgejo!();
    let _guard = LIVE_TEST_LOCK.lock().await;
    let repo = test_repo("phro-admin/phro-test-repo");
    let state = Forgejo.snapshot(&repo).await;

    assert!(
        state.default_branch.data.as_deref() == Some("main"),
        "default_branch: {:?}",
        state.default_branch
    );

    let branches = state.branches.data.expect("branches should be populated");
    assert!(branches.iter().any(|b| b.title == "main"), "{branches:?}");
    assert!(
        branches.iter().all(|b| b.detail != "?"),
        "every branch should report a real commit id: {branches:?}"
    );

    let prs = state.prs.data.expect("prs should be populated");
    assert!(
        prs.iter().any(|p| p.title.contains("topic")),
        "expected the seeded PR: {prs:?}"
    );

    let issues = state.issues.data.expect("issues should be populated");
    assert!(
        issues.iter().any(|i| i.title.contains("something broke")),
        "expected the seeded issue: {issues:?}"
    );

    assert!(
        state.ci.error.is_none() && state.ci.supported,
        "the commit-status endpoint call should succeed even with zero \
         configured CI systems (an empty list, not an error): {:?}",
        state.ci
    );

    assert!(!state.review_requests.supported);
    assert!(!state.publication.supported);
}

#[tokio::test]
async fn snapshot_fails_cleanly_for_an_unknown_repo() {
    require_forgejo!();
    let _guard = LIVE_TEST_LOCK.lock().await;
    let repo = test_repo("phro-admin/does-not-exist");
    let state = Forgejo.snapshot(&repo).await;
    assert!(state.default_branch.error.is_some());
}

#[tokio::test]
async fn snapshot_works_unauthenticated_for_a_readable_repo() {
    require_forgejo!();
    let _guard = LIVE_TEST_LOCK.lock().await;
    // SAFETY: serialized against the other tests in this file via
    // LIVE_TEST_LOCK, so no other test observes this var mid-mutation;
    // restored before returning.
    let host_var = phrourion::forgejo::env_token_var("localhost");
    let previous = std::env::var(&host_var).ok();
    unsafe {
        std::env::remove_var(&host_var);
    }
    let repo = test_repo("phro-admin/phro-test-repo");
    let state = Forgejo.snapshot(&repo).await;
    if let Some(token) = previous {
        unsafe {
            std::env::set_var(&host_var, token);
        }
    }
    assert!(
        state.default_branch.data.is_some(),
        "a public repo should be readable without a token: {:?}",
        state.default_branch
    );
}
