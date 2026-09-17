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
  signal, distinct from the existing PR list. *Shipped* (individual reviewer
  requests only; team review requests aren't resolved to "is this me").
- **GitLab, Forgejo, Bitbucket adapters** — same `RemoteProvider` shape as
  GitHub's. *Deprioritized:* staying GitHub-only for now; revisit once the
  GitHub-side backlog is done. (A Forgejo API shape was scoped during
  brainstorming — see git history if picking this back up: default branch,
  branches use `commit.id` not `commit.sha`, issues filter server-side via
  `?type=issues`, auth is `Authorization: token <value>` sourced from a
  per-host env var, no bundled CLI equivalent to `gh` so it'd go over HTTP.)

## New actions

- **Checkout a PR's branch locally** — allowed only when the working tree is
  clean, same guard family as the existing fast-forward pull: fetch + switch,
  no stash/reset. *Shipped.* Fetches GitHub's `refs/pull/<number>/head` (works
  for same-repo and fork-originated PRs alike, no fork remote needed) into a
  new local branch `pr/<number>`; refuses if that branch name is already
  taken or the tree isn't clean, and re-verifies nothing changed between
  preview and apply, mirroring `git::preview`/`git::apply` for pull. PR
  selection is a typed number (press `c`), not a selectable list — the
  dashboard has no per-item list selection anywhere yet.

Deliberately dropped this round: creating a branch from an issue (revisit once
Issues support sees real use), and any comment/close/merge action from the TUI
(conflicts with Phrourion's read-only, never-mutates-history identity).

## Ergonomics / attention signal

- **Desktop notification on attention transitions** — notify when a repo
  newly enters a Problem/Pending state (conflict, failed CI, diverged,
  pending release) since the last refresh, while `phro` is running. Not a
  background daemon; no notification if `phro` isn't open. *Shipped*
  (`notify-send` on Linux, `osascript` on macOS, best-effort — silently
  no-ops if neither is installed; fires only when a repo's attention tier
  moves from calm into Problem/Pending, not on every refresh while already
  in that tier; a repo that already has an issue when `phro` first starts
  will notify once on that first observation, since there's no prior state
  to compare against).
- **Per-repo label filters** — an `issue_labels` field in `repos.toml`
  mirroring the existing `release_labels` pattern, to narrow the Issues list
  per repository. *Shipped* (client-side OR match on label name; empty list
  means unfiltered, same default as `release_labels`; no in-TUI editor,
  hand-edit `repos.toml` like the other adapter-tuning fields).

Deliberately dropped this round: repo groups/tags — premature until someone's
registry is large enough to need grouping.

## Reliability

- **Rate-limit-aware backoff for `gh api`** — detect GitHub 403/429 responses
  and retry with backoff instead of surfacing a hard failure. Matters more
  once Issues and requested-reviews add API calls per refresh. *Shipped*
  (up to 3 attempts, 1s/2s exponential backoff, matched on stderr phrasing
  since `gh api` doesn't expose structured status codes to the caller).
