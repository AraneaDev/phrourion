# Roadmap

Working backlog from brainstorming what comes after the pre-release core (local
state, GitHub PRs/releases/checks, fast-forward pull). Items are grouped, not
ordered by priority within a group; status reflects what has actually shipped.

## More remote data

- **GitHub Issues** — all open issues per repository, shown as their own
  table column and detail tab, purely informational (does not affect the
  Attention ranking, same as open PRs today). *Shipped.*
- **Requested reviews** — pull requests, in any registered repository, where
  the authenticated `gh` user is a requested reviewer. The "waiting on me"
  signal, distinct from the existing PR list. *Not started.*
- **GitLab adapter** — same `RemoteProvider` shape as GitHub's, via `glab`.
  Waits on having a way to test against GitLab (account/token or a local
  instance). *Not started.*
- **Forgejo adapter** — locally testable now; build before GitLab.
  *Not started.*
- **Bitbucket adapter** — locally testable now; build before GitLab.
  *Not started.*

## New actions

- **Checkout a PR's branch locally** — allowed only when the working tree is
  clean, same guard family as the existing fast-forward pull: fetch + switch,
  no stash/reset. *Not started.*

Deliberately dropped this round: creating a branch from an issue (revisit once
Issues support sees real use), and any comment/close/merge action from the TUI
(conflicts with Phrourion's read-only, never-mutates-history identity).

## Ergonomics / attention signal

- **Desktop notification on attention transitions** — notify when a repo
  newly enters a Problem/Pending state (conflict, failed CI, diverged,
  pending release) since the last refresh, while `phro` is running. Not a
  background daemon; no notification if `phro` isn't open. *Not started.*
- **Per-repo label filters** — an `issue_labels` field in `repos.toml`
  mirroring the existing `release_labels` pattern, to narrow the Issues list
  per repository. *Not started.*

Deliberately dropped this round: repo groups/tags — premature until someone's
registry is large enough to need grouping.

## Reliability

- **Rate-limit-aware backoff for `gh api`** — detect GitHub 403/429 responses
  and retry with backoff instead of surfacing a hard failure. Matters more
  once Issues and requested-reviews add API calls per refresh. *Not started.*
