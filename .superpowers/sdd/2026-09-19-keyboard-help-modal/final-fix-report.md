# Final Fix Report

Date: 2026-09-19
Branch: `feat/keyboard-help-modal`
Starting commit: `3ea9ee0`

## Scope

Implemented the two final-review blockers for the keyboard help modal. No
README or `docs/` files were changed.

## Blocker A: preview results while help is open

`Mode::Help` now retains an optional pending preview while preserving its
original suspended mode. Successful pull and checkout preview messages are
stored instead of replacing the help overlay. `close_help` converts the
pending result into the appropriate confirmation mode after the overlay is
closed.

The event loop preview handling is shared by both preview message types. Help
dispatch remains the first key-dispatch layer, so dashboard/input keys remain
consumed while the overlay is open.

Regression coverage includes:

- pull preview deferred over a suspended, partially typed input mode;
- checkout preview deferred until help closes;
- preservation of the suspended input mode while pending;
- a dashboard key remaining a help-local ignored action while suspended.

## Blocker B: narrow help-modal rendering

Narrow help layouts now stack each binding key and description as separate
rows, then render them with wrapping enabled. Scroll limits are calculated
from the rendered narrow line widths, so wrapped content contributes to the
viewport height and the close controls remain in the modal footer.

Wide terminals retain the existing two-column, one-row-per-binding layout.

Regression coverage verifies that a narrow render contains the complete
`select repository` description rather than a truncated prefix. Existing
scroll and resize tests were updated for the new rendered row geometry.

## Verification

- Focused red phase: tests failed to compile before the new production API and
  pending-help state existed.
- Focused green phase: pull-preview, checkout-preview, and narrow-render
  regressions passed.
- `cargo test tui::tests`: 82 passed, 0 failed.
- `cargo fmt --check`: passed.
- `scripts/check-screenshot.sh`: passed; generated comparison output at
  `/tmp/tmp.ztD7wmGbxr`.
- `git diff --check`: passed.

## Concerns

The narrow modal's scroll maximum increases because stacked rows intentionally
use more vertical space. The existing wide two-column behavior is unchanged.
