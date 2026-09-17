mod animation;
mod app;
mod cache;
mod draw;
mod event_loop;

pub use app::{App, RowState};
pub use draw::draw;

use crate::registry;
use anyhow::Result;
use std::path::PathBuf;

pub async fn run(config: PathBuf) -> Result<()> {
    let mut app = App::new(registry::load(&config)?.repos);
    let mut terminal = ratatui::init();
    let result = async {
        animation::startup_screen(&mut terminal).await?;
        event_loop::event_loop(&mut terminal, &mut app, &config).await
    }
    .await;
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{
        animation::{
            EXIT_FRAMES, SCENE_WIDTH, STARTUP_DURATION_MS, STARTUP_FRAMES, draw_animation,
            exit_scene, exit_skips, startup_scene, startup_skips,
        },
        app::AttentionPriority,
        cache::CACHE_PLACEHOLDER,
        draw::{
            CellState, action_message, attention_priority_color, cell_state, count, count_color,
            count_color_combined, health_glyph, meter, spinner_frame, state_color, worse_state,
        },
        event_loop::{entered_notice_tier, notify_script},
    };
    use crate::{
        git::LocalState,
        model::{Observation, RemoteState, Repo},
    };
    use crossterm::event::KeyCode;
    use ratatui::style::Color;
    use std::time::Instant;

    #[test]
    fn notify_script_escapes_quotes_and_backslashes_in_repo_supplied_text() {
        let script = notify_script(r#"weird "repo\name""#, "CI failed");
        assert_eq!(
            script,
            r#"display notification "CI failed" with title "weird \"repo\\name\"""#
        );
    }

    #[test]
    fn notice_fires_only_when_entering_problem_or_pending_from_a_calmer_tier() {
        use AttentionPriority::*;
        assert!(entered_notice_tier(Quiet, Problem));
        assert!(entered_notice_tier(LocalWork, Pending));
        assert!(entered_notice_tier(Quiet, Pending));
        assert!(!entered_notice_tier(Problem, Problem));
        assert!(!entered_notice_tier(Pending, Problem));
        assert!(!entered_notice_tier(Quiet, LocalWork));
        assert!(!entered_notice_tier(Quiet, Quiet));
    }

    fn test_row(local: LocalState) -> RowState {
        RowState {
            repo: Repo {
                id: "demo".into(),
                name: "demo".into(),
                path: ".".into(),
                remote: "origin".into(),
                identity: crate::model::Remote {
                    kind: crate::model::ProviderKind::Local,
                    host: String::new(),
                    project: "demo".into(),
                },
                enabled: true,
                release_workflows: Vec::new(),
                release_labels: Vec::new(),
                issue_labels: Vec::new(),
            },
            local: Observation::success(local),
            remote: RemoteState::default(),
            fetched: None,
            local_busy: true,
            remote_busy: true,
            next_remote: Instant::now(),
            failures: 0,
        }
    }

    #[test]
    fn attention_orders_groups_then_names_without_adding_minor_conditions() {
        let mut app = App::new(Vec::new());
        for (name, local) in [
            ("quiet", LocalState::default()),
            (
                "dirty",
                LocalState {
                    modified: 9,
                    ..LocalState::default()
                },
            ),
            (
                "z-conflict",
                LocalState {
                    conflicts: 1,
                    ..LocalState::default()
                },
            ),
            (
                "b-incoming",
                LocalState {
                    upstream: "origin/main".into(),
                    behind: 2,
                    modified: 4,
                    ..LocalState::default()
                },
            ),
            (
                "a-unpushed",
                LocalState {
                    upstream: "origin/main".into(),
                    ahead: 3,
                    ..LocalState::default()
                },
            ),
            (
                "a-diverged",
                LocalState {
                    upstream: "origin/main".into(),
                    ahead: 1,
                    behind: 1,
                    ..LocalState::default()
                },
            ),
        ] {
            let mut row = test_row(local);
            row.repo.name = name.into();
            row.repo.id = name.into();
            app.rows.push(row);
        }
        let names: Vec<_> = app
            .visible()
            .into_iter()
            .map(|i| app.rows[i].repo.name.as_str())
            .collect();
        assert_eq!(
            names,
            [
                "a-diverged",
                "z-conflict",
                "a-unpushed",
                "b-incoming",
                "dirty",
                "quiet"
            ]
        );
        app.filter = "incoming".into();
        assert_eq!(app.visible(), [3]);
    }

    #[test]
    fn attention_prioritizes_confirmed_ci_and_pending_releases() {
        let mut app = App::new(Vec::new());
        for name in ["quiet", "release", "failed", "stale", "unsupported"] {
            let mut row = test_row(LocalState::default());
            row.repo.name = name.into();
            row.repo.id = name.into();
            app.rows.push(row);
        }
        app.rows[1].remote.drafts = Observation::success(vec![crate::model::Item::default()]);
        let failed = Observation::success(vec![crate::model::Item {
            detail: "completed failure | abc".into(),
            ..Default::default()
        }]);
        app.rows[2].remote.ci = failed.clone();
        app.rows[3].remote.ci = Observation::failure("offline").retain_previous(&failed);
        app.rows[3].local =
            Observation::failure("missing").retain_previous(&Observation::success(LocalState {
                conflicts: 1,
                ..Default::default()
            }));
        app.rows[4].remote.ci = failed;
        app.rows[4].remote.ci.supported = false;
        let names: Vec<_> = app
            .visible()
            .into_iter()
            .map(|i| app.rows[i].repo.name.as_str())
            .collect();
        assert_eq!(
            names,
            ["failed", "release", "quiet", "stale", "unsupported"]
        );
    }

    #[test]
    fn attention_refresh_preserves_selection_and_handles_removal() {
        let mut app = App::new(Vec::new());
        for name in ["alpha", "beta", "gamma"] {
            let mut row = test_row(LocalState::default());
            row.repo.name = name.into();
            row.repo.id = name.into();
            app.rows.push(row);
        }
        app.selected = 1;
        app.update_rows(|rows| rows[2].local.data.as_mut().unwrap().conflicts = 1);
        assert_eq!(app.current(), Some(1));
        assert_eq!(app.selected, 2);
        app.update_rows(|rows| rows.retain(|r| r.repo.id != "beta"));
        assert!(app.current().is_some());
        app.update_rows(Vec::clear);
        assert_eq!(app.current(), None);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn attention_reason_is_visible_on_compact_and_wide_screens() {
        for width in [80, 140] {
            let mut row = test_row(LocalState {
                upstream: "origin/main".into(),
                ahead: 3,
                modified: 1,
                ..Default::default()
            });
            row.remote.ci = Observation::success(vec![crate::model::Item {
                detail: "completed failure | abc".into(),
                ..Default::default()
            }]);
            let mut app = App::new(Vec::new());
            app.rows.push(row);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 30)).unwrap();
            terminal.draw(|f| draw(f, &app)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("Attention"));
            assert!(text.contains("CI failed"));
            assert!(!text.contains("3 unpushed"));
        }
    }

    #[test]
    fn empty_screen_renders_at_small_and_large_sizes() {
        for (width, height) in [(40, 12), (120, 40)] {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &App::new(Vec::new()))).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("P H R O U R I O N"));
            assert!(text.contains("Aranea Development"));
            let top: String = buffer
                .content
                .iter()
                .take(width as usize)
                .map(|c| c.symbol())
                .collect();
            assert!(top.contains("P H R O U R I O N"));
            assert!(!top.contains("Aranea Development"));
        }
    }
    #[test]
    fn error_counts_never_look_like_zero() {
        let o: Observation<Vec<String>> = Observation::failure("denied");
        assert_eq!(count(&o), "!");
        assert_eq!(count(&Observation::<Vec<String>>::success(vec![])), "0");
    }

    #[test]
    fn cell_state_prefers_data_over_a_stale_or_real_error() {
        let never_fetched: Observation<Vec<String>> = Observation::default();
        assert_eq!(cell_state(&never_fetched), CellState::Loading);

        let stale_placeholder: Observation<Vec<String>> = Observation::failure(CACHE_PLACEHOLDER);
        assert_eq!(cell_state(&stale_placeholder), CellState::Loading);

        let real_error: Observation<Vec<String>> = Observation::failure("denied");
        assert_eq!(cell_state(&real_error), CellState::Error);

        let unsupported: Observation<Vec<String>> = Observation::unsupported();
        assert_eq!(cell_state(&unsupported), CellState::Unsupported);

        let with_data: Observation<Vec<String>> = Observation::success(vec!["x".into()]);
        assert_eq!(cell_state(&with_data), CellState::Data);

        // retain_previous backfills `data` from a prior successful fetch
        // without clearing the placeholder error — data must still win.
        let mut stale_but_has_data: Observation<Vec<String>> =
            Observation::failure(CACHE_PLACEHOLDER);
        stale_but_has_data.data = Some(vec!["cached".into()]);
        assert_eq!(cell_state(&stale_but_has_data), CellState::Data);
    }

    #[test]
    fn worse_state_prioritizes_error_over_loading_over_unsupported_over_data() {
        assert_eq!(
            worse_state(CellState::Data, CellState::Error),
            CellState::Error
        );
        assert_eq!(
            worse_state(CellState::Loading, CellState::Unsupported),
            CellState::Loading
        );
        assert_eq!(
            worse_state(CellState::Data, CellState::Data),
            CellState::Data
        );
    }

    #[test]
    fn state_color_only_uses_the_data_color_for_real_data() {
        assert_eq!(state_color(CellState::Data, Color::Magenta), Color::Magenta);
        assert_eq!(
            state_color(CellState::Loading, Color::Magenta),
            Color::DarkGray
        );
        assert_eq!(
            state_color(CellState::Unsupported, Color::Magenta),
            Color::DarkGray
        );
        assert_eq!(state_color(CellState::Error, Color::Magenta), Color::Red);
    }

    #[test]
    fn count_color_mutes_a_real_zero_and_accents_a_nonzero_count() {
        let empty: Observation<Vec<String>> = Observation::success(vec![]);
        assert_eq!(count_color(&empty, Color::Magenta), Color::DarkGray);

        let nonempty: Observation<Vec<String>> = Observation::success(vec!["x".into()]);
        assert_eq!(count_color(&nonempty, Color::Magenta), Color::Magenta);

        let errored: Observation<Vec<String>> = Observation::failure("denied");
        assert_eq!(count_color(&errored, Color::Magenta), Color::Red);

        let loading: Observation<Vec<String>> = Observation::default();
        assert_eq!(count_color(&loading, Color::Magenta), Color::DarkGray);
    }

    #[test]
    fn count_color_combined_accents_if_either_side_is_nonempty() {
        let empty: Observation<Vec<String>> = Observation::success(vec![]);
        let nonempty: Observation<Vec<String>> = Observation::success(vec!["x".into()]);

        assert_eq!(
            count_color_combined(&empty, &empty, Color::Yellow),
            Color::DarkGray
        );
        assert_eq!(
            count_color_combined(&nonempty, &empty, Color::Yellow),
            Color::Yellow
        );
        assert_eq!(
            count_color_combined(&empty, &nonempty, Color::Yellow),
            Color::Yellow
        );

        // An error on either side is more attention-worthy than empty data
        // on the other, so it wins per worse_state's ranking.
        let errored: Observation<Vec<String>> = Observation::failure("denied");
        assert_eq!(
            count_color_combined(&errored, &empty, Color::Yellow),
            Color::Red
        );
    }

    #[test]
    fn attention_priority_color_flags_problem_red_and_pending_work_yellow() {
        assert_eq!(
            attention_priority_color(AttentionPriority::Problem),
            Color::Red
        );
        assert_eq!(
            attention_priority_color(AttentionPriority::Pending),
            Color::Yellow
        );
        assert_eq!(
            attention_priority_color(AttentionPriority::LocalWork),
            Color::Yellow
        );
        assert_eq!(
            attention_priority_color(AttentionPriority::Quiet),
            Color::Reset
        );
    }

    #[test]
    fn spinner_cycles_through_readable_unicode_frames() {
        assert_eq!(spinner_frame(0), "·");
        assert_eq!(spinner_frame(5), "∘");
        assert_eq!(spinner_frame(6), "·");
    }

    #[test]
    fn meter_is_bounded_and_preserves_empty_state() {
        assert_eq!(meter(0, 10, 8), "        ");
        assert_eq!(meter(5, 10, 8), "████    ");
        assert_eq!(meter(20, 10, 8), "████████");
    }

    #[test]
    fn action_message_prioritizes_failures_then_work() {
        assert_eq!(action_message(0, 0, 2, 0, 0), "inspect 2 failing checks");
        assert_eq!(action_message(1, 3, 0, 0, 0), "clean 1 dirty checkout");
        assert_eq!(action_message(0, 3, 0, 0, 0), "pull 3 repositories");
        assert_eq!(
            action_message(0, 0, 0, 1, 2),
            "review 1 release and 2 pull requests"
        );
        assert_eq!(action_message(0, 0, 0, 0, 0), "all clear");
    }

    #[test]
    fn startup_has_a_complete_tower_animation() {
        assert_eq!(startup_scene(STARTUP_FRAMES), startup_scene(5));
        assert!(startup_scene(1).contains("/\\"));
    }

    #[test]
    fn only_space_skips_startup() {
        assert!(startup_skips(KeyCode::Char(' ')));
        assert!(!startup_skips(KeyCode::Enter));
        assert!(!startup_skips(KeyCode::Char('q')));
    }

    #[test]
    fn startup_scene_moves_guard_into_the_tower() {
        assert!(startup_scene(0).contains(" o "));
        assert!(startup_scene(3).contains(" o "));
        assert!(startup_scene(5).contains(" o "));
        assert!(startup_scene(5).contains("WATCH"));
    }

    #[test]
    fn exit_scene_ends_with_guard_leaving_the_tower() {
        assert!(exit_scene(0).contains("*"));
        assert!(exit_scene(EXIT_FRAMES - 1).contains("tower is dark"));
        assert!(!exit_scene(EXIT_FRAMES - 1).contains("o/"));
    }

    #[test]
    fn exit_can_be_skipped_without_waiting() {
        assert!(exit_skips(KeyCode::Char('q')));
        assert!(exit_skips(KeyCode::Char(' ')));
        assert!(!exit_skips(KeyCode::Enter));
    }

    #[test]
    fn status_columns_are_not_truncated_and_headers_align_with_content() {
        let mut row = test_row(LocalState {
            branch: "main".into(),
            upstream: "origin/main".into(),
            ahead: 12,
            behind: 34,
            ..LocalState::default()
        });
        row.remote.prs = Observation::failure("denied");
        row.remote.proposals = Observation::failure("denied");
        row.remote.drafts = Observation::failure("denied");
        let mut app = App::new(Vec::new());
        app.rows.push(row);

        let backend = ratatui::backend::TestBackend::new(140, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &app)).unwrap();
        let buffer = terminal.backend().buffer();
        // One `char` per terminal cell (every symbol used in this UI is a single
        // Unicode scalar), so byte offsets from `str::find` would misalign across
        // lines that mix multi-byte glyphs (e.g. health dots) with plain ASCII.
        let lines: Vec<Vec<char>> = (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().chars().next().unwrap_or(' '))
                    .collect()
            })
            .collect();
        fn find_end(line: &[char], needle: &str) -> Option<usize> {
            let needle: Vec<char> = needle.chars().collect();
            (0..=line.len().checked_sub(needle.len())?)
                .find(|&start| line[start..start + needle.len()] == needle[..])
                .map(|start| start + needle.len())
        }

        let sync_end = lines
            .iter()
            .find_map(|l| find_end(l, "diverged +12 -34"))
            .expect("sync value should not be truncated");
        let reldraft_end = lines
            .iter()
            .find_map(|l| find_end(l, "! / !"))
            .expect("rel/draft value should not be truncated");
        let header_line = lines
            .iter()
            .find(|l| find_end(l, "Rel / Draft").is_some())
            .expect("header row should be present");

        let sync_header_end = find_end(header_line, "Sync*").unwrap();
        assert_eq!(
            sync_end, sync_header_end,
            "Sync* header should sit flush with the right edge of its column"
        );

        let reldraft_header_end = find_end(header_line, "Rel / Draft").unwrap();
        assert_eq!(
            reldraft_end, reldraft_header_end,
            "Rel / Draft header should sit flush with the right edge of its column"
        );
    }

    #[test]
    fn busy_refresh_keeps_known_health_glyph() {
        let mut row = test_row(LocalState {
            staged: 1,
            ..LocalState::default()
        });
        assert_eq!(health_glyph(&row), "◆");

        row.local.data = Some(LocalState::default());
        assert_eq!(health_glyph(&row), "●");
    }

    #[test]
    fn startup_animation_lasts_at_least_twice_the_original_duration() {
        assert!(std::hint::black_box(STARTUP_DURATION_MS) >= 3_900);
    }

    #[test]
    fn animation_tower_is_not_pushed_to_the_right() {
        for scene in (0..6).flat_map(|i| [startup_scene(i), exit_scene(i)]) {
            let tower_line = scene
                .lines()
                .find(|line| line.contains("/\\") || line.contains("/##\\"))
                .unwrap();
            let marker = if tower_line.contains("/\\") {
                "/\\"
            } else {
                "/##\\"
            };
            assert!(
                tower_line.find(marker).unwrap() < 24,
                "tower line {tower_line:?}"
            );
        }
    }

    #[test]
    fn rendered_tower_stays_centered_in_every_frame() {
        for (width, height) in [(40, 18), (80, 24), (121, 41)] {
            for step in 0..6 {
                for scene in [startup_scene(step), exit_scene(step)] {
                    let mut terminal =
                        ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height))
                            .unwrap();
                    terminal
                        .draw(|frame| draw_animation(frame, scene.clone()))
                        .unwrap();
                    let buffer = terminal.backend().buffer();
                    let roof: Vec<_> = (0..height)
                        .flat_map(|y| (0..width).map(move |x| (x, y)))
                        .filter(|&(x, y)| buffer[(x, y)].symbol() == "#")
                        .collect();
                    let left = roof.iter().map(|p| p.0).min().unwrap();
                    let right = roof.iter().map(|p| p.0).max().unwrap();
                    assert!((i32::from(left + right) - i32::from(width - 1)).abs() <= 1);
                    let top = roof.iter().map(|p| p.1).min().unwrap() - 3;
                    for y in 5..12 {
                        assert_eq!(buffer[(left - 2, top + y)].symbol(), "|");
                        assert_eq!(buffer[(right + 2, top + y)].symbol(), "|");
                    }
                }
            }
        }
    }

    #[test]
    fn animation_scene_rows_use_one_fixed_width_canvas() {
        for scene in (0..6).flat_map(|i| [startup_scene(i), exit_scene(i)]) {
            assert!(
                scene
                    .lines()
                    .all(|line| line.chars().count() == SCENE_WIDTH),
                "{scene:?}"
            );
        }
    }

    #[test]
    fn centered_text_starts_at_the_expected_column() {
        // SCENE_WIDTH (40) - "P H R O U R I O N".len() (17) = 23, halved
        // and truncated by integer division: 11. Hardcoded rather than
        // recomputed with the same formula, so a wrong operator in
        // `centered()` actually changes the expected value here too.
        let scene = startup_scene(0);
        let title_line = scene.lines().next().unwrap();
        let leading_spaces = title_line.chars().take_while(|&c| c == ' ').count();
        assert_eq!(leading_spaces, 11, "line: {title_line:?}");
        assert!(title_line[11..].starts_with("P H R O U R I O N"));
    }

    #[test]
    fn exiting_guard_moves_through_each_expected_position() {
        let position = |frame: usize| {
            let scene = exit_scene(frame);
            for (y, line) in scene.lines().enumerate() {
                if let Some(x) = line.find(" o ") {
                    return (x, y);
                }
            }
            panic!("guard not found in exiting scene frame {frame}: {scene}");
        };
        assert_eq!(position(0), (18, 5));
        assert_eq!(position(1), (18, 7));
        assert_eq!(position(2), (24, 9));
        assert_eq!(position(3), (29, 9));
        assert_eq!(position(4), (34, 9));
    }
}
