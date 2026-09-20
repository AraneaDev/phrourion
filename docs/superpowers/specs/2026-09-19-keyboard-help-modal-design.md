# Keyboard Help Modal and Keyboard-First TUI

Date: 2026-09-19

## Goal

Make the Phrourion TUI usable from the keyboard without relying on a dense,
ever-changing footer. Preserve familiar shortcuts, normalize their behavior,
and provide a discoverable help surface for every dashboard, input,
confirmation, and help action.

## Scope

The change covers the TUI event loop, application mode state, rendering, and
tests. It does not change repository refresh scheduling, provider behavior,
Git actions, registry persistence, or the meaning of existing action keys.

Existing action keys remain available: `a` add, `d` remove, `w` workspace,
`n` create workspace, `m/u` add/remove membership, `c` checkout a PR, `p` pull,
`r/R` refresh selected/all, `o` open the selected remote page, `t` open a
terminal, `/` filter, and `1`–`6` select detail tabs.

## Interaction model

### Help

- `?` or `F1` opens help from the normal dashboard.
- Help is represented as a real modal overlay, not only as a footer variant.
- `Esc`, `?`, `F1`, and `q` close the help modal.
- `j/k`, arrow keys, and `PageUp/PageDown` scroll help when its content is
  taller than the available modal area.
- While help is open, dashboard actions do not receive key events.

### Navigation

- `j/k` and Up/Down move the repository selection.
- `h/l` and Left/Right move between detail tabs.
- `Tab` and `Shift-Tab` move forward/backward between detail tabs.
- `Enter` advances the selected detail tab on the dashboard, submits input in
  text-entry modes, and accepts a confirmation. Direct tab selection remains
  available through `1`–`6`, `h/l`, arrows, and `Tab`/`Shift-Tab`.
- `Esc` consistently backs out of an input, confirmation, help, or detail
  interaction and resets detail scrolling where that is the existing behavior.
- `q` quits from the normal dashboard; `Ctrl-C` remains the emergency quit
  shortcut. In help, `q` closes the modal rather than quitting.

Text-entry modes keep printable characters as input. Their controls remain
`Backspace` to edit, `Enter` to submit, and `Esc` to cancel. `F1` opens help
from these modes without making `?` impossible to type. Confirmation modes
retain `y`/`Enter` to accept, `n`/`Esc` to reject, and `F1` for help.

## Command catalog

The TUI will have one authoritative keyboard-binding catalog. Each entry
contains:

- displayable key or key combination;
- short action description;
- context in which it applies;
- whether it is a navigation, action, input, confirmation, or help command;
- optional availability information for action-busy or no-selection states.

The help modal and compact footer hints are rendered from this catalog. The
catalog is documentation data, not a second action dispatcher: key handling
continues to call the existing application and service operations.

Help is an overlay over the current mode. Opening it stores the mode being
suspended, including any partially typed input or pending preview; closing it
restores that exact mode. Opening help while help is already visible does not
nest overlays.

## Modal layout

Help renders as a centered modal over the existing dashboard:

- dim or clear the modal rectangle so the dashboard cannot visually compete;
- size the panel to its content with safe margins, clamped to terminal width
  and height;
- use two grouped columns when the terminal is wide enough;
- fall back to one scrollable column on narrow terminals;
- keep the title and close hint visible while the binding list scrolls;
- show grouped sections for Navigation, Details, Repository actions,
  Workspaces, Input fields, Confirmations, and Help.

The footer becomes a compact context hint and status area, for example:
`? help · Enter details · / filter · r refresh · q quit`. It never duplicates
the complete catalog.

## State and error handling

Help state is local UI state. Opening, closing, scrolling, or resizing it must
not trigger a refresh, mutation, registry write, or background task. Key
dispatch checks help before normal actions so commands cannot leak through the
overlay.

Actions that are unavailable while a task is busy or no repository is
selected remain visible in help but are marked unavailable. Modal sizing is
clamped for very small terminals; content remains scrollable and the close
hint stays reachable.

## Testing

Add unit tests for:

1. every documented binding mapping to its intended key/action and context;
2. opening, scrolling, and closing help through `?`, `F1`, `Esc`, and `q`;
3. dashboard keys not triggering actions while help is open, including when a
   text-entry or confirmation mode is suspended;
4. input and confirmation modes retaining their existing semantics;
5. tab navigation through arrows, `h/l`, `Tab`, and `Shift-Tab`;
6. action availability while busy and with no selected repository;
7. modal layout at narrow and wide terminal sizes;
8. rendered help content containing all command groups and bindings.

Run the full Rust test suite and the existing screenshot/layout checks after the
implementation.

## Non-goals

- introducing a searchable command palette;
- changing the meaning of repository actions;
- adding mouse-only interaction;
- moving action scheduling or provider logic into the UI layer;
- adding a second, manually maintained shortcut list.
