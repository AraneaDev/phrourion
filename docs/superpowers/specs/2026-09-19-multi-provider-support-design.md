# Multi-Provider Support Design

## Goal

Add first-class remote monitoring for GitLab.com and Bitbucket Cloud without requiring paid services, while preserving the existing GitHub, Forgejo, and TUI behavior.

The first milestone must be testable entirely with local fixtures and mock HTTP servers. Optional live smoke tests may use free hosted accounts, but no CI test may require an account, paid plan, or network access.

## Scope

### Included

- GitLab.com support.
- Bitbucket Cloud support.
- Configurable provider base URLs so the adapters can later target self-hosted-compatible deployments.
- Automatic detection for `gitlab.com` and `bitbucket.org`.
- Explicit `--provider gitlab` or `--provider bitbucket` selection for other hosts.
- Shared HTTP request, authentication, error, pagination, and fixture-test utilities.
- Provider-specific response mapping into the existing neutral `RemoteState` model.
- Deterministic unit and mock-server integration tests.
- Optional, environment-gated live smoke tests.
- README, CLI help, roadmap, and release documentation updates.

### Excluded from the first milestone

- A new UI capability framework.
- Write operations against any hosting provider.
- Automatic discovery of self-hosted provider type.
- A dedicated self-hosted GitLab or Bitbucket deployment in CI.
- Treating Bitbucket Server/Data Center as equivalent to Bitbucket Cloud.
- Inventing release semantics for provider APIs that do not expose a reliable equivalent.

`ProviderKind::BitbucketServer` may remain in the model for compatibility, but it remains unsupported until a separate adapter design exists.

## Current architecture and constraints

The application already has the appropriate primary seam:

```rust
pub trait RemoteProvider: Send + Sync {
    fn snapshot<'a>(&'a self, repo: &'a Repo) -> SnapshotFuture<'a>;
}
```

`ProviderKind` is already persisted in repository registry entries, and `RemoteState` already exposes neutral observations for branches, pull requests, review requests, issues, proposals, drafts, published releases, CI, and publication evidence. The UI consumes only this neutral model.

The current provider implementation combines GitHub-specific CLI transport, JSON handling, pagination, normalization, and tests in `src/provider.rs`; Forgejo has a separate HTTP adapter in `src/forgejo.rs`. The expansion will extract reusable HTTP concerns without forcing a broad rewrite of the neutral model or UI.

## Architecture

### Modules

The target module layout is:

```text
src/
  provider.rs          # trait, adapter dispatch, neutral helpers
  provider_http.rs     # shared HTTP transport, auth, errors, pagination
  github.rs            # GitHub adapter; migrated incrementally from provider.rs
  forgejo.rs           # existing Forgejo adapter, adopting shared conventions
  gitlab.rs            # GitLab REST adapter
  bitbucket.rs         # Bitbucket Cloud REST adapter
```

The exact GitHub split may be staged if moving it immediately creates unnecessary risk. The required architectural boundary is that new providers do not add more provider-specific branches to a shared implementation function.

### Data flow

```text
registry::parse_remote
        |
        v
     Repo.identity.kind + host + project
        |
        v
provider::adapter(kind)
        |
        v
provider adapter -> provider_http request/pagination helpers
        |
        v
provider-specific JSON mappers
        |
        v
neutral RemoteState -> TUI / JSON status output
```

Provider-specific response types must not cross into the TUI or registry layers.

## Capability mapping

Adapters preserve the existing `RemoteState` shape. Each observation is either populated with normalized data, reports an error, or is explicitly `unsupported()` when the provider has no reliable equivalent.

### GitLab

- Project metadata default branch → `default_branch`.
- Repository branches → `branches`.
- Open merge requests → `prs`.
- Merge-request reviewers/assignees where exposed → `review_requests`.
- Open issues → `issues`.
- Merge requests/releases matching configured release evidence → `proposals` and `published`.
- Draft merge requests → `drafts`.
- Pipelines or commit status for the default branch → `ci`.
- Release evidence uses configured labels, release branch conventions, and titles only where the GitLab response supplies the required fields.

### Bitbucket Cloud

- Repository metadata default branch → `default_branch`.
- Repository branches → `branches`.
- Open pull requests → `prs`.
- Pull-request reviewers where exposed → `review_requests`.
- Repository issues where enabled → `issues`.
- Bitbucket Pipelines status where available → `ci`.
- Draft pull requests map to `drafts` only when the API exposes an unambiguous draft state.
- Release/publication observations remain unsupported unless a direct, stable Bitbucket Cloud API mapping is established by fixtures and documentation.

The UI must continue to distinguish unsupported from error and loading states using the existing `Observation` behavior.

## Authentication and endpoint configuration

Provider credentials are read at request time from environment variables and are never serialized into the registry:

```text
PHROURION_GITLAB_TOKEN
PHROURION_BITBUCKET_TOKEN
```

Base URL overrides are also environment-driven:

```text
PHROURION_GITLAB_BASE_URL
PHROURION_BITBUCKET_BASE_URL
```

Defaults point to the hosted services. Tests use local mock-server URLs. A base URL override is an endpoint/testing seam, not automatic self-hosted provider discovery; users still select the provider explicitly for unknown hosts.

The shared HTTP layer must centralize:

- authorization header construction;
- URL joining and project-path escaping;
- response status handling;
- bounded response-body diagnostics;
- JSON decoding errors;
- pagination collection;
- provider-independent retry policy only where safe and explicitly configured.

Provider modules own endpoint paths, query parameters, authentication scheme differences, pagination links/cursors, and response field mapping.

## Host detection and CLI behavior

`registry::parse_remote` will continue to detect:

- `gitlab.com` → `ProviderKind::Gitlab`;
- `bitbucket.org` → `ProviderKind::Bitbucket`.

Unknown hosts continue to require `--provider`. The CLI enum already includes both provider names and should gain no new provider-specific flags in this milestone.

Remote project paths must preserve nested GitLab namespaces and Bitbucket workspace/repository identifiers. API URL encoding belongs in the provider adapter, not in registry parsing.

## Testing strategy

### Pure mapping tests

Each provider gets fixtures and focused mapper tests for:

- complete successful responses;
- missing optional fields;
- malformed required fields;
- empty collections;
- draft/open/closed state mapping;
- labels and reviewer mapping;
- CI status normalization;
- pagination response shapes;
- unsupported capability decisions.

### Mock HTTP integration tests

Each adapter is exercised against a local HTTP server using the real request path. Tests verify:

- endpoint paths and query parameters;
- authentication headers without exposing token values in errors;
- base URL overrides;
- pagination traversal and termination;
- individual endpoint failure while other observations still succeed;
- malformed JSON and non-success status handling;
- nested project-path encoding;
- preservation of stale data behavior through the existing snapshot flow.

These tests run by default in CI and require no external network or account.

### Optional live smoke tests

Live tests are opt-in and skipped unless configured:

```text
PHROURION_GITLAB_TEST_URL
PHROURION_GITLAB_TOKEN
PHROURION_BITBUCKET_TEST_URL
PHROURION_BITBUCKET_TOKEN
```

They validate authentication and a minimal read-only snapshot against explicitly supplied free test repositories. They must not be required for mergeability.

## Delivery sequence

1. Extract shared HTTP and pagination utilities while preserving current behavior.
2. Establish shared fixture and mock-server test helpers.
3. Migrate or wrap existing Forgejo HTTP behavior onto those conventions.
4. Add GitLab host detection, adapter, fixtures, mock integration tests, and optional smoke test.
5. Add Bitbucket Cloud host detection, adapter, fixtures, mock integration tests, and optional smoke test.
6. Add provider-specific documentation, environment-variable reference, and roadmap updates.
7. Run full tests, Clippy, coverage/mutation checks, screenshot validation, and provider-focused review.
8. Release through the existing release-please workflow.

Each provider should be independently reviewable and mergeable. A failure in Bitbucket-specific behavior must not block GitLab or regress GitHub/Forgejo.

## Acceptance criteria

- A GitLab.com remote is auto-detected and produces a real normalized snapshot when a token is configured.
- A Bitbucket Cloud remote is auto-detected and produces a real normalized snapshot when a token is configured.
- Explicit provider selection works for test/self-hosted base URLs.
- No provider-specific types reach the TUI or registry layers.
- Unsupported capabilities are visibly marked unsupported rather than reported as successful empty data.
- All default tests pass without network credentials.
- Mock integration tests verify requests, auth, pagination, malformed responses, and partial failures.
- Optional live smoke tests are clearly documented and skipped by default.
- Existing GitHub, Forgejo, local, registry, CLI, and TUI tests remain green.
