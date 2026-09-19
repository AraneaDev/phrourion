# Final narrow-scroll fix report

## Status

Implemented and verified the authorized remaining blocker on the current
branch at `34768fb`.

## Change

`src/tui/draw.rs` now calculates narrow help content height with Ratatui's
`Paragraph::line_count` using the same `Wrap { trim: false }` geometry used by
the rendered paragraph. This replaces the display-width ceiling estimate that
could undercount word-wrapped lines. The Ratatui
`unstable-rendered-line-info` feature is enabled to access that API.

Added a regression test at 20x12 that sets stored help scroll to the maximum
and verifies the final wrapped description remains reachable.

Wide two-column rendering, fixed close controls, and stored scroll
normalization are unchanged.

## TDD evidence

- RED: the new regression failed before the fix because the final lowercase
  `page` line was unreachable.
- GREEN: the same focused test passed after the fix.

## Verification

- `cargo test tui::tests` — 83 passed, 0 failed
- `cargo fmt --check` — passed
- `scripts/check-screenshot.sh` — passed
- `git diff --check` — passed

## Concerns

The implementation relies on Ratatui's currently unstable rendered-line-info
feature, because it is the exact layout calculation used by the Paragraph
renderer. No unrelated observations or documentation were changed.
