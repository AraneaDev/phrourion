# Workspaces and Open-in-Terminal Design

## Status

Approved design for implementation on 2026-09-18.

## Goal

Add named workspaces as reusable groups over the existing repository registry,
while preserving the current flat registry and dashboard behavior. Add an
action that opens a new terminal in the selected repository's directory. After
implementation, assign all 13 currently registered repositories to the
`AraneaDev` workspace.

## Scope and decisions

- A repository may belong to multiple workspaces.
- `All` is an implicit view and is not stored as a workspace.
- Repositories remain the source of truth; workspaces are membership labels.
- Workspace selection persists between launches and falls back to `All` when
  the saved workspace no longer exists.
- Workspace management is available from both the CLI and TUI.
- Opening a terminal always starts a new terminal process/window.
- The terminal action changes no repository or registry state.

## Data model and compatibility

Add a defaulted `workspaces: Vec<String>` field to `Repo`, plus a defaulted
registry-level `workspaces: Vec<String>` catalog and optional active-workspace
preference. Old TOML registries without these fields deserialize with no
memberships and no named workspaces. The catalog allows an empty workspace to
exist while memberships remain simple labels on each repository.

Workspace names are trimmed, non-empty, case-preserving display values. Name
comparisons are case-insensitive so duplicate labels cannot be created. The
catalog is sorted deterministically. Adding an existing membership is
idempotent. Removing a missing membership is also idempotent where the
requested workspace and repository are otherwise valid.

The existing locked, atomic registry update path remains the only write path.
Deleting a workspace removes its memberships from repositories but never
deletes repositories or checkout files. The post-implementation migration adds
`AraneaDev` to every currently registered repository.

## Registry API

The registry module owns the workspace catalog, membership mutation, and
filtering so CLI and TUI share validation and behavior. It should provide
operations equivalent to:

- list known workspace names;
- create and delete a workspace;
- add or remove a repository membership;
- retrieve repositories filtered by workspace;
- read and persist the active workspace preference.

Operations must reject empty names, the reserved `All` name, unknown
workspaces where membership is being changed, unknown repositories, and
ambiguous repository selectors before writing. Deleting a workspace removes
the catalog entry and all matching memberships. Workspace and repository lists
remain deterministic and human-readable in TOML.

## CLI

Add a `workspace` command family:

```text
phro workspace list
phro workspace create NAME
phro workspace delete NAME
phro workspace add NAME REPO...
phro workspace remove NAME REPO...
phro workspace repos NAME
```

Repository selectors use existing name-or-ID matching rules. Existing `list`
output gains workspace membership. `status` gains an optional workspace filter;
without it, current behavior is unchanged. Add `phro open-terminal REPO`, using
the same selector rules.

## TUI

The dashboard header displays the active workspace and visible repository
count. A workspace switcher exposes `All` and all named workspaces. Switching
filters visible rows without changing refresh, cache, sorting, or repository
action safety behavior, and persists the selection.

Add keyboard actions for switching workspace, creating a workspace, assigning
the selected repository, removing the selected repository from a workspace,
and opening a new terminal for the selected repository. The exact unused key
bindings should follow the existing help/status conventions; `t` is reserved
for opening the terminal. Empty workspaces show a clear empty state and
available actions.

## Open-in-terminal behavior

Implement a small platform-facing helper that receives a registered checkout
path and returns a normal `Result`. It validates that the path still exists
and is a directory, selects `$TERMINAL` first, then supported common Linux
terminals with their correct working-directory arguments, and launches a
detached new process/window with the checkout as its working directory.

The initial fallback set should cover the terminals available in the target
environment and common alternatives such as `foot`, `kitty`, `alacritty`,
`wezterm`, and `gnome-terminal`. Unsupported or failed launches report an
error in the TUI log/status line or CLI output. The helper must not invoke a
shell command inside the repository.

## Error handling

All registry mutations are validated before any write and use the existing
exclusive lock plus atomic replacement. A failed workspace or terminal action
leaves both registry and checkout state unchanged. Missing checkout paths are
reported explicitly. TUI errors are recorded without terminating the monitor;
CLI errors use the existing `Result` path and non-success exit behavior.

## Verification

Add tests for:

- deserializing old registries without workspace fields;
- workspace name validation and case-insensitive duplicate prevention;
- create/delete and multi-membership operations;
- idempotent assignment and removal;
- repository filtering and CLI selector behavior;
- active-workspace fallback and persistence;
- TUI selection stability when changing workspace views;
- terminal command construction and invalid path handling without launching a
  real terminal.

Run the project's formatting check, Clippy with warnings denied, full test
suite, and existing screenshot/build checks. Verify the final registry contains
all 13 current repositories in `AraneaDev` and no unintended checkout changes.
