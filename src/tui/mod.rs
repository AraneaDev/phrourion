mod animation;
mod app;
mod cache;
mod draw;
mod event_loop;
mod keys;

pub use app::{App, RowState};
pub use draw::draw;

use crate::registry;
use anyhow::Result;
use std::path::PathBuf;

pub async fn run(config: PathBuf) -> Result<()> {
    let mut app = App::with_registry(registry::load(&config)?);
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
            EXIT_FRAMES, SCENE_HEIGHT, SCENE_WIDTH, STARTUP_DURATION_MS, STARTUP_FRAMES,
            draw_animation, exit_scene, exit_skips, startup_scene, startup_skips,
        },
        app::{AttentionPriority, known_count},
        cache::CACHE_PLACEHOLDER,
        draw::{
            CellState, action_message, animation_frame, attention_priority_color, cell_state,
            ci_color, ci_label, count, count_color, count_color_combined, health_glyph, items,
            meter, spinner_frame, state_color, status_color, triage, worse_state,
        },
        event_loop::{
            Dispatch, HelpAction, KeyContext, Message, TabDirection, accepts_confirmation,
            apply_preview_message, dispatch_for, entered_notice_tier, help_action, is_press,
            is_quit_hotkey, next_failure_count, next_tab, notify_script, opens_help,
            remote_backoff, remote_failed, tab_direction,
        },
    };
    use crate::{
        git::{CheckoutPreview, LocalState, PullPreview},
        model::{Observation, RemoteState, Repo},
        registry::Registry,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
    use ratatui::{buffer::Buffer, style::Color};
    use std::time::{Duration, Instant};

    fn pull_preview(target: &str) -> PullPreview {
        PullPreview {
            repo: test_row(LocalState::default()).repo,
            before: LocalState::default(),
            target: target.into(),
            tracking_remote: "origin".into(),
            tracking_ref: "refs/remotes/origin/main".into(),
        }
    }

    fn checkout_preview(number: u64) -> CheckoutPreview {
        CheckoutPreview {
            repo: test_row(LocalState::default()).repo,
            before: LocalState::default(),
            number,
            branch: format!("pr/{number}"),
            target: "def456".into(),
        }
    }

    #[test]
    fn tab_navigation_wraps_in_both_directions() {
        assert_eq!(next_tab(0, TabDirection::Previous), 5);
        assert_eq!(next_tab(5, TabDirection::Next), 0);
    }

    #[test]
    fn help_keys_are_contextual_and_do_not_consume_text_question_marks() {
        assert!(opens_help(
            KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE),
            false
        ));
        assert!(!opens_help(
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
            true
        ));
    }

    #[test]
    fn tab_navigation_accepts_letters_arrows_and_tab_keys() {
        for code in [KeyCode::Char('h'), KeyCode::Left, KeyCode::BackTab] {
            assert_eq!(
                tab_direction(KeyEvent::new(code, KeyModifiers::NONE)),
                Some(TabDirection::Previous)
            );
        }
        for code in [
            KeyCode::Char('l'),
            KeyCode::Right,
            KeyCode::Tab,
            KeyCode::Enter,
        ] {
            assert_eq!(
                tab_direction(KeyEvent::new(code, KeyModifiers::NONE)),
                Some(TabDirection::Next)
            );
        }
    }

    #[test]
    fn enter_follows_the_confirmation_acceptance_path() {
        assert!(accepts_confirmation(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::NONE
        )));
        assert!(accepts_confirmation(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        )));
        assert!(!accepts_confirmation(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn help_close_and_scroll_keys_are_local_actions() {
        for code in [
            KeyCode::Esc,
            KeyCode::Char('?'),
            KeyCode::F(1),
            KeyCode::Char('q'),
        ] {
            assert_eq!(
                help_action(KeyEvent::new(code, KeyModifiers::NONE)),
                HelpAction::Close
            );
        }
        for (code, expected) in [
            (KeyCode::Char('j'), HelpAction::ScrollDown),
            (KeyCode::Down, HelpAction::ScrollDown),
            (KeyCode::Char('k'), HelpAction::ScrollUp),
            (KeyCode::Up, HelpAction::ScrollUp),
            (KeyCode::PageDown, HelpAction::PageDown),
            (KeyCode::PageUp, HelpAction::PageUp),
        ] {
            assert_eq!(
                help_action(KeyEvent::new(code, KeyModifiers::NONE)),
                expected
            );
        }
    }

    #[test]
    fn dashboard_actions_are_suppressed_while_help_is_open() {
        let action_key = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);

        assert_eq!(
            dispatch_for(action_key, KeyContext::Help),
            Dispatch::Help(HelpAction::Ignore)
        );
        assert_eq!(
            dispatch_for(action_key, KeyContext::Dashboard),
            Dispatch::ModeSpecific
        );
    }

    #[test]
    fn help_mode_restores_filter_text() {
        let mut app = App::new(Vec::new());
        app.filter = "needle".into();
        app.mode = app::Mode::Filter;

        app.open_help();
        app.close_help();

        assert_eq!(app.filter, "needle");
        assert!(matches!(app.mode, app::Mode::Filter));
    }

    #[test]
    fn help_mode_restores_add_input() {
        let mut app = App::new(Vec::new());
        app.mode = app::Mode::Add("partially typed".into());

        app.open_help();
        app.close_help();

        assert!(matches!(app.mode, app::Mode::Add(ref input) if input == "partially typed"));
    }

    #[test]
    fn help_mode_restores_confirmation_preview() {
        let mut app = App::new(Vec::new());
        app.mode = app::Mode::Confirm(Box::new(pull_preview("abc123")));

        app.open_help();
        app.close_help();

        assert!(matches!(
            app.mode,
            app::Mode::Confirm(ref preview) if preview.target == "abc123"
        ));
    }

    #[test]
    fn help_mode_opening_twice_keeps_one_suspended_layer() {
        let mut app = App::new(Vec::new());
        app.mode = app::Mode::Add("draft".into());

        app.open_help();
        app.open_help();

        assert!(matches!(
            app.mode,
            app::Mode::Help {
                ref previous,
                scroll: 0,
                ..
            } if matches!(previous.as_ref(), app::Mode::Add(input) if input == "draft")
        ));
        app.close_help();
        assert!(matches!(app.mode, app::Mode::Add(ref input) if input == "draft"));
    }

    #[test]
    fn pull_preview_waits_for_help_to_close_and_preserves_suspended_input() {
        let mut app = App::new(Vec::new());
        app.mode = app::Mode::Add("partially typed".into());
        app.open_help();

        apply_preview_message(
            &mut app,
            Message::Preview(Box::new(Ok(pull_preview("abc123")))),
        );

        assert!(matches!(
            app.mode,
            app::Mode::Help {
                ref previous,
                pending: Some(_),
                ..
            } if matches!(previous.as_ref(), app::Mode::Add(input) if input == "partially typed")
        ));
        assert_eq!(
            dispatch_for(
                KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
                KeyContext::Help
            ),
            Dispatch::Help(HelpAction::Ignore)
        );

        app.close_help();
        assert!(matches!(
            app.mode,
            app::Mode::Confirm(ref preview) if preview.target == "abc123"
        ));
    }

    #[test]
    fn checkout_preview_waits_for_help_to_close() {
        let mut app = App::new(Vec::new());
        app.open_help();

        apply_preview_message(
            &mut app,
            Message::CheckoutPreview(Box::new(Ok(checkout_preview(42)))),
        );

        assert!(matches!(
            app.mode,
            app::Mode::Help {
                pending: Some(_),
                ..
            }
        ));
        app.close_help();
        assert!(matches!(
            app.mode,
            app::Mode::ConfirmCheckout(ref preview) if preview.number == 42
        ));
    }

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
                workspaces: Vec::new(),
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
    fn workspace_view_filters_rows_and_all_restores_them() {
        let mut alpha = test_row(LocalState::default()).repo;
        alpha.id = "alpha".into();
        alpha.name = "alpha".into();
        alpha.workspaces = vec!["Primary".into()];
        let mut beta = test_row(LocalState::default()).repo;
        beta.id = "beta".into();
        beta.name = "beta".into();
        beta.workspaces = vec!["Other".into()];
        let mut app = App::with_registry(Registry {
            repos: vec![alpha, beta],
            workspaces: vec!["Primary".into(), "Other".into()],
            active_workspace: Some("Primary".into()),
        });

        assert_eq!(app.visible().len(), 1);
        app.active_workspace = None;
        assert_eq!(app.visible().len(), 2);
    }

    #[test]
    fn invalid_saved_workspace_falls_back_to_all_repositories() {
        let mut alpha = test_row(LocalState::default()).repo;
        alpha.id = "alpha".into();
        alpha.name = "alpha".into();
        alpha.workspaces = vec!["Primary".into()];
        let mut beta = test_row(LocalState::default()).repo;
        beta.id = "beta".into();
        beta.name = "beta".into();
        beta.workspaces = vec!["Other".into()];

        let app = App::with_registry(Registry {
            repos: vec![alpha, beta],
            workspaces: vec!["Primary".into(), "Other".into()],
            active_workspace: Some("Missing".into()),
        });

        assert_eq!(app.active_workspace, None);
        assert_eq!(app.visible().len(), 2);
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
            terminal.draw(|f| draw(f, &mut app)).unwrap();
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
            let mut app = App::new(Vec::new());
            terminal.draw(|f| draw(f, &mut app)).unwrap();
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
        // Isolated (only one of releases/prs nonzero) so the combined
        // releases>0 && prs>0 branch is provably not what fired here.
        assert_eq!(action_message(0, 0, 0, 1, 0), "review 1 release");
        assert_eq!(action_message(0, 0, 0, 0, 1), "review 1 pull request");
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
        terminal.draw(|f| draw(f, &mut app)).unwrap();
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
    fn health_glyph_shows_error_glyph_when_any_single_source_errors() {
        let mut local_errors = test_row(LocalState::default());
        local_errors.local = Observation::failure("boom");
        assert_eq!(health_glyph(&local_errors), "!");

        let mut branch_errors = test_row(LocalState::default());
        branch_errors.remote.default_branch = Observation::failure("boom");
        assert_eq!(health_glyph(&branch_errors), "!");

        let mut ci_errors = test_row(LocalState::default());
        ci_errors.remote.ci = Observation::failure("boom");
        assert_eq!(health_glyph(&ci_errors), "!");
    }

    #[test]
    fn health_glyph_shows_incoming_arrow_only_when_behind_is_positive() {
        let clean_row = test_row(LocalState::default());
        assert_eq!(health_glyph(&clean_row), "●");

        let behind_row = test_row(LocalState {
            behind: 1,
            ..LocalState::default()
        });
        assert_eq!(health_glyph(&behind_row), "↓");
    }

    #[test]
    fn triage_computes_exact_counts_and_the_action_message() {
        let mut app = App::new(Vec::new());

        // Dirty and behind.
        let mut dirty_row = test_row(LocalState {
            modified: 1,
            upstream: "origin/main".into(),
            behind: 2,
            ..LocalState::default()
        });
        dirty_row.repo.id = "r1".into();
        app.rows.push(dirty_row);

        // Confirmed CI failure.
        let mut failed_row = test_row(LocalState::default());
        failed_row.repo.id = "r2".into();
        failed_row.remote.ci = Observation::success(vec![crate::model::Item {
            detail: "completed failure | abc".into(),
            ..Default::default()
        }]);
        app.rows.push(failed_row);

        // One open PR, one pending release proposal, two draft releases
        // (distinct counts, so releases = proposals + drafts is provably
        // an addition and not, say, a subtraction).
        let mut release_row = test_row(LocalState::default());
        release_row.repo.id = "r3".into();
        release_row.remote.prs = Observation::success(vec![crate::model::Item::default()]);
        release_row.remote.proposals = Observation::success(vec![crate::model::Item::default()]);
        release_row.remote.drafts = Observation::success(vec![
            crate::model::Item::default(),
            crate::model::Item::default(),
        ]);
        app.rows.push(release_row);

        let (dirty, behind, failures, prs, releases, actionable, action) = triage(&app);
        assert_eq!(dirty, 1);
        assert_eq!(behind, 1);
        assert_eq!(failures, 1);
        assert_eq!(prs, 1);
        assert_eq!(releases, 3);
        assert_eq!(actionable, 7);
        assert_eq!(action, "inspect 1 failing checks");
    }

    #[test]
    fn health_glyph_shows_idle_dot_only_when_no_fetch_is_in_flight() {
        // Not `assert_ne!(health_glyph(&busy_row), "·")`: spinner_frame's
        // own first frame IS "·", so that assertion would flake roughly
        // one time in six depending on the real clock at test time. The
        // idle case alone is deterministic (it never calls animation_frame)
        // and still catches the && -> || mutation on this condition.
        let mut idle_row = test_row(LocalState::default());
        idle_row.local.data = None;
        idle_row.local_busy = false;
        idle_row.remote_busy = false;
        assert_eq!(health_glyph(&idle_row), "·");
    }

    #[test]
    fn animation_frame_reflects_real_elapsed_time() {
        // Unix-ms/140 for any real date since ~2001 is far larger than a
        // stubbed 0 or 1.
        assert!(animation_frame() > 1_000);
    }

    #[test]
    fn status_color_matches_each_health_glyph() {
        let mut error_row = test_row(LocalState::default());
        error_row.local = Observation::failure("boom");
        assert_eq!(status_color(&error_row), Color::Red);

        let dirty_row = test_row(LocalState {
            staged: 1,
            ..LocalState::default()
        });
        assert_eq!(status_color(&dirty_row), Color::Yellow);

        let behind_row = test_row(LocalState {
            behind: 1,
            ..LocalState::default()
        });
        assert_eq!(status_color(&behind_row), Color::Yellow);

        let mut loading_row = test_row(LocalState::default());
        loading_row.local.data = None;
        loading_row.local_busy = false;
        loading_row.remote_busy = false;
        assert_eq!(status_color(&loading_row), Color::DarkGray);

        let clean_row = test_row(LocalState::default());
        assert_eq!(status_color(&clean_row), Color::Green);
    }

    #[test]
    fn ci_label_maps_ci_observation_states_to_labels() {
        let item = |detail: &str| crate::model::Item {
            detail: detail.into(),
            ..Default::default()
        };

        let unsupported = RemoteState {
            ci: Observation::unsupported(),
            ..RemoteState::default()
        };
        assert_eq!(ci_label(&unsupported), "n/a");

        assert_eq!(ci_label(&RemoteState::default()), "…");

        let errored = RemoteState {
            ci: Observation::failure("boom"),
            ..RemoteState::default()
        };
        assert_eq!(ci_label(&errored), "!");

        let empty = RemoteState {
            ci: Observation::success(vec![]),
            ..RemoteState::default()
        };
        assert_eq!(ci_label(&empty), "absent");

        let failing = RemoteState {
            ci: Observation::success(vec![item("completed failure")]),
            ..RemoteState::default()
        };
        assert_eq!(ci_label(&failing), "fail");

        let running = RemoteState {
            ci: Observation::success(vec![item("in_progress")]),
            ..RemoteState::default()
        };
        assert_eq!(ci_label(&running), "running");

        let passing = RemoteState {
            ci: Observation::success(vec![item("completed success")]),
            ..RemoteState::default()
        };
        assert_eq!(ci_label(&passing), "pass");

        let other = RemoteState {
            ci: Observation::success(vec![item("some unrecognized status")]),
            ..RemoteState::default()
        };
        assert_eq!(ci_label(&other), "other");
    }

    #[test]
    fn ci_color_maps_each_label_to_its_own_color() {
        assert_eq!(ci_color("fail"), Color::Red);
        assert_eq!(ci_color("running"), Color::Yellow);
        assert_eq!(ci_color("pass"), Color::Green);
        assert_eq!(ci_color("!"), Color::Red);
        assert_eq!(ci_color("n/a"), Color::DarkGray);
        assert_eq!(ci_color("other"), Color::DarkGray);
    }

    #[test]
    fn items_formats_label_error_and_each_item() {
        let with_error = Observation::<Vec<crate::model::Item>>::failure("boom");
        let s = items("Widgets", &with_error);
        assert!(s.contains("Widgets"));
        assert!(s.contains("boom"));

        let empty = Observation::success(Vec::<crate::model::Item>::new());
        assert!(items("Widgets", &empty).contains("None observed"));

        let with_item = Observation::success(vec![crate::model::Item {
            title: "T1".into(),
            detail: "D1".into(),
            url: "U1".into(),
        }]);
        let rendered = items("Widgets", &with_item);
        assert!(rendered.contains("T1"));
        assert!(rendered.contains("D1"));
        assert!(rendered.contains("U1"));
    }

    #[test]
    fn sync_placeholder_shows_error_glyph_only_when_local_state_itself_errored() {
        // health_glyph() also renders "!" for an errored local (in the
        // Repository column, via its own, separately-tested condition), so
        // a plain substring search can't isolate the Sync column's own
        // placeholder — it must be read at its exact column position.
        let mut error_row = test_row(LocalState::default());
        error_row.local = Observation::failure("boom");
        error_row.repo.name = "err-repo".into();
        error_row.repo.id = "err-repo".into();

        let mut loading_row = test_row(LocalState::default());
        loading_row.local = Observation::default();
        loading_row.repo.name = "load-repo".into();
        loading_row.repo.id = "load-repo".into();

        let mut app = App::new(Vec::new());
        app.rows.push(error_row);
        app.rows.push(loading_row);

        let backend = ratatui::backend::TestBackend::new(140, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
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

        let header_line = lines
            .iter()
            .find(|l| find_end(l, "Sync*").is_some())
            .expect("header row should be present");
        let sync_col = find_end(header_line, "Sync*").unwrap() - 1;

        let err_line = lines
            .iter()
            .find(|l| find_end(l, "err-repo").is_some())
            .unwrap();
        let load_line = lines
            .iter()
            .find(|l| find_end(l, "load-repo").is_some())
            .unwrap();

        assert_eq!(
            err_line[sync_col],
            '!',
            "{:?}",
            err_line.iter().collect::<String>()
        );
        assert_eq!(
            load_line[sync_col],
            '…',
            "{:?}",
            load_line.iter().collect::<String>()
        );
    }

    #[test]
    fn calm_count_only_counts_rows_with_the_calm_glyph() {
        let mut app = App::new(Vec::new());
        app.rows.push(test_row(LocalState::default())); // calm: ●
        app.rows.push(test_row(LocalState::default())); // calm: ●
        app.rows.push(test_row(LocalState {
            staged: 1,
            ..LocalState::default()
        })); // dirty: ◆, not calm

        let text = render_text(&mut app);
        // meter(2, 3, 8) = 6 filled blocks; meter(1, 3, 8) (the wrong count
        // a `==` -> `!=` mutation would produce) is 3 -- distinguishable by
        // counting. Nothing else in the UI renders '█'.
        assert_eq!(text.matches('█').count(), 6, "{text}");
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

    #[test]
    fn startup_guard_moves_through_each_expected_position() {
        let position = |frame: usize| {
            let scene = startup_scene(frame);
            for (y, line) in scene.lines().enumerate() {
                if let Some(x) = line.find(" o ") {
                    return (x, y);
                }
            }
            panic!("guard not found in startup scene frame {frame}: {scene}");
        };
        assert_eq!(position(0), (2, 9));
        assert_eq!(position(1), (8, 9));
        assert_eq!(position(2), (13, 9));
        assert_eq!(position(3), (18, 9));
        assert_eq!(position(4), (18, 7));
    }

    #[test]
    fn known_count_is_zero_unless_supported_and_error_free() {
        let ok: Observation<Vec<String>> = Observation::success(vec!["a".into(), "b".into()]);
        assert_eq!(known_count(&ok), 2);

        // Stale data alongside a fresh error must not be counted as known.
        let mut errored: Observation<Vec<String>> = Observation::failure("boom");
        errored.data = Some(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(known_count(&errored), 0);

        // A disabled feature's leftover data (e.g. from cache) must not
        // be counted either.
        let mut unsupported: Observation<Vec<String>> = Observation::unsupported();
        unsupported.data = Some(vec!["a".into()]);
        assert_eq!(known_count(&unsupported), 0);
    }

    #[test]
    fn attention_reports_unpushed_before_incoming_exactly_at_the_boundary() {
        let local = |ahead, behind| LocalState {
            upstream: "origin/main".into(),
            ahead,
            behind,
            ..LocalState::default()
        };
        let row = |ahead, behind| test_row(local(ahead, behind));

        // Neither branch should fire when both counts are exactly zero.
        let (_, reason) = row(0, 0).attention();
        assert_ne!(reason, "0 unpushed");
        assert_ne!(reason, "0 incoming");

        assert_eq!(row(1, 0).attention().1, "1 unpushed");
        assert_eq!(row(0, 1).attention().1, "1 incoming");
    }

    #[test]
    fn record_trims_the_log_to_at_most_100_entries() {
        let mut app = App::new(Vec::new());
        for i in 0..100 {
            app.record(format!("entry {i}"));
        }
        assert_eq!(app.log.len(), 100);
        assert_eq!(app.log[0], "entry 0");

        app.record("entry 100");
        assert_eq!(app.log.len(), 100, "log must stay capped at 100 entries");
        assert_eq!(app.log[0], "entry 1", "the oldest entry must be dropped");
    }

    #[test]
    fn leg_glyph_is_drawn_two_rows_below_the_guard_and_alternates_by_frame_parity() {
        let leg_at = |scene: &str, x: usize, y: usize| -> String {
            scene
                .lines()
                .nth(y)
                .unwrap()
                .chars()
                .skip(x)
                .take(3)
                .collect()
        };
        // startup frame 0: guard at (2, 9) (see startup_guard_moves_through_...);
        // even step -> "/ \".
        assert_eq!(leg_at(&startup_scene(0), 2, 11), "/ \\");
        // startup frame 1: guard at (8, 9); odd step -> " /|".
        assert_eq!(leg_at(&startup_scene(1), 8, 11), " /|");
    }

    #[test]
    fn tower_light_is_lit_only_at_the_final_startup_frame_and_the_first_exiting_frame() {
        assert!(startup_scene(5).contains('*'));
        assert!(!startup_scene(0).contains('*'));
        assert!(exit_scene(0).contains('*'));
        assert!(!exit_scene(5).contains('*'));
    }

    #[test]
    fn draw_animation_colors_rows_15_and_17_gray_and_others_cyan() {
        let scene = startup_scene(0);
        let backend = ratatui::backend::TestBackend::new(SCENE_WIDTH as u16, SCENE_HEIGHT as u16);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| draw_animation(frame, scene.clone()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        for y in 0..SCENE_HEIGHT as u16 {
            let expected = if y == 15 || y == 17 {
                Color::DarkGray
            } else {
                Color::Cyan
            };
            // The line's style applies across its whole rendered span
            // (including leading space characters), so column 0 reflects
            // the row's color regardless of what glyph sits there.
            assert_eq!(
                buffer[(0, y)].fg,
                expected,
                "row {y} should be {expected:?}"
            );
        }
    }

    fn render_app(app: &mut App, width: u16, height: u16) -> Buffer {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().buffer().clone()
    }

    fn buffer_lines(buffer: &Buffer) -> Vec<String> {
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect()
            })
            .collect()
    }

    fn buffer_contains(buffer: &Buffer, needle: &str) -> bool {
        buffer_lines(buffer)
            .iter()
            .any(|line| line.contains(needle))
    }

    fn render_text(app: &mut App) -> String {
        render_app(app, 140, 30)
            .content
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    fn test_app_in_help_mode() -> App {
        let mut app = App::new(Vec::new());
        app.open_help();
        app
    }

    fn help_scroll(app: &App) -> Option<u16> {
        match &app.mode {
            app::Mode::Help { scroll, .. } => Some(*scroll),
            _ => None,
        }
    }

    #[test]
    fn help_modal_is_centered_and_contains_grouped_bindings() {
        let mut app = test_app_in_help_mode();
        let buffer = render_app(&mut app, 100, 30);

        assert!(buffer_contains(&buffer, "Keyboard help"));
        for title in [
            "Navigation",
            "Details",
            "Repository actions",
            "Workspaces",
            "Input fields",
            "Confirmations",
            "Help",
        ] {
            assert!(buffer_contains(&buffer, title), "missing group {title}");
        }
        assert!(buffer_contains(&buffer, "r / R refresh selected / all"));
        assert!(buffer_contains(&buffer, "Esc / ? / F1 / q close help"));

        let lines = buffer_lines(&buffer);
        let top = lines
            .iter()
            .find(|line| line.contains("Keyboard help"))
            .expect("modal title row");
        let cells = top.chars().collect::<Vec<_>>();
        let left = cells.iter().position(|cell| *cell == '┌').unwrap();
        let right = cells.iter().rposition(|cell| *cell == '┐').unwrap();
        assert!(left.abs_diff(99 - right) <= 1, "modal must be centered");
    }

    #[test]
    fn help_modal_clamps_to_a_narrow_terminal() {
        let mut app = test_app_in_help_mode();
        let buffer = render_app(&mut app, 32, 12);
        let lines = buffer_lines(&buffer);
        let top = lines
            .iter()
            .find(|line| line.contains("Keyboard help"))
            .expect("modal title must remain visible");
        let cells = top.chars().collect::<Vec<_>>();

        assert_eq!(cells.iter().position(|cell| *cell == '┌'), Some(1));
        assert_eq!(cells.iter().rposition(|cell| *cell == '┐'), Some(30));
        assert!(buffer_contains(&buffer, "Esc / ? / F1 / q close help"));
        assert!(buffer_contains(&buffer, "↑/↓ scroll"));
    }

    #[test]
    fn help_modal_wraps_narrow_rows_instead_of_truncating_descriptions() {
        let mut app = test_app_in_help_mode();
        let buffer = render_app(&mut app, 32, 12);

        assert!(buffer_contains(&buffer, "select repository"));
    }

    #[test]
    fn help_modal_narrow_scroll_reaches_the_final_wrapped_description() {
        let mut app = test_app_in_help_mode();
        let app::Mode::Help { scroll, .. } = &mut app.mode else {
            panic!("test app should be in help mode");
        };
        *scroll = u16::MAX;

        let buffer = render_app(&mut app, 20, 12);

        assert!(buffer_contains(&buffer, "page"));
    }

    #[test]
    fn help_modal_scrolls_rows_but_keeps_controls_visible() {
        let mut app = test_app_in_help_mode();
        let at_top = render_app(&mut app, 48, 14);

        let app::Mode::Help { scroll, .. } = &mut app.mode else {
            panic!("test app should be in help mode");
        };
        *scroll = 16;
        let scrolled = render_app(&mut app, 48, 14);

        assert!(buffer_contains(&at_top, "Navigation"));
        assert!(!buffer_contains(&at_top, "Repository actions"));
        assert!(!buffer_contains(&scrolled, "Navigation"));
        assert!(buffer_contains(&scrolled, "Repository actions"));
        for buffer in [&at_top, &scrolled] {
            assert!(buffer_contains(buffer, "Keyboard help"));
            assert!(buffer_contains(buffer, "Esc / ? / F1 / q close help"));
            assert!(buffer_contains(buffer, "↑/↓ scroll"));
        }
    }

    #[test]
    fn help_modal_normalizes_stored_scroll_after_resize() {
        let mut app = test_app_in_help_mode();
        let app::Mode::Help { scroll, .. } = &mut app.mode else {
            panic!("test app should be in help mode");
        };
        *scroll = u16::MAX;

        let narrow = render_app(&mut app, 48, 14);
        assert!(buffer_contains(&narrow, "Keyboard help"));
        assert!(buffer_contains(&narrow, "Esc / ? / F1 / q close help"));
        assert_eq!(
            help_scroll(&app),
            Some(69),
            "narrow rendering must store the viewport maximum"
        );

        let wide = render_app(&mut app, 100, 30);
        assert!(buffer_contains(&wide, "Keyboard help"));
        assert!(buffer_contains(&wide, "Esc / ? / F1 / q close help"));
        assert_eq!(
            help_scroll(&app),
            Some(0),
            "resizing to a fully visible modal must reset stored overscroll"
        );
    }

    #[test]
    fn normal_footer_is_a_compact_catalog_hint() {
        let mut app = App::new(Vec::new());
        let buffer = render_app(&mut app, 140, 30);
        let footer = buffer_lines(&buffer)[28..].join("\n");

        assert!(footer.contains("? help · Enter details · / filter · r refresh · q quit"));
        assert!(!footer.contains("w workspace"));
        assert!(!footer.contains("m/u membership"));
    }

    #[test]
    fn help_modal_marks_selection_actions_unavailable_without_a_repository() {
        let mut app = test_app_in_help_mode();
        let buffer = render_app(&mut app, 100, 30);

        assert!(buffer_contains(&buffer, "d remove checkout (unavailable)"));
        assert!(buffer_contains(&buffer, "a add checkout"));
        assert!(!buffer_contains(&buffer, "a add checkout (unavailable)"));
    }

    #[test]
    fn help_modal_matches_terminal_busy_dispatch_availability() {
        let mut app = App::new(Vec::new());
        app.rows.push(test_row(LocalState::default()));
        app.action_busy = true;
        app.open_help();

        let buffer = render_app(&mut app, 100, 30);

        assert!(buffer_contains(&buffer, "t open terminal (unavailable)"));
        assert!(buffer_contains(&buffer, "o open remote page"));
        assert!(!buffer_contains(
            &buffer,
            "o open remote page (unavailable)"
        ));
    }

    #[test]
    fn help_mode_footer_shows_only_the_catalog_close_hint() {
        let mut app = App::new(Vec::new());
        app.open_help();

        let buffer = render_app(&mut app, 140, 30);
        let footer = buffer_lines(&buffer)[28..].join("\n");

        assert!(footer.contains("Esc / ? / F1 / q close help"));
        assert!(!footer.contains("a add"));
    }

    #[test]
    fn details_panel_switches_content_by_tab() {
        // If a tab's match arm were deleted, control would fall to the `_`
        // (log) arm instead, and that tab's marker text — which appears
        // nowhere in app.log — would be absent.
        let mut row = test_row(LocalState {
            head: "abc123head".into(),
            ..LocalState::default()
        });
        row.remote.prs = Observation::success(vec![crate::model::Item {
            title: "pr-marker".into(),
            ..Default::default()
        }]);
        row.remote.issues = Observation::success(vec![crate::model::Item {
            title: "issue-marker".into(),
            ..Default::default()
        }]);
        row.remote.proposals = Observation::success(vec![crate::model::Item {
            title: "proposal-marker".into(),
            ..Default::default()
        }]);

        let mut app = App::new(Vec::new());
        app.rows.push(row);

        app.tab = 0;
        assert!(render_text(&mut app).contains("HEAD: abc123head"));

        app.tab = 1;
        assert!(render_text(&mut app).contains("Local and cached remote-tracking branches"));

        app.tab = 2;
        assert!(render_text(&mut app).contains("pr-marker"));

        app.tab = 3;
        assert!(render_text(&mut app).contains("issue-marker"));

        app.tab = 4;
        assert!(render_text(&mut app).contains("proposal-marker"));
    }

    #[test]
    fn default_branch_placeholder_shows_error_glyph_only_when_it_itself_errored() {
        let mut error_row = test_row(LocalState::default());
        error_row.remote.default_branch = Observation::failure("boom");
        let mut app = App::new(Vec::new());
        app.rows.push(error_row);
        app.tab = 0;
        assert!(render_text(&mut app).contains("Default branch: !"));

        let mut loading_row = test_row(LocalState::default());
        loading_row.remote.default_branch = Observation::default();
        let mut app = App::new(Vec::new());
        app.rows.push(loading_row);
        app.tab = 0;
        assert!(render_text(&mut app).contains("Default branch: …"));
    }

    #[test]
    fn narrow_layout_breakpoint_is_at_exactly_110_columns() {
        let mut app = App::new(Vec::new());
        app.rows.push(test_row(LocalState::default()));

        let mut render_at = |width: u16| -> String {
            let backend = ratatui::backend::TestBackend::new(width, 30);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|c| c.symbol())
                .collect::<String>()
        };

        assert!(
            !render_at(109).contains("Review"),
            "109 columns should be narrow"
        );
        assert!(
            render_at(110).contains("Review"),
            "110 columns should be wide"
        );
    }

    #[test]
    fn hint_shows_the_busy_message_only_when_an_action_is_running() {
        let mut app = App::new(Vec::new());
        app.rows.push(test_row(LocalState::default()));

        assert!(
            render_text(&mut app).contains("? help"),
            "idle hint should show the full key list"
        );

        app.action_busy = true;
        let busy_text = render_text(&mut app);
        assert!(
            busy_text.contains("action running"),
            "busy hint should show while an action runs"
        );
        assert!(!busy_text.contains("? help"));
    }

    #[test]
    fn footer_second_line_shows_the_last_log_entry_only_when_it_is_non_empty() {
        let mut app = App::new(Vec::new());
        app.rows.push(test_row(LocalState::default()));
        app.log.push("footer-marker-text".into());
        assert!(render_text(&mut app).contains("footer-marker-text"));

        app.log.push(String::new());
        assert!(
            !render_text(&mut app).contains("footer-marker-text"),
            "an empty last entry must not resurrect the prior marker"
        );
    }

    #[test]
    fn action_status_color_reflects_failures_then_actionable_then_calm() {
        let color_at_action = |app: &mut App| -> Color {
            let backend = ratatui::backend::TestBackend::new(140, 30);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, app)).unwrap();
            let buffer = terminal.backend().buffer();
            for y in 0..buffer.area.height {
                let line: String = (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect();
                if let Some(byte_pos) = line.find("ACTION:") {
                    let char_pos = line[..byte_pos].chars().count() as u16;
                    return buffer[(char_pos, y)].fg;
                }
            }
            panic!("ACTION: not found in rendered output: nothing to sample");
        };

        let mut failing_app = App::new(Vec::new());
        let mut failing_row = test_row(LocalState::default());
        failing_row.remote.ci = Observation::success(vec![crate::model::Item {
            detail: "completed failure".into(),
            ..Default::default()
        }]);
        failing_app.rows.push(failing_row);
        assert_eq!(color_at_action(&mut failing_app), Color::Red);

        let mut actionable_app = App::new(Vec::new());
        actionable_app.rows.push(test_row(LocalState {
            modified: 1,
            ..LocalState::default()
        }));
        assert_eq!(color_at_action(&mut actionable_app), Color::Yellow);

        let mut calm_app = App::new(Vec::new());
        calm_app.rows.push(test_row(LocalState::default()));
        assert_eq!(color_at_action(&mut calm_app), Color::Green);
    }

    #[test]
    fn is_press_matches_only_the_press_event_kind() {
        assert!(is_press(&KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE
        )));
        let mut release = KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert!(!is_press(&release));
    }

    #[test]
    fn is_quit_hotkey_requires_both_ctrl_and_c() {
        assert!(is_quit_hotkey(&KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL
        )));
        assert!(!is_quit_hotkey(&KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::NONE
        )));
        assert!(!is_quit_hotkey(&KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL
        )));
    }

    #[test]
    fn remote_failed_is_true_only_when_one_of_the_three_calls_errored() {
        assert!(!remote_failed(&RemoteState::default()));

        let branch_errored = RemoteState {
            default_branch: Observation::failure("boom"),
            ..RemoteState::default()
        };
        assert!(remote_failed(&branch_errored));

        let prs_errored = RemoteState {
            prs: Observation::failure("boom"),
            ..RemoteState::default()
        };
        assert!(remote_failed(&prs_errored));

        let ci_errored = RemoteState {
            ci: Observation::failure("boom"),
            ..RemoteState::default()
        };
        assert!(remote_failed(&ci_errored));
    }

    #[test]
    fn next_failure_count_increments_and_caps_at_four_then_resets_on_success() {
        assert_eq!(next_failure_count(0, true), 1);
        assert_eq!(next_failure_count(1, true), 2);
        assert_eq!(next_failure_count(3, true), 4);
        assert_eq!(next_failure_count(4, true), 4, "capped at 4");
        assert_eq!(next_failure_count(4, false), 0, "a success resets to 0");
    }

    #[test]
    fn remote_backoff_doubles_from_a_one_minute_base() {
        assert_eq!(remote_backoff(0), Duration::from_secs(60));
        assert_eq!(remote_backoff(1), Duration::from_secs(120));
        assert_eq!(remote_backoff(4), Duration::from_secs(60 * 16));
    }
}
