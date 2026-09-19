# Phrourion design

Approved direction: a standalone Rust and Ratatui TUI beside the existing 12 repositories. Its initial registry includes those repositories and Phrourion itself, for 13 entries. GitHub support ships first. Other providers must fit through a stable adapter boundary.

## Scope

Monitor local working trees, branch tracking, remote branches, pull requests, CI, and releases. Add and remove repositories, refresh their state, pull a selected checkout, and open remote pages. Monitoring never pulls automatically.

The first version does not merge or close pull requests, publish releases, push commits, delete branches, stash changes, or resolve conflicts. These can become explicit provider capabilities later.

## Screen

Illustrative data:

```text
Phrourion                                  13 repos  Updated 12s ago

Repository       Branch    Local       Sync      PRs  Releases  CI
Argos-MCP        main      clean       current    0      0      pass
Chaos-MCP        main      clean       behind 2   1      0      running
Knossos-MCP      fix/docs  modified 3  ahead 1    1      0      pass
...
phrourion        main      clean       current    0      0      pass

Chaos-MCP
Folder   /home/tim/Work/AraneaDev/Chaos-MCP
Remote   github.com/AraneaDev/Chaos-MCP
Branches main, fix/execution
Release  latest v5.1.1 | no pending release PRs
Action   Pull main: 2 incoming commits

? help · Enter details · / filter · r refresh · q quit
```

The upper table lists repositories. The lower pane shows the selected repository and its exact folder. Details provide tabs for working tree changes, branches, pull requests, releases, and action output. Narrow terminals hide secondary columns while retaining all fields in details.

Up/Down and `j`/`k` select rows. Enter opens details; Escape returns. Within
details, Left/Right and `h`/`l` select the previous or next tab, matching
Shift-Tab/Tab. The normal footer stays compact and points to the contextual
keyboard reference: `?` or `F1` opens help, including `F1` from text input and
confirmation prompts. Closing help restores the interrupted mode. Confirmation
prompts accept `y` or Enter and cancel with `n` or Escape. The help modal is the
source of truth for the remaining bindings and marks actions that are currently
unavailable.

Use text labels alongside color. Preserve selection while results arrive. Errors are attached to the affected repository and do not prevent navigation.

## Registry and identity

Store machine-specific configuration outside repositories at `$XDG_CONFIG_HOME/phrourion/repos.toml`, falling back to `~/.config/phrourion/repos.toml`. The directory name is plain ASCII. Use the platform configuration directory on platforms without the XDG convention.

Each entry has a stable ID, display name, canonical checkout path, selected Git remote name, provider kind, provider host, repository identifier, and enabled flag. Discover the default branch from the remote; cache it with observation time rather than treating it as permanent configuration. Remote URL changes require revalidation before actions.

Resolve an added subdirectory to its Git worktree root. Canonicalize paths and reject duplicate checkouts. Separate worktrees or clones of the same remote may have separate entries because local state and action targets differ. Share remote reads by provider, host, and repository identity.

If multiple remotes exist, present the detected choices during registration. Support HTTPS and SSH remote URLs, including self-hosted hosts and nested repository paths. Never save credentials embedded in a URL.

The initial setup can discover direct child repositories of `/home/tim/Work/AraneaDev` and present the 13 entries for registration. Do not hardcode that path into the binary. Phrourion is registered by its source checkout path just like any other repository; a binary installation alone cannot identify a source checkout.

Write configuration atomically and protect concurrent updates from lost writes. Missing directories remain visible with a repair-path action. Removing an entry leaves all repository files intact.

CLI surface:

```text
phrourion
phrourion add /path/to/repository
phrourion remove <name-or-id>
```

Ambiguous names require an ID. Add/remove use the same registry service as the TUI.

## State and freshness

Local state includes current branch or detached HEAD, commit ID, staged/unstaged/untracked counts, conflicts, operation-in-progress state, upstream, and ahead/behind counts. An absent upstream is distinct from an up-to-date branch.

Sync compares the checked-out branch with its configured upstream. Default-branch state is shown separately when a feature branch is checked out. Cached remote-tracking refs are identified as cached until a successful fetch. Refreshing GitHub metadata alone does not establish that local Git refs are current.

Branch details list local and remote branches separately, their tracking relationships, and associated pull requests where known. Branch existence does not imply an open pull request or unmerged work.

Every remote collection carries a state: loading, fresh, stale, error, or unsupported. Include the last successful observation time and pagination completeness. A failed, incomplete, unauthorized, or unsupported query cannot produce a displayed count of zero.

CI in the overview refers to the remote default branch and identifies the observed commit in details. Pull request checks belong to each PR's head commit. Distinguish passing, failing, running, absent, and unknown checks.

Local reads refresh every five seconds while the TUI is open; provider reads refresh every sixty seconds. Explicit refresh also fetches the selected remote to update Git tracking refs. Background fetch is off initially. Bound concurrent jobs, time out commands, back off after rate limits, and keep the interface responsive. Cache remote results outside repositories and show their age after an offline restart.

## Release state

Represent three independent categories:

- Open release proposals, including Release Please PRs.
- Draft releases awaiting publication.
- Publication workflows still running or failed.

Show the latest published release separately. Do not count an ordinary source branch, a version difference, or generated version edits as proof of a pending release or runtime fix.

Release proposal detection uses automation labels and branch conventions, with title matching treated as a candidate when evidence is weak. Fetch all open PR pages, so missing labels cannot hide a proposal. Show detection evidence in details. Allow repository-specific release automation rules.

Publication workflow detection uses explicit configured workflow identifiers. Where discovery is uncertain, show the limitation and link to workflows rather than reporting that publication is complete. These categories can overlap, so show their separate counts rather than implying a count of unique releases.

## Pull behavior

Pull targets the selected registered checkout and its current branch's upstream. It never silently switches to the default branch. If the current branch is a feature branch, the confirmation names that branch. The default branch can be pulled by selecting its registered worktree or checking it out outside Phrourion.

Before execution:

1. Resolve and verify the registered checkout root and current remote identity.
2. Check for dirty files, conflicts, detached HEAD, an ongoing merge/rebase, missing upstream, and a changed remote mapping. Refuse with a precise reason if any applies.
3. Fetch the explicit upstream remote and calculate incoming/outgoing commits. Refuse divergence. If there are no incoming commits, report that no pull is needed.
4. Show canonical folder, branch, remote, upstream, and incoming count in a confirmation dialog.
5. Recheck local branch, HEAD, working-tree state, and upstream after confirmation. If any changed, invalidate the preview.
6. Fast-forward to the exact fetched upstream commit using Git in the explicit checkout directory. Disable implicit autostash. Keep Git's own locking and checks in force.
7. Refresh local state and retain command output, exit status, and before/after commit IDs in the action log.

Use a shared lock per Git common directory to serialize Phrourion mutations across linked worktrees. External tools can still modify a repository, so revalidation and Git's own safeguards remain necessary. Execute subprocesses with argument arrays, never shell interpolation. No automatic reset, rebase, stash, or branch switch.

## Provider boundary

The TUI consumes provider-neutral repository, branch, pull-request, check, and release records. Provider modules own URL parsing, API pagination, authentication integration, and translation into those records.

The interface exposes repository discovery, capability reporting, repository snapshots, branch listing, PR listing, check lookup, release listing, and remote links. Future mutation actions use typed requests and declared capabilities. Local pull belongs to the Git service, not a hosting provider.

Capabilities are granular. Unsupported draft releases, checks, or workflow operations remain visible as unsupported. Provider failures retain useful local Git state.

GitHub uses the existing authenticated `gh` executable, always with an explicit host and repository. Phrourion does not persist provider tokens. Command execution, pagination, backoff, and cache behavior are shared infrastructure.

Forgejo now has a working adapter, following this same provider-module shape over HTTP (no suitable CLI exists, so it uses external credential sources instead). Adding GitLab or Bitbucket still requires a provider module, registration in the provider factory, URL fixtures, and shared contract tests. No TUI changes should be required. Support custom hosts and provider overrides for URLs that cannot identify the service. Bitbucket Cloud and Server/Data Center must be identified separately because their APIs differ.

## Components

- Registry: configuration, checkout identity, add/remove, and persistence.
- Git service: local snapshots, fetch, pull preview, and guarded fast-forward.
- Providers: remote snapshots, capabilities, links, and provider-specific parsing.
- Scheduler: bounded jobs, refresh timing, caching, cancellation, and backoff.
- Application state: immutable observations and action progress keyed by repository ID.
- TUI: table, details, forms, confirmations, and command output.

Keep subprocess execution behind a testable interface. Escape untrusted terminal control sequences from repository names, branch names, PR titles, and command output before rendering. Stop child processes on cancellation and restore the terminal on exit or panic.

## Verification and delivery order

1. Registry and CLI: temporary-directory tests for canonical paths, duplicates, linked worktrees, persistence, missing folders, and ambiguous names.
2. Local Git snapshots: temporary real repositories covering clean, dirty, untracked, conflicts, detached HEAD, ahead, behind, divergence, and missing upstream.
3. Provider contract and GitHub adapter: recorded fixtures for multiple pages, missing release labels, drafts, checks tied to commits, permission errors, rate limits, and partial responses. A fake second provider demonstrates that application code has no GitHub dependencies.
4. Scheduler and TUI: deterministic refresh tests and Ratatui buffer snapshots for ordinary/narrow terminals, stale state, errors, and selection stability.
5. Pull actions: bare local remotes and two checkouts verify only the registered folder changes, only fast-forwards occur, changed previews are rejected, feature branches stay selected, and dirty/diverged checkouts are preserved.
6. Integration: register the 13 actual checkouts, compare displayed state with explicit Git/provider queries, and exercise actions against disposable fixtures. Add README usage, CI, and a release policy that does not release docs/test/CI-only changes.

The first delivery is GitHub monitoring and safe local pull. The Forgejo adapter has since shipped through the same tested boundary; GitLab and Bitbucket implementations follow it next. The user registry and credentials are never committed to Phrourion.
