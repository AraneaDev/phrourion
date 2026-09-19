# Multi-Provider Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add tested GitLab.com and Bitbucket Cloud monitoring adapters with configurable HTTP endpoints while preserving the existing neutral TUI/provider contract.

**Architecture:** Keep `RemoteProvider` and `RemoteState` as the application boundary. Extract shared HTTP/auth/error/pagination helpers into `provider_http.rs`, then implement provider-specific request and JSON mapping modules in `gitlab.rs` and `bitbucket.rs`. New adapters return `Observation::unsupported()` for capabilities without a reliable provider equivalent.

**Tech Stack:** Rust 2024, `reqwest` with rustls, `serde_json`, Tokio, existing local HTTP test harness patterns, Cargo tests, Clippy, coverage/mutation checks, and release-please.

**Spec:** `docs/superpowers/specs/2026-09-19-multi-provider-support-design.md`

## Global Constraints

- Support hosted `gitlab.com` and `bitbucket.org` first; do not add a paid or required external service to CI.
- Preserve `RemoteProvider`, `RemoteState`, `Observation`, and the TUI-facing neutral data boundary.
- Read `PHROURION_GITLAB_TOKEN` and `PHROURION_BITBUCKET_TOKEN` at request time; never persist credentials.
- Read `PHROURION_GITLAB_BASE_URL` and `PHROURION_BITBUCKET_BASE_URL` for mock/self-hosted-compatible endpoint overrides.
- Keep `ProviderKind::BitbucketServer` unsupported in this milestone.
- Default tests must run without network credentials and without network access.
- Use TDD: write a failing focused test, run it, implement the smallest behavior, rerun the focused test, then run the relevant broader suite.
- Use frequent commits whose messages describe one independently reviewable change.

---

### Task 1: Establish shared provider HTTP test seams

**Files:**
- Create: `src/provider_http.rs`
- Modify: `src/lib.rs`
- Modify: `Cargo.toml` only if the existing local HTTP harness cannot be reused without a test-only dependency
- Test: `src/provider_http.rs` unit tests

**Interfaces:**
- Consumes: `reqwest`, `anyhow`, `serde::de::DeserializeOwned`, existing environment-lock conventions.
- Produces: shared request/auth/error/pagination helpers used by GitLab, Bitbucket, and later Forgejo migration.

- [ ] **Step 1: Inspect the Forgejo local server and existing response helpers**

  Identify the reusable request parsing and local HTTP server code in `src/forgejo.rs`. Preserve the current Forgejo behavior while extracting only provider-independent pieces.

- [ ] **Step 2: Write failing tests for shared request behavior**

  Add tests covering the intended public(crate) helper behavior:

  ```rust
  #[tokio::test]
  async fn bearer_auth_and_json_errors_are_normalized() {
      let server = test_server(|request| {
          assert_eq!(request.header("authorization"), Some("Bearer test-token"));
          response(200, r#"{"default_branch":"main"}"#)
      }).await;

      let client = HttpClient::new(server.url(), Auth::Bearer("test-token".into())).unwrap();
      let value: serde_json::Value = client.get_json("projects/example").await.unwrap();
      assert_eq!(value["default_branch"], "main");
  }
  ```

  Also add failures for non-success status, malformed JSON, and a paginated response with a continuation link.

- [ ] **Step 3: Run the focused tests and confirm they fail**

  Run: `cargo test provider_http -- --test-threads=1`

  Expected: compile/test failure because the shared client and test helpers do not exist yet.

- [ ] **Step 4: Implement the minimal shared HTTP client**

  Add these exact concepts in `src/provider_http.rs`:

  ```rust
  pub(crate) enum Auth {
      None,
      Bearer(String),
      Basic { username: String, password: String },
  }

  pub(crate) struct HttpClient { /* reqwest client, normalized base URL, auth */ }

  impl HttpClient {
      pub(crate) fn new(base_url: &str, auth: Auth) -> Result<Self>;
      pub(crate) async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T>;
      pub(crate) async fn get_json_value(&self, path: &str) -> Result<Value>;
  }
  ```

  Normalize the base URL once, join relative paths without double slashes, attach auth headers, reject non-success responses with bounded body context, and surface JSON decode errors with endpoint context.

- [ ] **Step 5: Implement pagination as a response-shape helper**

  Support the two shapes needed by the first providers: an array plus provider-owned next-link extraction, and a page object whose `next` link is a URL. Keep provider-specific next-link extraction in closures or provider modules rather than embedding GitLab/Bitbucket field names in the generic client.

- [ ] **Step 6: Run focused and existing provider tests**

  Run: `cargo test provider_http -- --test-threads=1`

  Run: `cargo test forgejo provider -- --test-threads=1`

  Expected: all new shared-helper tests and all existing provider tests pass.

- [ ] **Step 7: Commit the shared seam**

  ```bash
  git add src/provider_http.rs src/lib.rs Cargo.toml
  git commit -m "refactor(provider): add shared HTTP test seam"
  ```

### Task 2: Add reusable provider fixture and mock-server infrastructure

**Files:**
- Create: `tests/fixtures/gitlab/` JSON fixture files
- Create: `tests/fixtures/bitbucket/` JSON fixture files
- Modify: `src/lib.rs` test-support module
- Modify: `src/forgejo.rs` only when extracting generic local-server helpers
- Test: provider-specific tests added in later tasks

**Interfaces:**
- Consumes: `provider_http::HttpClient`, existing Forgejo local server behavior.
- Produces: deterministic fixture loading and local request assertions for GitLab and Bitbucket tests.

- [ ] **Step 1: Write a failing fixture-loading test**

  Add a test-support function with an explicit path and expected JSON value:

  ```rust
  pub(crate) fn fixture(path: &str) -> serde_json::Value;
  ```

  The test must fail until the fixture loader exists and must reject missing fixture files with a useful path.

- [ ] **Step 2: Extract or implement the local HTTP server helper**

  Provide a test-only server that can inspect method/path/query/headers and return queued responses. It must bind to an ephemeral localhost port and shut down when dropped.

- [ ] **Step 3: Add representative fixture files**

  Add small, sanitized fixtures for project metadata, branches, pull/merge requests, issues, releases, reviewers, and CI. Keep each response focused on fields the mapper consumes; add malformed and empty fixtures only where a test needs them.

- [ ] **Step 4: Run the fixture/helper tests**

  Run: `cargo test test_support -- --test-threads=1`

  Run: `cargo test --all-targets -- --test-threads=1`

  Expected: helper tests pass and existing tests remain green.

- [ ] **Step 5: Commit the test infrastructure**

  ```bash
  git add src/lib.rs src/forgejo.rs tests/fixtures
  git commit -m "test(provider): add shared fixtures and mock server helpers"
  ```

### Task 3: Implement GitLab JSON mapping

**Files:**
- Create: `src/gitlab.rs`
- Modify: `src/lib.rs`
- Test: `src/gitlab.rs` unit tests and `tests/fixtures/gitlab/*.json`

**Interfaces:**
- Consumes: `model::{Item, Observation, RemoteState, Repo}`, shared text/label normalization helpers, fixture loader.
- Produces: pure GitLab response mappers and `pub struct Gitlab` implementing `RemoteProvider` in a later step.

- [ ] **Step 1: Define mapper input/output tests before implementation**

  Add focused tests for:

  ```rust
  #[test]
  fn project_maps_default_branch_and_urls() { /* ... */ }

  #[test]
  fn merge_requests_map_open_drafts_reviewers_and_release_evidence() { /* ... */ }

  #[test]
  fn missing_optional_fields_do_not_fail_the_snapshot() { /* ... */ }

  #[test]
  fn malformed_required_project_fields_return_an_error() { /* ... */ }
  ```

  Assert exact titles, URLs, details, draft handling, labels, reviewer identity, and release evidence.

- [ ] **Step 2: Run the GitLab mapper tests and confirm they fail**

  Run: `cargo test gitlab::tests -- --test-threads=1`

  Expected: failure because mapper functions and `src/gitlab.rs` do not exist.

- [ ] **Step 3: Implement small pure mapping functions**

  Keep endpoint JSON values at the module boundary and return neutral values. Use explicit required-field errors for project identity/default branch and tolerant defaults for optional display fields. Do not call the network from mapper tests.

- [ ] **Step 4: Add GitLab capability decisions**

  Implement helpers that return `Observation::unsupported()` for no-equivalent capabilities and populate `prs`, `drafts`, `proposals`, `published`, and `ci` only from fields covered by fixtures.

- [ ] **Step 5: Run mapper tests and commit**

  Run: `cargo test gitlab::tests -- --test-threads=1`

  ```bash
  git add src/gitlab.rs src/lib.rs tests/fixtures/gitlab
  git commit -m "feat(provider): add GitLab response mappings"
  ```

### Task 4: Implement GitLab HTTP snapshot and detection

**Files:**
- Modify: `src/gitlab.rs`
- Modify: `src/provider.rs`
- Modify: `src/registry.rs`
- Modify: `src/main.rs` only if CLI conversion needs correction
- Test: `src/gitlab.rs` mock-server tests and `tests/gitlab_workflow.rs`

**Interfaces:**
- Consumes: Task 1 HTTP client, Task 2 mock server, Task 3 mappers, existing `RemoteProvider`/`SnapshotFuture` contract.
- Produces: `Gitlab` adapter selected by `provider::adapter`, hosted host detection, and optional live smoke-test entry points.

- [ ] **Step 1: Add failing mock snapshot tests**

  Add tests for a complete snapshot, a paginated collection, a single endpoint failure, malformed JSON, missing token behavior, and a nested namespace project path. Assert that the server saw the expected request paths, query parameters, and `Authorization` header.

- [ ] **Step 2: Run the GitLab HTTP tests and confirm failure**

  Run: `cargo test gitlab -- --test-threads=1`

  Expected: failure because the adapter is not dispatched and request methods are absent.

- [ ] **Step 3: Implement endpoint configuration and authentication**

  Add defaults and environment overrides:

  ```rust
  const DEFAULT_BASE_URL: &str = "https://gitlab.com/api/v4/";
  const TOKEN_ENV: &str = "PHROURION_GITLAB_TOKEN";
  const BASE_URL_ENV: &str = "PHROURION_GITLAB_BASE_URL";
  ```

  Use a bearer token when present; preserve public-read behavior where the provider allows it. Build API paths by URL-encoding the full nested project path in the adapter.

- [ ] **Step 4: Implement `Gitlab::snapshot`**

  Fetch independent observations concurrently with `tokio::join!`, map each response independently, and preserve successful observations when a sibling endpoint fails. Ensure unsupported capabilities are explicit, not empty successful lists.

- [ ] **Step 5: Register the adapter and verify host detection**

  Add `ProviderKind::Gitlab => Box::new(crate::gitlab::Gitlab)` to `provider::adapter`. Add/extend registry tests for `https://gitlab.com/group/subgroup/repo.git`, SSH URLs, and unknown-host explicit provider overrides.

- [ ] **Step 6: Add optional live smoke-test gating**

  Add `tests/gitlab_workflow.rs` with a skip guard requiring `PHROURION_GITLAB_TEST_URL` and `PHROURION_GITLAB_TOKEN`. Keep it read-only and assert only stable baseline behavior such as a successful default branch and snapshot request.

- [ ] **Step 7: Run verification and commit**

  Run: `cargo fmt --check`

  Run: `cargo test gitlab registry provider -- --test-threads=1`

  ```bash
  git add src/gitlab.rs src/provider.rs src/registry.rs src/main.rs tests/gitlab_workflow.rs
  git commit -m "feat(provider): add GitLab monitoring adapter"
  ```

### Task 5: Implement Bitbucket Cloud JSON mapping

**Files:**
- Create: `src/bitbucket.rs`
- Modify: `src/lib.rs`
- Test: `src/bitbucket.rs` unit tests and `tests/fixtures/bitbucket/*.json`

**Interfaces:**
- Consumes: neutral model types, shared normalization helpers, fixture loader.
- Produces: pure Bitbucket Cloud mappers and explicit unsupported capability decisions.

- [ ] **Step 1: Write failing mapping tests**

  Cover repository metadata, branches, pull requests, reviewers, issues, pipeline status, missing optional values, draft state when exposed, and unsupported releases/publication.

- [ ] **Step 2: Run the focused tests and confirm failure**

  Run: `cargo test bitbucket::tests -- --test-threads=1`

  Expected: failure because the module and mapper functions do not exist.

- [ ] **Step 3: Implement pure mappers**

  Normalize Bitbucket workspace/repository fields into `Item` values. Keep release/publication observations unsupported unless a fixture proves a direct stable mapping. Do not treat an empty response as successful release data.

- [ ] **Step 4: Run mapper tests and commit**

  Run: `cargo test bitbucket::tests -- --test-threads=1`

  ```bash
  git add src/bitbucket.rs src/lib.rs tests/fixtures/bitbucket
  git commit -m "feat(provider): add Bitbucket response mappings"
  ```

### Task 6: Implement Bitbucket Cloud HTTP snapshot and detection

**Files:**
- Modify: `src/bitbucket.rs`
- Modify: `src/provider.rs`
- Modify: `src/registry.rs`
- Test: `src/bitbucket.rs` mock-server tests and `tests/bitbucket_workflow.rs`

**Interfaces:**
- Consumes: shared HTTP client, mock server, Task 5 mappers, provider dispatch contract.
- Produces: `Bitbucket` adapter selected by `provider::adapter`, hosted host detection, and optional live smoke tests.

- [ ] **Step 1: Add failing mock HTTP tests**

  Verify repository metadata, branches, pull requests, issues, and pipelines request paths. Include pagination through Bitbucket’s `next` URL, a missing/invalid token response, malformed JSON, and independent endpoint failure.

- [ ] **Step 2: Run focused tests and confirm failure**

  Run: `cargo test bitbucket -- --test-threads=1`

  Expected: failure because the adapter is not dispatched and endpoint methods are absent.

- [ ] **Step 3: Implement endpoint configuration and auth**

  Add:

  ```rust
  const DEFAULT_BASE_URL: &str = "https://api.bitbucket.org/2.0/";
  const TOKEN_ENV: &str = "PHROURION_BITBUCKET_TOKEN";
  const BASE_URL_ENV: &str = "PHROURION_BITBUCKET_BASE_URL";
  ```

  Support the read-only token authentication form used by the adapter through the shared `Auth` enum, without logging credentials or response bodies that may contain them.

- [ ] **Step 4: Implement `Bitbucket::snapshot`**

  Fetch independent observations concurrently, follow provider pagination links, map successful fields, and mark release/publication unsupported. Do not silently convert authorization failures into empty data.

- [ ] **Step 5: Register the adapter and verify remote parsing**

  Add `ProviderKind::Bitbucket => Box::new(crate::bitbucket::Bitbucket)` to dispatch. Add registry tests for HTTPS and SSH `bitbucket.org/workspace/repository` remotes and explicit override behavior for non-hosted test hosts.

- [ ] **Step 6: Add optional live smoke-test gating**

  Add `tests/bitbucket_workflow.rs` with environment guards requiring `PHROURION_BITBUCKET_TEST_URL` and `PHROURION_BITBUCKET_TOKEN`. Keep assertions read-only and stable.

- [ ] **Step 7: Run verification and commit**

  Run: `cargo fmt --check`

  Run: `cargo test bitbucket registry provider -- --test-threads=1`

  ```bash
  git add src/bitbucket.rs src/provider.rs src/registry.rs tests/bitbucket_workflow.rs
  git commit -m "feat(provider): add Bitbucket Cloud monitoring adapter"
  ```

### Task 7: Document configuration and provider semantics

**Files:**
- Modify: `README.md`
- Modify: `docs/roadmap.md`
- Test: documentation command snippets and `cargo run -- --help`

**Interfaces:**
- Consumes: completed GitLab and Bitbucket adapters and environment variable names.
- Produces: user-facing setup guidance that accurately describes hosted support, optional live tests, unsupported capabilities, and self-hosted endpoint overrides.

- [ ] **Step 1: Write documentation checks/expectations**

  Confirm the README will state that default CI uses local fixtures, live tests are opt-in, tokens are not stored in `repos.toml`, and Bitbucket Server/Data Center is not supported by this milestone.

- [ ] **Step 2: Update README provider support and setup sections**

  Document automatic detection, `--provider`, token variables, base URL overrides, and the capability differences users will see in the TUI.

- [ ] **Step 3: Update the roadmap**

  Mark GitLab and Bitbucket Cloud shipped and retain clearly scoped follow-ups for self-hosted discovery, Bitbucket Server/Data Center, and any missing capability mappings.

- [ ] **Step 4: Verify docs and CLI help**

  Run: `cargo run -- --help`

  Run: `cargo run -- add --help`

  Verify all environment names and provider enum values match the implementation exactly.

- [ ] **Step 5: Commit documentation**

  ```bash
  git add README.md docs/roadmap.md
  git commit -m "docs: document GitLab and Bitbucket providers"
  ```

### Task 8: Full verification, resilience review, and release handoff

**Files:**
- Modify: any provider/test files required by verification findings
- Test: entire repository test and quality suite

**Interfaces:**
- Consumes: all previous tasks.
- Produces: a merge-ready provider expansion with evidence for coverage, mutation resilience, screenshots, and release automation.

- [ ] **Step 1: Run formatting, lint, and all tests**

  ```bash
  cargo fmt --check
  cargo clippy --all-targets --all-features -- -D warnings
  cargo test --all-targets -- --test-threads=1
  ```

  Expected: all existing and new tests pass without provider credentials.

- [ ] **Step 2: Run deterministic screenshot verification**

  Run: `scripts/check-screenshot.sh`

  Expected: dashboard and help-modal screenshots match fresh output.

- [ ] **Step 3: Run coverage and mutation checks**

  Run the repository’s existing coverage script and Chaos MCP audits for `src/provider.rs`, `src/provider_http.rs`, `src/gitlab.rs`, `src/bitbucket.rs`, `src/forgejo.rs`, and `src/registry.rs`. Add tests for every high-severity surviving mutant in provider dispatch, auth, pagination termination, capability support flags, and malformed-response handling.

- [ ] **Step 4: Review architecture and test impact**

  Run Knossos review against the base branch and verify new provider modules do not create boundary violations or route provider-specific response types into the TUI/registry layers. Run the impacted test set before the full suite if further edits are required.

- [ ] **Step 5: Verify optional live tests remain opt-in**

  Run the full default suite with all provider variables unset. Confirm live test files skip cleanly and no test attempts network access by default.

- [ ] **Step 6: Commit any final verification fixes separately**

  Use a focused commit message such as:

  ```bash
  git commit -m "test(provider): harden multi-provider verification"
  ```

- [ ] **Step 7: Prepare integration and release**

  Push the branch, open a PR with the spec’s acceptance criteria summarized, wait for CI, merge when green, and allow release-please to create the version PR. Merge the release PR only after its generated changelog and CI checks pass.
