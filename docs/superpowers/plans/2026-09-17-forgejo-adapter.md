# Forgejo Adapter Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a working `RemoteProvider` implementation for Forgejo/Gitea-compatible hosts, so Phrourion can monitor Forgejo repos (default branch, branches, PRs, issues, releases, CI status) the same way it already does GitHub.

**Architecture:** A new `src/forgejo.rs` module implements the existing `RemoteProvider` trait over HTTP (via `reqwest`), reusing GitHub-adapter field-mapping helpers from `src/provider.rs` wherever the JSON shapes already match (PRs, issues, release-evidence, label filtering). `provider::adapter()` routes `ProviderKind::Forgejo` to it. A Docker Compose fixture plus a bootstrap script stand up a real local Forgejo instance for integration tests, which skip cleanly when that instance isn't running.

**Tech Stack:** Rust, tokio (existing), reqwest 0.12 (new, rustls-tls + json features only), serde_json::Value (existing pattern, not typed structs), Docker Compose, jq/curl for the bootstrap script.

**Spec:** `docs/superpowers/specs/2026-09-17-forgejo-adapter-design.md`

## Global Constraints

- `reqwest` must use `default-features = false, features = ["json", "rustls-tls"]` — no OpenSSL/native-tls dependency.
- JSON is parsed as `serde_json::Value`, never typed structs — matches the existing GitHub adapter style and lets this module reuse its helpers.
- Auth token comes from env var `PHROURION_TOKEN_<HOST>` (host uppercased, non-alphanumeric → `_`); requests proceed **unauthenticated** if unset, never fail outright for a missing token.
- Base URL is `https://{host}/api/v1`, overridable wholesale via `PHROURION_FORGEJO_TEST_URL` for local/dev testing (mirrors the existing `PHROURION_GH` override pattern).
- `publication` and `review_requests` are `Observation::unsupported()` for Forgejo in v1 — explicitly out of scope, not an oversight.
- Every task must leave `cargo fmt -- --check`, `cargo clippy --all-targets --all-features -- -D warnings`, and `cargo test` clean before its commit.
- Docker-based integration tests are **not** wired into CI in this plan — they must skip (not fail) when `PHROURION_FORGEJO_TEST_URL` is unset, so plain `cargo test` on a machine without Docker running stays green.

---

## File Structure

- **Modify** `Cargo.toml` — add the `reqwest` dependency.
- **Modify** `src/lib.rs` — register `pub mod forgejo;`.
- **Modify** `src/provider.rs` — widen visibility on `text`, `pr_item`, `issue_item`, `matches_issue_labels` to `pub(crate)`; extract the inline release-item closure into a named `pub(crate) fn release_item`; add the `ProviderKind::Forgejo` arm to `adapter()`.
- **Create** `src/forgejo.rs` — the new adapter: pure helpers (`env_token_var`, `base_url`), the HTTP fetch layer (`get`, `paginated`), and `impl RemoteProvider for Forgejo`.
- **Create** `docker-compose.forgejo.yml` — dev-only local Forgejo instance.
- **Create** `scripts/forgejo-dev.sh` — starts the container, bootstraps an admin user/token/repo/PR/issue, prints the env exports to run.
- **Create** `tests/forgejo_workflow.rs` — integration tests against the live instance, skipping when `PHROURION_FORGEJO_TEST_URL` is unset.
- **Modify** `README.md`, `docs/roadmap.md` — document the new provider and its env vars; update roadmap status.

---

## Task 1: Widen provider.rs helper visibility for cross-module reuse

**Files:**
- Modify: `src/provider.rs:96` (`text`), `src/provider.rs:130` (`pr_item`), `src/provider.rs:150` (`matches_issue_labels`), `src/provider.rs:162` (`issue_item`), `src/provider.rs:276-300` (extract `release_item`)

**Interfaces:**
- Consumes: nothing new.
- Produces (for later tasks to import as `crate::provider::...`):
  - `pub(crate) fn text(v: &serde_json::Value, key: &str) -> String`
  - `pub(crate) fn pr_item(v: &serde_json::Value) -> crate::model::Item`
  - `pub(crate) fn issue_item(v: &serde_json::Value) -> crate::model::Item`
  - `pub(crate) fn matches_issue_labels(v: &serde_json::Value, labels: &[String]) -> bool`
  - `pub(crate) fn release_item(v: &serde_json::Value) -> crate::model::Item`
  - `pub fn release_evidence(...)` (already public, unchanged)

- [ ] **Step 1: Widen the four existing `fn` declarations to `pub(crate) fn`**

In `src/provider.rs`, change these four signatures (bodies unchanged):

```rust
pub(crate) fn text(v: &Value, key: &str) -> String {
```
```rust
pub(crate) fn pr_item(v: &Value) -> Item {
```
```rust
pub(crate) fn matches_issue_labels(v: &Value, labels: &[String]) -> bool {
```
```rust
pub(crate) fn issue_item(v: &Value) -> Item {
```

- [ ] **Step 2: Extract the inline release-item closure into a named function**

Replace this block (currently around line 276-300):

```rust
            match releases.and_then(|v| flatten_pages(v, None)) {
                Ok(rows) => {
                    let item = |v: &Value| Item {
                        title: text(v, "tag_name"),
                        url: text(v, "html_url"),
                        detail: format!("published {}", text(v, "published_at")),
                    };
                    state.drafts = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == true)
                            .map(item)
                            .collect(),
                    );
                    state.published = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == false)
                            .map(item)
                            .collect(),
                    );
                }
                Err(e) => {
                    state.drafts = Observation::failure(&e);
                    state.published = Observation::failure(e);
                }
            }
```

with:

```rust
            match releases.and_then(|v| flatten_pages(v, None)) {
                Ok(rows) => {
                    state.drafts = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == true)
                            .map(release_item)
                            .collect(),
                    );
                    state.published = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == false)
                            .map(release_item)
                            .collect(),
                    );
                }
                Err(e) => {
                    state.drafts = Observation::failure(&e);
                    state.published = Observation::failure(e);
                }
            }
```

Then add the extracted function near the other `*_item` functions (e.g. right after `issue_item`):

```rust
pub(crate) fn release_item(v: &Value) -> Item {
    Item {
        title: text(v, "tag_name"),
        url: text(v, "html_url"),
        detail: format!("published {}", text(v, "published_at")),
    }
}
```

- [ ] **Step 3: Verify nothing broke**

Run: `cargo build && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: builds clean, 28 lib tests + 13 integration tests still pass (same counts as before this change — this step only changes visibility and extracts a function, no behavior change).

- [ ] **Step 4: Commit**

```bash
git add src/provider.rs
git commit -m "refactor: widen provider helper visibility for cross-adapter reuse"
```

---

## Task 2: Add reqwest, register the module, and the pure helper functions

**Files:**
- Modify: `Cargo.toml`
- Modify: `src/lib.rs`
- Create: `src/forgejo.rs`
- Test: inline `#[cfg(test)] mod tests` in `src/forgejo.rs`

**Interfaces:**
- Consumes: nothing.
- Produces (for Task 3/4 to use, and for Task 6's integration test, which
  compiles as a separate external crate and therefore needs `env_token_var`
  fully `pub`, not `pub(crate)`):
  - `pub fn env_token_var(host: &str) -> String`
  - `pub(crate) fn base_url(host: &str) -> String`

- [ ] **Step 1: Add the dependency**

In `Cargo.toml`, in the `[dependencies]` section, add:

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```

- [ ] **Step 2: Register the module**

In `src/lib.rs`, add (alphabetically, after `command`):

```rust
pub mod command;
pub mod forgejo;
pub mod git;
pub mod model;
pub mod provider;
pub mod registry;
pub mod tui;
```

- [ ] **Step 3: Write the failing tests for the pure helpers**

Create `src/forgejo.rs` with just the test module first:

```rust
//! Forgejo/Gitea-compatible hosting adapter. HTTP-based: no gh-equivalent CLI
//! exists for Forgejo, so this talks to the REST API directly.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_token_var_uppercases_the_host_and_replaces_non_alphanumerics() {
        assert_eq!(env_token_var("codeberg.org"), "PHROURION_TOKEN_CODEBERG_ORG");
        assert_eq!(
            env_token_var("forge.example.com:3000"),
            "PHROURION_TOKEN_FORGE_EXAMPLE_COM_3000"
        );
    }

    #[test]
    fn base_url_defaults_to_https_and_honors_the_test_override() {
        // SAFETY: single-threaded test process for env var mutation; no other
        // test in this crate reads PHROURION_FORGEJO_TEST_URL.
        unsafe {
            std::env::remove_var("PHROURION_FORGEJO_TEST_URL");
        }
        assert_eq!(base_url("codeberg.org"), "https://codeberg.org/api/v1");
        unsafe {
            std::env::set_var("PHROURION_FORGEJO_TEST_URL", "http://localhost:3000");
        }
        assert_eq!(base_url("codeberg.org"), "http://localhost:3000/api/v1");
        unsafe {
            std::env::remove_var("PHROURION_FORGEJO_TEST_URL");
        }
    }
}
```

- [ ] **Step 4: Run the tests to verify they fail to compile**

Run: `cargo test --lib forgejo`
Expected: FAIL — `env_token_var` and `base_url` are not defined.

- [ ] **Step 5: Implement the two helpers**

Add above the test module in `src/forgejo.rs`:

```rust
pub fn env_token_var(host: &str) -> String {
    let normalized: String = host
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("PHROURION_TOKEN_{normalized}")
}

pub(crate) fn base_url(host: &str) -> String {
    let root = std::env::var("PHROURION_FORGEJO_TEST_URL")
        .unwrap_or_else(|_| format!("https://{host}"));
    format!("{root}/api/v1")
}
```

`PHROURION_FORGEJO_TEST_URL` is the bare instance root (e.g.
`http://localhost:3000`, no `/api/v1` suffix) — `/api/v1` is always appended
by this function, in both branches. This matches Task 5's bootstrap script,
which prints the bare root, and its manual verification `curl` commands,
which append `/api/v1` themselves on top of `$PHROURION_FORGEJO_TEST_URL`.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test --lib forgejo`
Expected: PASS, 2 tests.

Then run the full check: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: all clean; lib test count is now 30 (28 + 2 new).

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml Cargo.lock src/lib.rs src/forgejo.rs
git commit -m "feat: scaffold the Forgejo adapter module with its pure helpers"
```

---

## Task 3: HTTP fetch layer (`get`, `paginated`)

**Files:**
- Modify: `src/forgejo.rs`

**Interfaces:**
- Consumes: `env_token_var`, `base_url` from Task 2.
- Produces (for Task 4):
  - `pub(crate) fn client() -> &'static reqwest::Client`
  - `pub(crate) async fn get(host: &str, path: &str, query: &[(&str, &str)]) -> anyhow::Result<serde_json::Value>`
  - `pub(crate) async fn paginated(host: &str, path: &str, query: &[(&str, &str)]) -> anyhow::Result<Vec<serde_json::Value>>`

No unit tests in this task: both functions make real HTTP calls, and the project's existing convention (see `provider::api` in `src/provider.rs`, which shells out to `gh` and has no direct unit test either — only the pure helpers around it are tested) is to cover network-touching code via the integration tests in Task 6, not mocked unit tests. Correctness here is verified end-to-end in Task 6.

- [ ] **Step 1: Add the imports**

The file currently starts with a module doc comment (the two `//!` lines)
followed by a blank line, then `#[cfg(test)] mod tests { ... }`. Insert the
imports after that blank line, before `#[cfg(test)]`, so the top of the file
reads:

```rust
//! Forgejo/Gitea-compatible hosting adapter. HTTP-based: no gh-equivalent CLI
//! exists for Forgejo, so this talks to the REST API directly.

use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;
use std::sync::OnceLock;

#[cfg(test)]
mod tests {
```

(The doc comment and the `#[cfg(test)] mod tests { ... }` block's contents
are unchanged from Task 2 — only the `use` block is new, inserted between
them.)

- [ ] **Step 2: Implement the shared client and `get`**

Add above the `#[cfg(test)]` block:

```rust
pub(crate) fn client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(Client::new)
}

pub(crate) async fn get(host: &str, path: &str, query: &[(&str, &str)]) -> Result<Value> {
    let url = format!("{}/{path}", base_url(host));
    let mut request = client().get(&url).query(query);
    if let Ok(token) = std::env::var(env_token_var(host)) {
        request = request.header("Authorization", format!("token {token}"));
    }
    let response = request.send().await.context("Forgejo request failed")?;
    let status = response.status();
    let body: Value = response.json().await.context("Invalid Forgejo JSON")?;
    if !status.is_success() {
        let message = body["message"].as_str().unwrap_or("Forgejo API error");
        bail!("{status}: {message}");
    }
    Ok(body)
}
```

- [ ] **Step 3: Implement `paginated`**

Add directly below `get`:

```rust
pub(crate) async fn paginated(host: &str, path: &str, query: &[(&str, &str)]) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    let mut page: u32 = 1;
    loop {
        let page_str = page.to_string();
        let mut full_query: Vec<(&str, &str)> = query.to_vec();
        full_query.push(("page", &page_str));
        full_query.push(("limit", "50"));
        let value = get(host, path, &full_query).await?;
        let batch = value.as_array().context("Expected a JSON array")?.clone();
        let count = batch.len();
        rows.extend(batch);
        if count < 50 {
            break;
        }
        page += 1;
    }
    Ok(rows)
}
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo build`
Expected: succeeds. `get` and `paginated` are not yet called from anywhere, so expect an `unused` warning — that's fine, it resolves once Task 4 wires them in. Don't run clippy with `-D warnings` yet for that reason.

- [ ] **Step 5: Commit**

```bash
git add src/forgejo.rs
git commit -m "feat: add the Forgejo adapter's HTTP fetch and pagination layer"
```

---

## Task 4: `Forgejo::snapshot` and wiring into `provider::adapter`

**Files:**
- Modify: `src/forgejo.rs`
- Modify: `src/provider.rs`

**Interfaces:**
- Consumes: `get`, `paginated`, `client` from Task 3; `provider::{text, pr_item, issue_item, matches_issue_labels, release_evidence, release_item}` from Task 1.
- Produces: `pub struct Forgejo;` implementing `crate::provider::RemoteProvider`, wired into `provider::adapter()`.

- [ ] **Step 1: Add the remaining imports**

In `src/forgejo.rs`, extend the import block from Task 3 to:

```rust
use crate::{
    model::{Item, Observation, RemoteState, Repo},
    provider::{self, RemoteProvider, SnapshotFuture},
};
use anyhow::{Context, Result, bail};
use reqwest::Client;
use serde_json::Value;
use std::sync::OnceLock;
```

- [ ] **Step 2: Implement `Forgejo` and its `RemoteProvider` impl**

Add this above the `#[cfg(test)]` block, after `paginated`:

```rust
pub struct Forgejo;

impl RemoteProvider for Forgejo {
    fn snapshot<'a>(&'a self, repo: &'a Repo) -> SnapshotFuture<'a> {
        Box::pin(async move {
            let host = repo.identity.host.as_str();
            let project = repo.identity.project.as_str();
            let mut state = RemoteState::default();

            state.default_branch = match get(host, &format!("repos/{project}"), &[]).await {
                Ok(v) => match v["default_branch"].as_str() {
                    Some(branch) => Observation::success(branch.to_string()),
                    None => Observation::failure("Missing default branch"),
                },
                Err(e) => Observation::failure(e),
            };

            let (branches, pulls, issues, releases) = tokio::join!(
                paginated(host, &format!("repos/{project}/branches"), &[]),
                paginated(
                    host,
                    &format!("repos/{project}/pulls"),
                    &[("state", "open")]
                ),
                paginated(
                    host,
                    &format!("repos/{project}/issues"),
                    &[("state", "open"), ("type", "issues")]
                ),
                paginated(host, &format!("repos/{project}/releases"), &[]),
            );

            state.branches = match branches {
                Ok(rows) => Observation::success(
                    rows.iter()
                        .map(|v| Item {
                            title: provider::text(v, "name"),
                            detail: v["commit"]["id"].as_str().unwrap_or("?").into(),
                            url: String::new(),
                        })
                        .collect(),
                ),
                Err(e) => Observation::failure(e),
            };

            match pulls {
                Ok(rows) => {
                    state.prs = Observation::success(rows.iter().map(provider::pr_item).collect());
                    state.proposals = Observation::success(
                        rows.iter()
                            .filter_map(|v| {
                                provider::release_evidence(v, &repo.release_labels).map(|evidence| {
                                    let mut item = provider::pr_item(v);
                                    item.detail = format!("{evidence} | {}", item.detail);
                                    item
                                })
                            })
                            .collect(),
                    );
                }
                Err(e) => {
                    state.prs = Observation::failure(&e);
                    state.proposals = Observation::failure(e);
                }
            }

            state.issues = match issues {
                Ok(rows) => Observation::success(
                    rows.iter()
                        .filter(|v| provider::matches_issue_labels(v, &repo.issue_labels))
                        .map(provider::issue_item)
                        .collect(),
                ),
                Err(e) => Observation::failure(e),
            };

            match releases {
                Ok(rows) => {
                    state.drafts = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == true)
                            .map(provider::release_item)
                            .collect(),
                    );
                    state.published = Observation::success(
                        rows.iter()
                            .filter(|v| v["draft"] == false)
                            .map(provider::release_item)
                            .collect(),
                    );
                }
                Err(e) => {
                    state.drafts = Observation::failure(&e);
                    state.published = Observation::failure(e);
                }
            }

            state.ci = if let Some(branch) = &state.default_branch.data {
                let encoded =
                    url::form_urlencoded::byte_serialize(branch.as_bytes()).collect::<String>();
                match get(host, &format!("repos/{project}/commits/{encoded}/status"), &[]).await {
                    Ok(v) => {
                        let items = v["statuses"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|s| Item {
                                title: provider::text(s, "context"),
                                url: provider::text(s, "target_url"),
                                detail: provider::text(s, "state"),
                            })
                            .collect();
                        Observation::success(items)
                    }
                    Err(e) => Observation::failure(e),
                }
            } else {
                Observation::failure("Default branch is unknown")
            };

            state.review_requests = Observation::unsupported();
            state.publication = Observation::unsupported();

            state
        })
    }
}
```

- [ ] **Step 3: Wire it into `provider::adapter`**

In `src/provider.rs`, change:

```rust
pub fn adapter(kind: &ProviderKind) -> Box<dyn RemoteProvider> {
    match kind {
        ProviderKind::Github => Box::new(Github),
        _ => Box::new(Unsupported),
    }
}
```

to:

```rust
pub fn adapter(kind: &ProviderKind) -> Box<dyn RemoteProvider> {
    match kind {
        ProviderKind::Github => Box::new(Github),
        ProviderKind::Forgejo => Box::new(crate::forgejo::Forgejo),
        _ => Box::new(Unsupported),
    }
}
```

- [ ] **Step 4: Verify the full workspace builds and lints clean**

Run: `cargo build && cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: all clean, same test counts as after Task 2 (this task adds no new unit tests — `Forgejo::snapshot` is exercised by Task 6's integration tests).

- [ ] **Step 5: Commit**

```bash
git add src/forgejo.rs src/provider.rs
git commit -m "feat: implement Forgejo::snapshot and wire it into provider::adapter"
```

---

## Task 5: Docker Compose fixture and bootstrap script

**Files:**
- Create: `docker-compose.forgejo.yml`
- Create: `scripts/forgejo-dev.sh`

**Interfaces:**
- Consumes: nothing from earlier tasks.
- Produces: a running Forgejo instance at `http://localhost:3000` with a known admin user/token and a `phro-admin/phro-test-repo` repository containing one open PR (`topic` → `main`) and one open issue, plus printed `export` lines for `PHROURION_FORGEJO_TEST_URL` and `PHROURION_TOKEN_LOCALHOST`.

- [ ] **Step 1: Write the Compose file**

Create `docker-compose.forgejo.yml`:

```yaml
# Dev-only fixture for testing the Forgejo adapter locally. Not part of the
# shipped crate. Start with scripts/forgejo-dev.sh, not directly.
services:
  forgejo:
    image: codeberg.org/forgejo/forgejo:7
    container_name: phrourion-forgejo
    environment:
      - FORGEJO__security__INSTALL_LOCK=true
      - FORGEJO__server__ROOT_URL=http://localhost:3000/
    ports:
      - "3000:3000"
    volumes:
      - phrourion-forgejo-data:/data

volumes:
  phrourion-forgejo-data:
```

- [ ] **Step 2: Write the bootstrap script**

Create `scripts/forgejo-dev.sh`:

```bash
#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

URL=http://localhost:3000
CONTAINER=phrourion-forgejo
ADMIN_USER=phro-admin
ADMIN_PASS=phro-admin-pw-12345
ADMIN_EMAIL=admin@phrourion.test
REPO=phro-test-repo

docker compose -f docker-compose.forgejo.yml up -d

echo "Waiting for Forgejo to respond..." >&2
until curl -sf "$URL/api/healthz" >/dev/null 2>&1; do
  sleep 1
done

if ! docker exec "$CONTAINER" forgejo admin user list 2>/dev/null | grep -q "$ADMIN_USER"; then
  docker exec "$CONTAINER" forgejo admin user create \
    --username "$ADMIN_USER" --password "$ADMIN_PASS" \
    --email "$ADMIN_EMAIL" --admin --must-change-password=false
fi

TOKEN=$(curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/users/$ADMIN_USER/tokens" \
  -H "Content-Type: application/json" \
  -d '{"name":"phrourion-dev","scopes":["write:repository","write:issue"]}' \
  | jq -r '.sha1')

curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST "$URL/api/v1/user/repos" \
  -H "Content-Type: application/json" \
  -d "{\"name\":\"$REPO\",\"auto_init\":true}" >/dev/null 2>&1 || true

CONTENT=$(printf 'hello from the topic branch' | base64 -w0)
curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/repos/$ADMIN_USER/$REPO/contents/topic.txt" \
  -H "Content-Type: application/json" \
  -d "{\"content\":\"$CONTENT\",\"message\":\"feat: topic\",\"branch\":\"main\",\"new_branch\":\"topic\"}" \
  >/dev/null 2>&1 || true

curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/repos/$ADMIN_USER/$REPO/pulls" \
  -H "Content-Type: application/json" \
  -d '{"title":"feat: topic","head":"topic","base":"main"}' \
  >/dev/null 2>&1 || true

curl -sf -u "$ADMIN_USER:$ADMIN_PASS" -X POST \
  "$URL/api/v1/repos/$ADMIN_USER/$REPO/issues" \
  -H "Content-Type: application/json" \
  -d '{"title":"bug: something broke"}' \
  >/dev/null 2>&1 || true

echo ""
echo "Forgejo is up. Run:"
echo "  export PHROURION_FORGEJO_TEST_URL=$URL"
echo "  export PHROURION_TOKEN_LOCALHOST=$TOKEN"
echo ""
echo "Test repo identity for fixtures: host=\"localhost\" project=\"$ADMIN_USER/$REPO\""
```

Make it executable:

```bash
chmod +x scripts/forgejo-dev.sh
```

- [ ] **Step 3: Run it and verify by hand**

Run: `./scripts/forgejo-dev.sh`
Expected: prints the two `export` lines with no errors, and the token line
is a non-empty string (not `null` or empty — if it is, the token-creation
response's JSON field isn't actually named `sha1` on this Forgejo version;
drop `-sf` on that one `curl` call temporarily, print the raw response, and
fix the `jq` filter to match the real field name). If any other `curl` step
fails (the non-`|| true` ones), read the response body the same way (drop
`-sf` to see it) and fix the endpoint/field name against what this specific
Forgejo version actually returns — the API shape in this script is based on
documented Gitea/Forgejo API conventions, not verified against a live
instance yet, so this step is the first real check of it.

Then manually verify the fixture data is queryable:

```bash
export PHROURION_FORGEJO_TEST_URL=http://localhost:3000
TOKEN=<the printed token>
curl -sf -H "Authorization: token $TOKEN" "$PHROURION_FORGEJO_TEST_URL/api/v1/repos/phro-admin/phro-test-repo/pulls?state=open" | jq .
curl -sf -H "Authorization: token $TOKEN" "$PHROURION_FORGEJO_TEST_URL/api/v1/repos/phro-admin/phro-test-repo/issues?state=open&type=issues" | jq .
curl -sf -H "Authorization: token $TOKEN" "$PHROURION_FORGEJO_TEST_URL/api/v1/repos/phro-admin/phro-test-repo/branches" | jq '.[0].commit'
curl -sf -H "Authorization: token $TOKEN" "$PHROURION_FORGEJO_TEST_URL/api/v1/repos/phro-admin/phro-test-repo/commits/main/status" | jq .
```
Expected: one open PR, one open issue, branches with a `commit.id` field (not `commit.sha`), and a status object. If the status endpoint's shape differs from `{"statuses": [...]}` assumed in Task 4 Step 2, note the actual shape now — it gets fixed in Task 6 Step 3 once the integration test exercises it directly.

- [ ] **Step 4: Commit**

```bash
git add docker-compose.forgejo.yml scripts/forgejo-dev.sh
git commit -m "chore: add a local Forgejo fixture for adapter testing"
```

---

## Task 6: Integration tests against the live instance

**Files:**
- Create: `tests/forgejo_workflow.rs`

**Interfaces:**
- Consumes: `phrourion::{forgejo, model::{Remote, ProviderKind, Repo}, provider::RemoteProvider}`, the running fixture from Task 5.
- Produces: nothing further downstream — this is the leaf verification task for the adapter itself.

- [ ] **Step 1: Write the skip-gated integration test file**

Create `tests/forgejo_workflow.rs`:

```rust
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
    }
}

macro_rules! require_forgejo {
    () => {
        if std::env::var("PHROURION_FORGEJO_TEST_URL").is_err() {
            eprintln!(
                "skipping: PHROURION_FORGEJO_TEST_URL is unset (run scripts/forgejo-dev.sh)"
            );
            return;
        }
    };
}

#[tokio::test]
async fn snapshot_reports_the_seeded_pr_issue_and_branch() {
    require_forgejo!();
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
    let repo = test_repo("phro-admin/does-not-exist");
    let state = Forgejo.snapshot(&repo).await;
    assert!(state.default_branch.error.is_some());
}

#[tokio::test]
async fn snapshot_works_unauthenticated_for_a_readable_repo() {
    require_forgejo!();
    // SAFETY: single-threaded within this test process's use of this var;
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
```

- [ ] **Step 2: Confirm it skips cleanly without the fixture running**

Run: `unset PHROURION_FORGEJO_TEST_URL; cargo test --test forgejo_workflow`
Expected: PASS (3 tests), each printing a `skipping: ...` line to stderr.

- [ ] **Step 3: Run it against the live fixture and fix any field-name mismatches**

Run:
```bash
./scripts/forgejo-dev.sh
export PHROURION_FORGEJO_TEST_URL=http://localhost:3000
export PHROURION_TOKEN_LOCALHOST=<printed token>
cargo test --test forgejo_workflow -- --nocapture
```
Expected: all 3 tests PASS. If `snapshot_reports_the_seeded_pr_issue_and_branch` fails on the `state.ci` assertion, or any other field comes back empty/wrong, compare against the raw `curl` output from Task 5 Step 3 and fix the mismatched field name in `src/forgejo.rs` (most likely spot: the `state.ci` block's assumption that the combined-status response has a top-level `statuses` array — adjust to match whatever this Forgejo version actually returns, then re-run).

- [ ] **Step 4: Run the full suite once more**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: all clean. Integration test count is now 16 (13 existing + 3 new in `forgejo_workflow.rs`), unchanged lib test count from Task 4 unless Task 3's fix step added anything.

- [ ] **Step 5: Tear down the fixture**

Run: `docker compose -f docker-compose.forgejo.yml down`

- [ ] **Step 6: Commit**

```bash
git add tests/forgejo_workflow.rs src/forgejo.rs
git commit -m "test: add integration tests for the Forgejo adapter"
```

(Include `src/forgejo.rs` in this commit only if Step 3 required a fix; otherwise commit just the test file.)

---

## Task 7: Documentation and roadmap update

**Files:**
- Modify: `README.md`
- Modify: `docs/roadmap.md`

**Interfaces:** none — documentation only.

- [ ] **Step 1: Update the README's provider status**

In `README.md`, find the "Status" section's sentence about which providers are ready (currently reads roughly "GitHub monitoring and the safe fast-forward pull path are ready. GitLab, Bitbucket, and Forgejo adapters are planned..."). Update it to reflect Forgejo now being implemented, e.g.:

```markdown
> **Status:** pre-release. GitHub and Forgejo monitoring and the safe fast-forward
> pull path are ready. GitLab and Bitbucket adapters are planned behind the same
> provider boundary. Phrourion requires Rust 1.85 or newer; GitHub monitoring
> requires an authenticated [GitHub CLI](https://cli.github.com/), and Forgejo
> monitoring requires a personal access token in a `PHROURION_TOKEN_<HOST>`
> environment variable (optional for public repositories).
```

Also add a short paragraph near the GitHub CLI auth instructions (in "Install" or "Use") documenting the Forgejo token env var convention:

```markdown
For a self-hosted or Codeberg Forgejo instance, monitoring a private repository
requires a personal access token in an environment variable named after the
host: uppercase it and replace every non-alphanumeric character with `_`, then
prefix with `PHROURION_TOKEN_`. For example, `codeberg.org` becomes
`PHROURION_TOKEN_CODEBERG_ORG`. Public repositories work without a token.
```

- [ ] **Step 2: Update the roadmap**

In `docs/roadmap.md`, replace the "GitLab, Forgejo, Bitbucket adapters" bullet with two entries — one shipped, one still deprioritized:

```markdown
- **Forgejo adapter** — same `RemoteProvider` shape as GitHub's, over HTTP
  (no bundled CLI equivalent to `gh` exists for Forgejo). *Shipped* (default
  branch, branches, PRs, issues, releases, CI via commit-status; publication
  and review_requests stay `unsupported`; token sourced from a per-host env
  var, optional for public repos; tested against a local Docker fixture, see
  `docs/superpowers/specs/2026-09-17-forgejo-adapter-design.md`).
- **GitLab, Bitbucket adapters** — *Deprioritized:* GitLab may use `glab api`
  instead of raw HTTP like Forgejo's adapter, which needs checking before
  assuming it follows the same shape; Bitbucket Cloud and Server/Data Center
  need separate identification since their APIs differ.
```

- [ ] **Step 3: Verify and commit**

Run: `cargo fmt -- --check && cargo clippy --all-targets --all-features -- -D warnings && cargo test`
Expected: all clean (doc-only change, but keep the habit).

```bash
git add README.md docs/roadmap.md
git commit -m "docs: document the Forgejo adapter and update the roadmap"
```

---

## Self-Review Notes

- **Spec coverage:** Architecture (Task 1, 4), Auth & transport (Task 2, 3), Data mapping table (Task 4, verified live in Task 6), Error handling (Task 3's `get`, Task 6's unknown-repo test), Dependency (Task 2), Test infrastructure (Task 5, 6). All spec sections have a task.
- **Placeholder scan:** no TBD/TODO; the one open question (combined-status response shape) is explicitly flagged as "verify against live instance and fix" rather than hand-waved, with a concrete fallback location named (Task 4 Step 2's `state.ci` block).
- **Type consistency:** `Forgejo` (unit struct) implements `RemoteProvider` from `crate::provider`, matching `Github`'s shape exactly. `env_token_var`/`base_url` signatures in Task 2 match their call sites in Task 3/6. `release_item`/`pr_item`/`issue_item`/`text`/`matches_issue_labels` signatures in Task 1 match their `provider::` call sites in Task 4.
