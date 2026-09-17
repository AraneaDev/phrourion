# Forgejo adapter design

Status: approved, ready for implementation planning.

## Context

Phrourion's remote data comes from a `RemoteProvider` trait (`src/provider.rs`)
with one implementation today: `Github`, which shells out to the `gh` CLI.
`docs/design.md` already anticipates other providers behind the same
boundary, and `docs/roadmap.md` lists GitLab/Forgejo/Bitbucket adapters as
deprioritized while the GitHub-only backlog was built out. That backlog is
now done; this spec covers the first of the three, Forgejo, chosen because
it's the one we can stand up and test locally with Docker.

GitLab and Bitbucket are explicitly out of scope for this spec — each gets
its own design once picked up (GitLab in particular may use `glab api`
instead of raw HTTP, which needs checking before assuming it follows this
same shape).

## Scope

**In v1:**
- Default branch
- Branches
- Open pull requests, plus release-proposal evidence (reusing
  `provider::release_evidence` unchanged)
- Open issues, filtered by `Repo.issue_labels` (reusing
  `provider::matches_issue_labels` unchanged), server-side excluded from PRs
  via `?type=issues`
- Releases, split into drafts/published
- CI, via the combined commit-status endpoint

**Deferred, not in v1:** `publication` (release workflow runs) stays
`Observation::unsupported()` for Forgejo. Native Forgejo Actions' run-listing
API shape hasn't been verified against a live instance; adding it later is a
small, isolated follow-up once that's checked, not a reason to block this
work.

## Architecture

- New module `src/forgejo.rs`, implementing `RemoteProvider` (`src/provider.rs`),
  same shape as the existing `Github` struct/impl.
- `provider::adapter()` gains: `ProviderKind::Forgejo => Box::new(forgejo::Forgejo)`.
- No changes to `src/model.rs` — `Item`, `Observation<T>`, `RemoteState` are
  already provider-neutral.
- A single `reqwest::Client` is built lazily once (`std::sync::OnceLock` or
  equivalent) and reused across calls, rather than constructed per snapshot,
  so connection pooling survives across refresh cycles.

## Auth & transport

- Base URL: `https://{repo.identity.host}/api/v1`.
- Test/dev override: if the env var `PHROURION_FORGEJO_TEST_URL` is set, it
  replaces the base URL verbatim (e.g. `http://localhost:3000`) regardless of
  `repo.identity.host`. Same escape-hatch pattern as the existing
  `PHROURION_GH` env var that overrides the `gh` executable for tests.
- Token: env var `PHROURION_TOKEN_<HOST>`, where `<HOST>` is
  `repo.identity.host` uppercased with every non-alphanumeric character
  replaced by `_` (e.g. `codeberg.org` → `PHROURION_TOKEN_CODEBERG_ORG`).
  `repos.toml` never stores credentials, matching the existing registry
  design principle.
- If the token env var is unset, requests are sent **unauthenticated** rather
  than failing outright — public Forgejo/Codeberg repos are readable without
  a token, so this keeps the zero-config case working. When present, sent as
  header `Authorization: token <value>`.
- Pagination: applies only to the four list endpoints (branches, pulls,
  issues, releases) — `default_branch` and the commit-status lookup are
  single-object endpoints with nothing to paginate. Forgejo/Gitea's list
  endpoints take `page`/`limit` query params and don't reliably self-report
  total count, so the client loops `page=1, 2, …` with `limit=50`, stopping
  when a page's row count is less than `limit` (including an empty final
  page).
- JSON responses are parsed as `serde_json::Value`, matching the existing
  GitHub adapter's style (not fixed structs) — this lets `forgejo.rs` reuse
  `provider`'s existing `text()`, `release_evidence`, and
  `matches_issue_labels` helpers directly instead of re-deriving them for a
  typed model.

## Data mapping

Endpoints (Forgejo/Gitea v1 API), all under the base URL from above, with
`{base}` = `repos/{owner}/{repo}`:

| Data | Endpoint | Notes |
|---|---|---|
| default branch | `GET {base}` | `default_branch` field |
| branches | `GET {base}/branches` | commit sha is at `commit.id`, **not** `commit.sha` (differs from GitHub) |
| pull requests | `GET {base}/pulls?state=open` | `head`/`base` use `ref`/`sha` (matches GitHub's shape here) |
| issues | `GET {base}/issues?state=open&type=issues` | `type=issues` excludes PRs server-side, no client-side `pull_request`-field filtering needed (unlike GitHub) |
| releases | `GET {base}/releases` | `tag_name`, `html_url`, `draft`, `published_at` (matches GitHub's shape) |
| CI | `GET {base}/commits/{ref}/status` | `{ref}` accepts the default branch name directly — no separate SHA-resolution call needed, unlike the GitHub adapter |

Item construction reuses the same field-mapping shape as the GitHub adapter's
`pr_item`/`issue_item`/release mapping closures, adjusted only for the field
name differences above.

## Error handling

- Non-2xx HTTP response: Forgejo/Gitea error bodies are typically
  `{"message": "..."}`; surface that message (or the raw body if it doesn't
  parse) via `Observation::failure`.
- No rate-limit-specific retry logic in v1 (unlike the GitHub adapter's
  backoff on 403/429) — self-hosted Forgejo instances don't commonly
  rate-limit API access the way GitHub does, and there's no evidence this is
  needed yet. Add it later if it turns out to matter.
- Connection failures (instance unreachable) surface as a normal
  `Observation::failure`, same as any other fetch error.

## Dependency

```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls"] }
```
`default-features = false` avoids pulling in `native-tls`/OpenSSL; `rustls-tls`
keeps the build self-contained the way the rest of the toolchain already is
(no system TLS dependency anywhere else in the project).

## Test infrastructure

- `docker-compose.forgejo.yml` at the repo root (dev-only, not part of the
  shipped crate): a single `codeberg.org/forgejo/forgejo` service on
  `localhost:3000` with `INSTALL_LOCK=true` so it starts pre-configured.
- `scripts/forgejo-dev.sh`: starts the container, waits for it to be ready,
  then uses the Forgejo API itself to create a test user, a personal access
  token, a repo, and an open PR + issue (mirroring the GitHub adapter's
  `tests/git_workflow.rs` fixture style, which drives real git operations
  rather than mocking). Prints the `export PHROURION_FORGEJO_TEST_URL=...`
  and `export PHROURION_TOKEN_...=...` lines to run.
- Integration tests (new `tests/forgejo_workflow.rs`, mirroring
  `tests/git_workflow.rs`) check `PHROURION_FORGEJO_TEST_URL` at the top of
  each test and skip with a printed reason if it's unset, so `cargo test`
  stays green with no Docker running.
- Not wired into CI yet (per decision this round) — local-only until the
  adapter is proven out.
- Pure-function unit tests (field-mapping closures, pagination-stop logic,
  the host-to-env-var-name transform) follow the same style as the existing
  `provider::tests` module — no network needed for those.

## Non-goals

- GitLab and Bitbucket adapters (separate specs).
- Forgejo Actions workflow run listing (`publication` field) — deferred, see Scope.
- Rate-limit backoff for the Forgejo adapter — deferred, see Error handling.
- Wiring the Docker-based tests into CI — deferred, see Test infrastructure.
