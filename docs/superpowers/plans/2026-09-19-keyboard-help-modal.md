# Keyboard Help Modal and Keyboard-First TUI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a centered, scrollable keyboard-help modal and normalize keyboard navigation across the Phrourion TUI without changing repository actions or background scheduling.

**Architecture:** Keep event execution in `event_loop.rs`, application modes in `app.rs`, rendering in `draw.rs`, and put the authoritative binding descriptions in a new `keys.rs` module. Help overlays the current mode through an explicit suspended-mode representation, while the modal and compact footer both render from the same catalog.

**Tech Stack:** Rust 1.85, crossterm key events, Ratatui layout/widgets, existing Rust unit tests and buffer/layout test helpers.

**Spec:** `docs/superpowers/specs/2026-09-19-keyboard-help-modal-design.md`

## Global Constraints

- Preserve existing action meanings: `a`, `d`, `w`, `n`, `m/u`, `c`, `p`, `r/R`, `o`, `t`, `/`, and `1`–`6`.
- Help must not trigger refreshes, mutations, registry writes, or background tasks.
- Text-entry modes keep printable characters, `Backspace`, `Enter`, and `Esc` semantics.
- Confirmation modes accept with `y`/`Enter` and reject with `n`/`Esc`.
- The modal must clamp safely to narrow terminals and keep its close hint reachable.
- Do not add a second manually maintained shortcut list.

### Task 1: Introduce the authoritative keyboard catalog and suspended help state

**Files:**
- Create: `src/tui/keys.rs`
- Modify: `src/tui/mod.rs` to register the module
- Modify: `src/tui/app.rs` `Mode` definition and mode helpers
- Test: `src/tui/keys.rs` unit tests and existing `src/tui/mod.rs` tests

**Interfaces:**
- `keys.rs` produces `BindingGroup`/`Binding` values consumed by rendering and tests.
- `app.rs` exposes a help mode that can restore the exact previous `Mode`, including input text and previews.
- Later tasks consume `keys::catalog()` and the help scroll state; no key dispatcher belongs in `keys.rs`.

- [ ] **Step 1: Write failing catalog tests**

Add tests that assert the catalog contains the required groups and exact bindings:

```rust
#[test]
fn catalog_lists_all_dashboard_actions_and_modal_controls() {
    let bindings = catalog().iter().flat_map(|group| group.bindings.iter()).collect::<Vec<_>>();
    assert!(bindings.iter().any(|b| b.keys == "? / F1" && b.description == "open help"));
    assert!(bindings.iter().any(|b| b.keys == "r / R" && b.description == "refresh selected / all"));
    assert!(bindings.iter().any(|b| b.keys == "y / Enter" && b.description == "confirm"));
    assert!(catalog().iter().any(|group| group.title == "Workspaces"));
}
```

- [ ] **Step 2: Run the focused tests and verify the expected failure**

Run: `cargo test tui::keys::tests::catalog_lists_all_dashboard_actions_and_modal_controls`

Expected: FAIL because `keys.rs`, `catalog()`, and the binding types do not yet exist.

- [ ] **Step 3: Implement the catalog and mode representation**

Create small owned display types:

```rust
pub(crate) struct Binding { pub keys: &'static str, pub description: &'static str }
pub(crate) struct BindingGroup { pub title: &'static str, pub bindings: &'static [Binding] }
pub(crate) fn catalog() -> &'static [BindingGroup]
```

Use a suspended mode variant that preserves state without cloning action data unnecessarily:

```rust
pub(super) enum Mode {
    // existing variants...
    Help { previous: Box<Mode>, scroll: u16 },
}
```

Add helpers on `App` or `Mode` to open help once and restore the previous mode. Ensure opening help while already in help does not nest it.

- [ ] **Step 4: Add mode restoration tests**

Test that a filter string, add input, and confirmation preview survive `Help` open/close, and that opening help twice leaves only one suspended layer.

- [ ] **Step 5: Run the focused tests and commit**

Run: `cargo test tui::keys::tests tui::tests::help_mode`

Expected: PASS.

Commit: `git add src/tui/keys.rs src/tui/app.rs src/tui/mod.rs && git commit -m "feat(tui): add keyboard binding catalog"`

### Task 2: Normalize keyboard dispatch and modal behavior

**Files:**
- Modify: `src/tui/event_loop.rs` key dispatch and helper functions
- Modify: `src/tui/app.rs` tab/help helpers if needed
- Test: existing TUI unit tests in `src/tui/mod.rs` plus focused event helpers

**Interfaces:**
- Consumes `keys::catalog()` only for context decisions/tests; action execution continues through existing `git`, `registry`, `provider`, and terminal calls.
- Produces deterministic key semantics for normal, help, input, and confirmation modes.

- [ ] **Step 1: Add failing key-semantics tests**

Cover the pure decisions before editing the event loop:

```rust
#[test]
fn tab_navigation_wraps_in_both_directions() {
    assert_eq!(next_tab(0, TabDirection::Previous), 5);
    assert_eq!(next_tab(5, TabDirection::Next), 0);
}

#[test]
fn help_keys_are_contextual_and_do_not_consume_text_question_marks() {
    assert!(opens_help(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE), false));
    assert!(!opens_help(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE), true));
}
```

Add tests for `h/l`, Left/Right, Tab/Shift-Tab, Enter confirmation, help close keys, and dashboard action suppression while help is open.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test tui::tests::tab_navigation_wraps_in_both_directions tui::tests::help_keys_are_contextual_and_do_not_consume_text_question_marks`

Expected: FAIL because the new helpers and normalized semantics are not implemented.

- [ ] **Step 3: Implement dispatch changes**

Handle `F1` before mode-specific dispatch by wrapping the current mode in `Mode::Help`, except when already in help. In `Mode::Help`, handle `Esc`, `?`, `F1`, `q`, `j/k`, arrows, and page keys locally; do not fall through to dashboard actions.

Normalize normal-mode dispatch as follows:

```rust
KeyCode::Left | KeyCode::Char('h') => previous_tab(app),
KeyCode::Right | KeyCode::Char('l') => next_tab(app),
KeyCode::Tab => next_tab(app),
KeyCode::BackTab => previous_tab(app),
KeyCode::Enter => next_tab(app),
```

Make confirmation `Enter` follow the same acceptance path as `y`. Preserve `q` as quit only in normal mode and `Ctrl-C` as the emergency quit path.

- [ ] **Step 4: Run focused and full library tests**

Run: `cargo test tui::tests`

Expected: PASS with the existing action tests unchanged.

- [ ] **Step 5: Commit**

Commit: `git add src/tui/app.rs src/tui/event_loop.rs src/tui/mod.rs && git commit -m "feat(tui): normalize keyboard navigation"`

### Task 3: Render the centered help modal and compact footer

**Files:**
- Modify: `src/tui/draw.rs` footer rendering and new modal renderer
- Modify: `src/tui/keys.rs` context-specific compact hints if required
- Test: Ratatui buffer/layout tests in `src/tui/mod.rs` or `src/tui/draw.rs`

**Interfaces:**
- Consumes `App::mode`, help scroll state, and `keys::catalog()`.
- Produces a centered modal with grouped binding rows and a footer derived from the same catalog.

- [ ] **Step 1: Add failing render tests**

Render representative terminal sizes and assert the buffer contains the modal title, all group titles, close hint, and a compact footer when help is closed:

```rust
#[test]
fn help_modal_is_centered_and_contains_grouped_bindings() {
    let mut app = test_app_in_help_mode();
    let buffer = render_app(&mut app, 100, 30);
    assert!(buffer_contains(&buffer, "Keyboard help"));
    assert!(buffer_contains(&buffer, "Repository actions"));
    assert!(buffer_contains(&buffer, "Esc / ? / F1 close"));
}
```

Add a narrow-size test proving the panel clamps and a scroll test proving the title/close hint remain visible while rows change.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test tui::tests::help_modal_is_centered_and_contains_grouped_bindings`

Expected: FAIL because the current help mode only changes a footer string.

- [ ] **Step 3: Implement modal layout**

Add a `help_rect(area, content_height)` helper that applies safe margins, clamps width/height, and centers the panel. Use Ratatui `Clear` for the modal rectangle, a `Block` for the border/title, and a clipped `Paragraph`/`Text` layout for grouped rows. Use two columns above a documented minimum width and one column otherwise.

Keep the title and close hint outside the scrolling row region. Clamp `scroll` after terminal resize and render an explicit `↑/↓ scroll` hint only when content overflows.

Replace the long normal footer help string with a compact catalog-derived hint and retain the status/log line.

- [ ] **Step 4: Run layout/screenshot tests and adjust**

Run: `cargo test tui::tests` and `scripts/check-screenshot.sh`.

Expected: PASS; update the checked-in screenshot only if the repository’s deterministic screenshot check requires it.

- [ ] **Step 5: Commit**

Commit: `git add src/tui/draw.rs src/tui/keys.rs src/tui/mod.rs docs/screenshots/dashboard.svg && git commit -m "feat(tui): render centered keyboard help"`

### Task 4: Complete documentation and verification

**Files:**
- Modify: `README.md` keyboard reference to point users to `?`/`F1` and describe normalized navigation
- Modify: `docs/design.md` TUI interaction description
- Test: full repository test and screenshot checks

- [ ] **Step 1: Update user-facing keyboard documentation**

Remove duplicated exhaustive lists where the modal is the source of truth, retain a short startup hint, and document `h/l`, arrows, `Tab`/`Shift-Tab`, `F1`, and modal confirmation behavior.

- [ ] **Step 2: Run all verification**

Run:

```bash
cargo fmt --check
cargo test --all-targets -- --test-threads=1
scripts/check-screenshot.sh
git diff --check
```

Expected: all commands exit successfully.

- [ ] **Step 3: Review the final diff and commit**

Confirm the diff contains only the help-modal/keybinding implementation, tests, documentation, and deterministic screenshot updates. Commit: `git add README.md docs/design.md src/tui docs/screenshots/dashboard.svg && git commit -m "docs: document keyboard-first tui controls"`

## Plan self-review

- Spec coverage: help overlay state and restoration are covered by Tasks 1–2; catalog single source of truth by Tasks 1 and 3; normalized navigation by Task 2; centered responsive modal and compact footer by Task 3; testing and documentation by Task 4.
- Placeholder scan: no TODO/TBD or unspecified implementation steps remain.
- Type consistency: `keys::catalog()` returns static groups consumed by rendering; `Mode::Help { previous, scroll }` is restored by event dispatch; modal layout consumes the same catalog without owning action execution.
