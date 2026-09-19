use super::{
    app::{
        App, AttentionPriority, CI_FAIL_MARKERS, Mode, RowState, ci_failed_confirmed, known_count,
    },
    cache::CACHE_PLACEHOLDER,
    keys,
};
use crate::{
    git::LocalState,
    model::{Observation, RemoteState, clean},
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    prelude::Alignment,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
};

pub(super) fn attention_priority_color(priority: AttentionPriority) -> Color {
    match priority {
        AttentionPriority::Problem => Color::Red,
        AttentionPriority::Pending | AttentionPriority::LocalWork => Color::Yellow,
        AttentionPriority::Quiet => Color::Reset,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum CellState {
    Data,
    Loading,
    Error,
    Unsupported,
}

// Always prefer showing real data when present, even if a stale-cache
// placeholder or a real error is also set (retain_previous backfills data
// from a prior successful fetch without clearing the accompanying error).
pub(super) fn cell_state<T>(value: &Observation<T>) -> CellState {
    if !value.supported {
        CellState::Unsupported
    } else if value.data.is_some() {
        CellState::Data
    } else if value.error.is_none() || value.error.as_deref() == Some(CACHE_PLACEHOLDER) {
        CellState::Loading
    } else {
        CellState::Error
    }
}

// The more attention-worthy of two states, for a cell that combines two
// observations (e.g. "proposals / drafts") into one displayed value.
pub(super) fn worse_state(a: CellState, b: CellState) -> CellState {
    fn rank(s: CellState) -> u8 {
        match s {
            CellState::Error => 0,
            CellState::Loading => 1,
            CellState::Unsupported => 2,
            CellState::Data => 3,
        }
    }
    if rank(a) <= rank(b) { a } else { b }
}

// Placeholder states (loading/error/unsupported) get a muted, consistent
// treatment regardless of column; only real data earns the column's own
// color, so a wall of "…" or "n/a" doesn't compete visually with real counts.
pub(super) fn state_color(cell: CellState, data_color: Color) -> Color {
    match cell {
        CellState::Data => data_color,
        CellState::Loading | CellState::Unsupported => Color::DarkGray,
        CellState::Error => Color::Red,
    }
}

// A real-but-empty count ("0") is calm, informational data, not something
// worth the column's accent color — only a nonzero, actionable count earns
// it. Combines two observations (e.g. proposals + drafts) by taking the
// more attention-worthy of the two states first.
pub(super) fn count_color<T>(value: &Observation<Vec<T>>, accent: Color) -> Color {
    match cell_state(value) {
        CellState::Data if value.data.as_ref().is_some_and(|v| !v.is_empty()) => accent,
        CellState::Data => Color::DarkGray,
        other => state_color(other, accent),
    }
}

pub(super) fn count_color_combined<T, U>(
    a: &Observation<Vec<T>>,
    b: &Observation<Vec<U>>,
    accent: Color,
) -> Color {
    match worse_state(cell_state(a), cell_state(b)) {
        CellState::Data => {
            let any_nonempty = a.data.as_ref().is_some_and(|v| !v.is_empty())
                || b.data.as_ref().is_some_and(|v| !v.is_empty());
            if any_nonempty {
                accent
            } else {
                Color::DarkGray
            }
        }
        other => state_color(other, accent),
    }
}

pub(super) fn count<T>(value: &Observation<Vec<T>>) -> String {
    match cell_state(value) {
        CellState::Unsupported => "n/a".into(),
        CellState::Loading => "…".into(),
        CellState::Error => "!".into(),
        CellState::Data => value.data.as_ref().unwrap().len().to_string(),
    }
}

pub(super) fn spinner_frame(frame: usize) -> &'static str {
    ["·", "∘", "○", "◌", "○", "∘"][frame % 6]
}

pub(super) fn meter(value: usize, total: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let filled = if total == 0 {
        0
    } else {
        value.saturating_mul(width).div_ceil(total).min(width)
    };
    format!("{}{}", "█".repeat(filled), " ".repeat(width - filled))
}

pub(super) fn action_message(
    dirty: usize,
    behind: usize,
    failures: usize,
    releases: usize,
    prs: usize,
) -> String {
    if failures > 0 {
        return format!("inspect {failures} failing checks");
    }
    if dirty > 0 {
        return format!(
            "clean {dirty} dirty checkout{}",
            if dirty == 1 { "" } else { "s" }
        );
    }
    if behind > 0 {
        return format!(
            "pull {behind} repositor{}",
            if behind == 1 { "y" } else { "ies" }
        );
    }
    if releases > 0 && prs > 0 {
        return format!(
            "review {releases} release{} and {prs} pull request{}",
            if releases == 1 { "" } else { "s" },
            if prs == 1 { "" } else { "s" }
        );
    }
    if releases > 0 {
        return format!(
            "review {releases} release{}",
            if releases == 1 { "" } else { "s" }
        );
    }
    if prs > 0 {
        return format!(
            "review {prs} pull request{}",
            if prs == 1 { "" } else { "s" }
        );
    }
    "all clear".into()
}

pub(super) fn triage(app: &App) -> (usize, usize, usize, usize, usize, usize, String) {
    let mut dirty = 0;
    let mut behind = 0;
    let mut failures = 0;
    let mut prs = 0;
    let mut releases = 0;
    for row in &app.rows {
        if let Some(local) = &row.local.data {
            dirty += usize::from(local.dirty());
            behind += usize::from(local.behind > 0);
        }
        failures += usize::from(ci_failed_confirmed(&row.remote));
        prs += known_count(&row.remote.prs);
        releases += known_count(&row.remote.proposals) + known_count(&row.remote.drafts);
    }
    let actionable = failures + dirty + behind + prs + releases;
    (
        dirty,
        behind,
        failures,
        prs,
        releases,
        actionable,
        action_message(dirty, behind, failures, releases, prs),
    )
}

pub(super) fn health_glyph(row: &RowState) -> &'static str {
    if cell_state(&row.local) == CellState::Error
        || cell_state(&row.remote.default_branch) == CellState::Error
        || cell_state(&row.remote.ci) == CellState::Error
    {
        "!"
    } else if row.local.data.as_ref().is_some_and(LocalState::dirty) {
        "◆"
    } else if row.local.data.as_ref().is_some_and(|s| s.behind > 0) {
        "↓"
    } else if row.local.data.is_none() && (row.local_busy || row.remote_busy) {
        spinner_frame(animation_frame())
    } else if row.local.data.is_none() {
        "·"
    } else {
        "●"
    }
}

pub(super) fn animation_frame() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .saturating_div(140) as usize
}

pub(super) fn status_color(row: &RowState) -> Color {
    match health_glyph(row) {
        "!" => Color::Red,
        "◆" | "↓" => Color::Yellow,
        "·" => Color::DarkGray,
        _ => Color::Green,
    }
}

pub(super) fn ci_color(label: &str) -> Color {
    match label {
        "fail" => Color::Red,
        "running" => Color::Yellow,
        "pass" => Color::Green,
        "!" => Color::Red,
        _ => Color::DarkGray, // "n/a", "…", "absent", "other"
    }
}

pub(super) fn ci_label(state: &RemoteState) -> String {
    let observation = &state.ci;
    let v = match cell_state(observation) {
        CellState::Unsupported => return "n/a".into(),
        CellState::Loading => return "…".into(),
        CellState::Error => return "!".into(),
        CellState::Data => observation.data.as_ref().unwrap(),
    };
    if v.is_empty() {
        return "absent".into();
    }
    if v.iter()
        .any(|i| CI_FAIL_MARKERS.iter().any(|s| i.detail.contains(s)))
    {
        return "fail".into();
    }
    if v.iter().any(|i| {
        ["queued", "pending", "in_progress", "waiting", "requested"]
            .iter()
            .any(|s| i.detail.contains(s))
    }) {
        return "running".into();
    }
    if v.iter().all(|i| {
        ["success", "neutral", "skipped"]
            .iter()
            .any(|s| i.detail.contains(s))
    }) {
        return "pass".into();
    }
    // Real CI data that doesn't map cleanly to a known conclusion string
    // (e.g. a status context this matcher doesn't recognize yet) — distinct
    // from a fetch failure, so it gets its own label rather than "unknown".
    "other".into()
}

pub(super) fn items(label: &str, observation: &Observation<Vec<crate::model::Item>>) -> String {
    let mut s = format!("{label} [{}]\n", observation.label());
    if let Some(error) = &observation.error {
        s.push_str(&format!("{}\n", clean(error)));
    }
    if let Some(items) = &observation.data {
        if items.is_empty() {
            s.push_str("None observed\n");
        }
        for item in items {
            s.push_str(&format!(
                "{}\n  {}\n  {}\n",
                clean(&item.title),
                clean(&item.detail),
                clean(&item.url)
            ));
        }
    }
    s
}

const HELP_MAX_WIDTH: u16 = 98;
// At this width the modal has enough room for two readable binding columns.
const HELP_TWO_COLUMN_MIN_WIDTH: u16 = 96;

pub(super) fn help_rect(area: Rect, content_height: u16) -> Rect {
    let available_width = if area.width > 2 {
        area.width - 2
    } else {
        area.width
    };
    let available_height = if area.height > 2 {
        area.height - 2
    } else {
        area.height
    };
    let width = HELP_MAX_WIDTH.min(available_width);
    let height = content_height.saturating_add(3).min(available_height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn help_lines(groups: &[keys::BindingGroup], app: &App, stack_rows: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for group in groups {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::styled(
            group.title,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        for binding in group.bindings {
            let available = binding.is_available(app.current().is_some(), app.action_busy);
            let tone = if available {
                Color::Reset
            } else {
                Color::DarkGray
            };
            let description = if available {
                binding.description.to_string()
            } else {
                format!("{} (unavailable)", binding.description)
            };
            let key = Span::styled(
                binding.keys,
                Style::default().fg(if available {
                    Color::Cyan
                } else {
                    Color::DarkGray
                }),
            );
            let description = Span::styled(description, Style::default().fg(tone));
            if stack_rows {
                lines.push(Line::from(key));
                lines.push(Line::from(vec![Span::raw("  "), description]));
            } else {
                lines.push(Line::from(vec![key, Span::raw(" "), description]));
            }
        }
    }
    lines
}

fn wrapped_line_count(lines: &[Line<'static>], width: u16) -> u16 {
    if width == 0 {
        return 0;
    }
    lines
        .iter()
        .map(|line| line.width().div_ceil(width as usize).max(1))
        .sum::<usize>() as u16
}

fn draw_help_modal(frame: &mut Frame, app: &mut App) {
    let requested_scroll = match &app.mode {
        Mode::Help { scroll, .. } => *scroll,
        _ => return,
    };
    let two_columns = frame.area().width >= HELP_TWO_COLUMN_MIN_WIDTH;
    let groups = keys::catalog();
    let (left_groups, right_groups) = if two_columns {
        groups.split_at(3)
    } else {
        (groups, &[][..])
    };
    let left_lines = help_lines(left_groups, app, !two_columns);
    let right_lines = help_lines(right_groups, app, false);
    let panel_width = help_rect(frame.area(), 0).width;
    let content_width = panel_width.saturating_sub(2);
    let content_height = if two_columns {
        left_lines.len().max(right_lines.len()) as u16
    } else {
        wrapped_line_count(&left_lines, content_width)
    };
    let area = help_rect(frame.area(), content_height);
    if area.is_empty() {
        return;
    }

    let panel = Block::bordered()
        .title(" Keyboard help ")
        .title_alignment(Alignment::Center)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = panel.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(panel, area);

    let initial_body_height = inner.height.saturating_sub(1);
    let overflow = content_height > initial_body_height;
    let controls_height = if overflow { 2 } else { 1 }.min(inner.height);
    let body_height = inner.height.saturating_sub(controls_height);
    let max_scroll = content_height.saturating_sub(body_height);
    let scroll = requested_scroll.min(max_scroll);
    if let Mode::Help { scroll: stored, .. } = &mut app.mode {
        *stored = scroll;
    }
    let vertical = Layout::vertical([
        Constraint::Length(body_height),
        Constraint::Length(controls_height),
    ])
    .split(inner);

    if two_columns {
        let columns = Layout::horizontal([
            Constraint::Fill(1),
            Constraint::Length(1),
            Constraint::Fill(1),
        ])
        .split(vertical[0]);
        frame.render_widget(Paragraph::new(left_lines).scroll((scroll, 0)), columns[0]);
        frame.render_widget(Paragraph::new(right_lines).scroll((scroll, 0)), columns[2]);
    } else {
        frame.render_widget(
            Paragraph::new(left_lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0)),
            vertical[0],
        );
    }

    let mut controls = vec![Line::styled(
        keys::help_close_hint(),
        Style::default().fg(Color::Cyan),
    )];
    if overflow {
        controls.push(Line::styled(
            "↑/↓ scroll",
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(
        Paragraph::new(controls).alignment(Alignment::Center),
        vertical[1],
    );
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let areas = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(3),
        Constraint::Percentage(42),
        Constraint::Min(6),
        Constraint::Length(2),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!(
            " P H R O U R I O N  |  workspace: {}  |  {} repositories  |  filter: {}",
            app.active_workspace.as_deref().unwrap_or("All"),
            app.visible().len(),
            clean(&app.filter)
        ))
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        areas[0],
    );
    let (dirty, behind, failures, prs, releases, actionable, action) = triage(app);
    let spinner = if app.action_busy {
        format!("{} ", spinner_frame(animation_frame()))
    } else {
        String::new()
    };
    let watchline = app
        .rows
        .iter()
        .map(health_glyph)
        .collect::<Vec<_>>()
        .join("");
    let calm = app
        .rows
        .iter()
        .filter(|row| health_glyph(row) == "●")
        .count();
    let action_color = if failures > 0 {
        Color::Red
    } else if actionable > 0 {
        Color::Yellow
    } else {
        Color::Green
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {spinner}{dirty} dirty  "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{behind} behind  "),
                Style::default().fg(Color::Blue),
            ),
            Span::styled(format!("{prs} PRs  "), Style::default().fg(Color::Magenta)),
            Span::styled(
                format!("{releases} releases  "),
                Style::default().fg(Color::Yellow),
            ),
            Span::styled(
                format!("{failures} failing  "),
                Style::default().fg(Color::Red),
            ),
            Span::styled(
                format!("ACTION: {action}  "),
                Style::default()
                    .fg(action_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("health {}  ", meter(calm, app.rows.len(), 8)),
                Style::default().fg(Color::Green),
            ),
            Span::styled(
                format!("watch {watchline}"),
                Style::default().fg(Color::Cyan),
            ),
        ]))
        .block(
            Block::bordered()
                .title(" Triage  /  WATCH ")
                .border_style(Style::default().fg(Color::DarkGray)),
        ),
        areas[1],
    );
    let visible = app.visible();
    let narrow = frame.area().width < 110;
    struct RowText {
        repo_label: String,
        attention: String,
        attention_color: Color,
        branch: String,
        local: String,
        sync: String,
        prs: String,
        prs_color: Color,
        review: String,
        review_color: Color,
        issues: String,
        issues_color: Color,
        rel_draft: String,
        rel_draft_color: Color,
        ci: String,
        tone: Color,
        ci_color: Color,
    }
    let row_texts: Vec<RowText> = visible
        .iter()
        .map(|&i| {
            let r = &app.rows[i];
            let (branch, local, sync) = match &r.local.data {
                Some(s) => (
                    clean(&s.branch),
                    if s.dirty() {
                        format!(
                            "S{} M{} ?{} !{}",
                            s.staged, s.modified, s.untracked, s.conflicts
                        )
                    } else {
                        "clean".into()
                    },
                    s.sync(),
                ),
                None => {
                    let placeholder = match cell_state(&r.local) {
                        CellState::Error => "!",
                        _ => "…",
                    };
                    ("?".into(), r.local.label(), placeholder.into())
                }
            };
            let local = if r.local.error.is_some() {
                r.local.label()
            } else {
                local
            };
            let ci = ci_label(&r.remote);
            let ci_color = ci_color(&ci);
            let rel_draft_color =
                count_color_combined(&r.remote.proposals, &r.remote.drafts, Color::Yellow);
            let (attention_priority, attention) = r.attention();
            let attention_color = attention_priority_color(attention_priority);
            RowText {
                repo_label: format!("{} {}", health_glyph(r), clean(&r.repo.name)),
                attention,
                attention_color,
                branch,
                local,
                sync,
                prs: count(&r.remote.prs),
                prs_color: count_color(&r.remote.prs, Color::Magenta),
                review: count(&r.remote.review_requests),
                review_color: count_color(&r.remote.review_requests, Color::LightMagenta),
                issues: count(&r.remote.issues),
                issues_color: count_color(&r.remote.issues, Color::Cyan),
                rel_draft: format!(
                    "{} / {}",
                    count(&r.remote.proposals),
                    count(&r.remote.drafts)
                ),
                rel_draft_color,
                ci,
                tone: status_color(r),
                ci_color,
            }
        })
        .collect();
    let column_width = |header: &str, get: fn(&RowText) -> &str| -> u16 {
        row_texts
            .iter()
            .map(|r| get(r).chars().count())
            .max()
            .unwrap_or(0)
            .max(header.chars().count()) as u16
    };
    let local_w = column_width("Local", |r| &r.local);
    let attention_w = column_width("Attention", |r| &r.attention);
    let sync_w = column_width("Sync*", |r| &r.sync);
    let prs_w = column_width("PRs", |r| &r.prs);
    let review_w = column_width("Review", |r| &r.review);
    let issues_w = column_width("Issues", |r| &r.issues);
    let rel_draft_w = column_width("Rel / Draft", |r| &r.rel_draft);
    let ci_w = column_width("CI", |r| &r.ci);
    let right = |s: String, style: Style| {
        Cell::from(Line::from(s).alignment(Alignment::Right).style(style))
    };
    let rows = row_texts.into_iter().map(|r| {
        let mut cells = vec![
            Cell::from(r.repo_label).style(Style::default().fg(r.tone)),
            Cell::from(r.attention).style(Style::default().fg(r.attention_color)),
        ];
        if !narrow {
            cells.extend([
                Cell::from(r.branch).style(Style::default().fg(Color::Blue)),
                right(r.local, Style::default().fg(r.tone)),
            ]);
        }
        cells.push(right(r.sync, Style::default().fg(r.tone)));
        if !narrow {
            cells.extend([
                right(r.prs, Style::default().fg(r.prs_color)),
                right(r.review, Style::default().fg(r.review_color)),
                right(r.issues, Style::default().fg(r.issues_color)),
                right(r.rel_draft, Style::default().fg(r.rel_draft_color)),
                right(r.ci, Style::default().fg(r.ci_color)),
            ]);
        }
        Row::new(cells)
    });
    let heading = |s: &'static str| Cell::from(Line::from(s).alignment(Alignment::Right));
    let mut headings = vec![Cell::from("Repository"), Cell::from("Attention")];
    let mut widths = vec![Constraint::Fill(5), Constraint::Length(attention_w)];
    if !narrow {
        headings.extend([Cell::from("Branch"), heading("Local")]);
        widths.extend([Constraint::Fill(8), Constraint::Length(local_w)]);
    }
    headings.push(heading("Sync*"));
    widths.push(Constraint::Length(sync_w));
    if !narrow {
        headings.extend([
            heading("PRs"),
            heading("Review"),
            heading("Issues"),
            heading("Rel / Draft"),
            heading("CI"),
        ]);
        widths.extend([
            Constraint::Length(prs_w),
            Constraint::Length(review_w),
            Constraint::Length(issues_w),
            Constraint::Length(rel_draft_w),
            Constraint::Length(ci_w),
        ]);
    }
    let table = Table::new(rows, widths)
        .header(Row::new(headings).style(Style::default().fg(Color::Yellow)))
        .block(
            Block::bordered()
                .title(" Checkouts / attention first ")
                .border_style(Style::default().fg(Color::DarkGray)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ");
    frame.render_stateful_widget(
        table,
        areas[2],
        &mut TableState::default().with_selected(Some(app.selected)),
    );
    let mut details = String::new();
    if let Some(i) = app.current() {
        let row = &app.rows[i];
        details.push_str(&format!(
            "Folder: {}\nRemote: {} ({}/{})\n",
            clean(&row.repo.path.display().to_string()),
            clean(&row.repo.remote),
            clean(&row.repo.identity.host),
            clean(&row.repo.identity.project)
        ));
        details.push_str(&format!(
            "Workspaces: {}\n",
            if row.repo.workspaces.is_empty() {
                "-".into()
            } else {
                row.repo.workspaces.join(", ")
            }
        ));
        details.push_str(&format!(
            "*Sync uses Git tracking refs. Last fetch: {}\n",
            row.fetched
                .map(|t| format!("{}s ago", t.elapsed().as_secs()))
                .unwrap_or_else(|| "not fetched this session".into())
        ));
        match app.tab {
            0 => {
                if let Some(s) = &row.local.data {
                    details.push_str(&format!(
                        "HEAD: {}\nUpstream: {} | operation in progress: {}\n",
                        s.head,
                        clean(&s.upstream),
                        s.operation
                    ));
                    for change in &s.changes {
                        details.push_str(&format!("{}\n", clean(change)));
                    }
                }
                if let Some(e) = &row.local.error {
                    details.push_str(&format!("{}\n", clean(e)));
                }
                let default_branch_placeholder = match cell_state(&row.remote.default_branch) {
                    CellState::Error => "!",
                    _ => "…",
                };
                details.push_str(&format!(
                    "Default branch: {} [{}]\n",
                    clean(
                        row.remote
                            .default_branch
                            .data
                            .as_deref()
                            .unwrap_or(default_branch_placeholder)
                    ),
                    row.remote.default_branch.label()
                ));
                details.push_str(&items("Default branch checks", &row.remote.ci));
            }
            1 => {
                details.push_str("Local and cached remote-tracking branches\n");
                if let Some(s) = &row.local.data {
                    for b in &s.branches {
                        details.push_str(&format!("{}\n", clean(b)));
                    }
                }
                details.push_str(&items("Remote branches", &row.remote.branches));
            }
            2 => {
                details.push_str(&items(
                    "Open pull requests (open in browser for head checks)",
                    &row.remote.prs,
                ));
                details.push_str(&items("Awaiting your review", &row.remote.review_requests));
            }
            3 => {
                let label = if row.repo.issue_labels.is_empty() {
                    "Open issues (configure issue_labels to filter)".to_string()
                } else {
                    format!(
                        "Open issues (filtered by label: {})",
                        row.repo.issue_labels.join(", ")
                    )
                };
                details.push_str(&items(&label, &row.remote.issues));
            }
            4 => {
                details.push_str(&items("Release proposals", &row.remote.proposals));
                details.push_str(&items("Draft releases", &row.remote.drafts));
                details.push_str(&items(
                    "Publication workflows (configure release_workflows to enable)",
                    &row.remote.publication,
                ));
                details.push_str(&items("Published releases", &row.remote.published));
            }
            _ => details.push_str(
                &app.log
                    .iter()
                    .map(|s| s.lines().map(clean).collect::<Vec<_>>().join("\n"))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
            ),
        }
    } else {
        details.push_str(
            "No repositories in this view. Press w to switch workspace, n to create one, a to add a checkout, or use phrourion discover PATH.",
        );
    }
    let title = [
        "1 State",
        "2 Branches",
        "3 PRs",
        "4 Issues",
        "5 Releases",
        "6 Actions",
    ][app.tab];
    frame.render_widget(
        Paragraph::new(details)
            .block(
                Block::bordered()
                    .title(title)
                    .border_style(Style::default().fg(Color::DarkGray)),
            )
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        areas[3],
    );
    let hint = match &app.mode {
        Mode::Add(s) => format!(
            "Add: PATH | REMOTE (remote optional): {}  [Enter save / Esc cancel]",
            clean(s)
        ),
        Mode::Filter => "Type filter, Enter done, Esc clear".into(),
        Mode::Workspace(s) => format!(
            "Workspace: {}  [Enter select / empty All / Esc cancel]",
            clean(s)
        ),
        Mode::CreateWorkspace(s) => {
            format!("Create workspace: {}  [Enter save / Esc cancel]", clean(s))
        }
        Mode::AddWorkspace(s) => format!(
            "Add selected repo to workspace (blank = active): {}  [Enter save / Esc cancel]",
            clean(s)
        ),
        Mode::RemoveWorkspace(s) => format!(
            "Remove selected repo from workspace (blank = active): {}  [Enter save / Esc cancel]",
            clean(s)
        ),
        Mode::Confirm(p) => format!(
            "Pull {} [{}] {} -> {} ({} commits)? y / n",
            clean(&p.repo.path.display().to_string()),
            clean(&p.before.branch),
            &p.before.head[..7.min(p.before.head.len())],
            &p.target[..7.min(p.target.len())],
            p.before.behind
        ),
        Mode::Checkout(s) => format!(
            "Checkout PR number: {}  [Enter preview / Esc cancel]",
            clean(s)
        ),
        Mode::ConfirmCheckout(p) => format!(
            "Checkout PR #{} as {} in {}? y / n",
            p.number,
            clean(&p.branch),
            clean(&p.repo.path.display().to_string())
        ),
        Mode::Remove(name) => format!("Remove {} from registry only? y / n", clean(name)),
        Mode::Help { .. } => keys::help_close_hint(),
        Mode::Normal if app.action_busy => format!(
            "{} action running... monitoring remains available",
            spinner_frame(animation_frame())
        ),
        Mode::Normal => keys::compact_footer_hint(),
    };
    let mut footer = vec![Line::from(vec![
        Span::styled(
            "Aranea Development  |  ",
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(hint, Style::default().fg(Color::Cyan)),
    ])];
    if matches!(app.mode, Mode::Normal if !app.action_busy)
        && let Some(last) = app
            .log
            .last()
            .map(|s| clean(s.lines().next().unwrap_or("")))
        && !last.is_empty()
    {
        footer.push(Line::styled(last, Style::default().fg(Color::Cyan)));
    }
    frame.render_widget(Paragraph::new(footer), areas[4]);
    draw_help_modal(frame, app);
}
