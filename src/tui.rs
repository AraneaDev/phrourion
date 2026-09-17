use crate::{
    git::{self, CheckoutPreview, LocalState, PullPreview},
    model::{Observation, RemoteState, Repo, clean},
    provider, registry,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Flex, Layout},
    prelude::Alignment,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap},
};
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
};

#[derive(Clone)]
pub struct RowState {
    pub repo: Repo,
    pub local: Observation<LocalState>,
    pub remote: RemoteState,
    pub fetched: Option<Instant>,
    pub local_busy: bool,
    pub remote_busy: bool,
    pub next_remote: Instant,
    pub failures: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AttentionPriority {
    Problem,
    Pending,
    LocalWork,
    Quiet,
}

impl RowState {
    fn attention(&self) -> (AttentionPriority, String) {
        use AttentionPriority::*;
        let local = self
            .local
            .data
            .as_ref()
            .filter(|_| self.local.supported && self.local.error.is_none());
        if local.is_some_and(|s| s.conflicts > 0) {
            return (Problem, "Conflicts".into());
        }
        if local.is_some_and(|s| !s.upstream.is_empty() && s.ahead > 0 && s.behind > 0) {
            return (Problem, "Diverged".into());
        }
        if ci_failed_confirmed(&self.remote) {
            return (Problem, "CI failed".into());
        }
        if let Some(s) = local.filter(|s| !s.upstream.is_empty()) {
            if s.ahead > 0 {
                return (Pending, format!("{} unpushed", s.ahead));
            }
            if s.behind > 0 {
                return (Pending, format!("{} incoming", s.behind));
            }
        }
        if known_count(&self.remote.proposals) + known_count(&self.remote.drafts) > 0 {
            return (Pending, "Release pending".into());
        }
        if local.is_some_and(LocalState::dirty) {
            return (LocalWork, "Local changes".into());
        }
        // An empty reason does not claim that unavailable observations are healthy.
        (Quiet, String::new())
    }
}

enum Message {
    Local(String, Box<Observation<LocalState>>),
    Remote(String, Box<RemoteState>),
    Fetched(String, Result<()>),
    Preview(Box<Result<PullPreview>>),
    CheckoutPreview(Box<Result<CheckoutPreview>>),
    Action(Result<String>),
    Added(Result<()>),
}

enum Mode {
    Normal,
    Add(String),
    Filter,
    Confirm(Box<PullPreview>),
    Checkout(String),
    ConfirmCheckout(Box<CheckoutPreview>),
    Remove(String),
    Help,
}

pub struct App {
    pub rows: Vec<RowState>,
    pub selected: usize,
    pub filter: String,
    pub tab: usize,
    pub scroll: u16,
    pub log: Vec<String>,
    mode: Mode,
    action_busy: bool,
}

const STARTUP_FRAMES: usize = 6;
const STARTUP_FRAME_MS: u64 = 900;
const STARTUP_DURATION_MS: u64 = STARTUP_FRAME_MS * STARTUP_FRAMES as u64;
const EXIT_FRAMES: usize = STARTUP_FRAMES;
const SCENE_WIDTH: usize = 40;
const SCENE_HEIGHT: usize = 18;

fn tower_scene(frame: usize, exiting: bool) -> String {
    let step = frame.min(5);
    let mut grid = vec![vec![' '; SCENE_WIDTH]; SCENE_HEIGHT];
    fn put(grid: &mut [Vec<char>], x: usize, y: usize, text: &str) {
        for (offset, ch) in text.chars().enumerate() {
            if let Some(cell) = grid.get_mut(y).and_then(|row| row.get_mut(x + offset)) {
                *cell = ch;
            }
        }
    }
    fn centered(grid: &mut [Vec<char>], y: usize, text: &str) {
        put(grid, (SCENE_WIDTH - text.len()) / 2, y, text);
    }
    centered(&mut grid, 0, "P H R O U R I O N");
    // Tower geometry never changes. All sprites use the same cell coordinates.
    for (y, row) in [
        "   /\\   ",
        "  /##\\  ",
        " /####\\ ",
        "|      |",
        "|      |",
        "|------|",
        "|      |",
        "|      |",
        "|      |",
        "|______|",
    ]
    .iter()
    .enumerate()
    {
        put(&mut grid, 16, y + 2, row);
    }
    let guard = if exiting {
        match step {
            0 => Some((18, 5)),
            1 => Some((18, 7)),
            2 => Some((24, 9)),
            3 => Some((29, 9)),
            4 => Some((34, 9)),
            _ => None,
        }
    } else {
        Some(match step {
            0 => (2, 9),
            1 => (8, 9),
            2 => (13, 9),
            3 => (18, 9),
            4 => (18, 7),
            _ => (18, 5),
        })
    };
    if let Some((x, y)) = guard {
        put(&mut grid, x, y, " o ");
        put(&mut grid, x, y + 1, "/|\\");
        put(
            &mut grid,
            x,
            y + 2,
            if step % 2 == 0 { "/ \\" } else { " /|" },
        );
    }
    let lit = (!exiting && step == 5) || (exiting && step == 0);
    if lit {
        put(&mut grid, 22, 5, "*");
    }
    let caption = match (exiting, step) {
        (true, 5) => "The tower is dark",
        (true, _) => "Leaving the watch",
        (false, 5) => "WATCH ESTABLISHED",
        (false, _) => "Taking the watch",
    };
    centered(&mut grid, 13, caption);
    centered(&mut grid, 15, "Aranea Development");
    centered(
        &mut grid,
        17,
        if exiting {
            "[ Space / q ] skip"
        } else {
            "[ Space ] skip"
        },
    );
    grid.into_iter()
        .map(|row| row.into_iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn draw_animation(frame: &mut Frame, scene: String) {
    let vertical = Layout::vertical([Constraint::Length(SCENE_HEIGHT as u16)])
        .flex(Flex::Center)
        .split(frame.area())[0];
    let area = Layout::horizontal([Constraint::Length(SCENE_WIDTH as u16)])
        .flex(Flex::Center)
        .split(vertical)[0];
    let lines: Vec<Line> = scene
        .lines()
        .enumerate()
        .map(|(y, line)| {
            Line::styled(
                line.to_owned(),
                Style::default().fg(if y == 15 || y == 17 {
                    Color::DarkGray
                } else {
                    Color::Cyan
                }),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Left), area);
}

fn startup_scene(frame: usize) -> String {
    tower_scene(frame, false)
}

fn exit_scene(frame: usize) -> String {
    tower_scene(frame, true)
}

fn startup_skips(key: KeyCode) -> bool {
    key == KeyCode::Char(' ')
}

fn exit_skips(key: KeyCode) -> bool {
    matches!(key, KeyCode::Char('q') | KeyCode::Char(' '))
}

fn cache_path(repo: &Repo) -> Option<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", repo.identity).hash(&mut hasher);
    repo.release_workflows.hash(&mut hasher);
    repo.release_labels.hash(&mut hasher);
    repo.issue_labels.hash(&mut hasher);
    Some(
        dirs::cache_dir()?
            .join("phrourion")
            .join(format!("{:x}.json", hasher.finish())),
    )
}

// The placeholder `cached()` attaches to every field before this session's
// own fetch resolves it one way or the other. Rendering code checks for this
// exact string to distinguish it from a real fetch failure, so a repo that
// hasn't actually failed this session isn't shown as errored.
const CACHE_PLACEHOLDER: &str = "Cached; awaiting refresh";

fn cached(repo: &Repo) -> RemoteState {
    let state: Option<RemoteState> = cache_path(repo)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|s| serde_json::from_slice(&s).ok());
    if let Some(state) = state {
        let unavailable = RemoteState {
            default_branch: Observation::failure(CACHE_PLACEHOLDER),
            branches: Observation::failure(CACHE_PLACEHOLDER),
            prs: Observation::failure(CACHE_PLACEHOLDER),
            review_requests: Observation::failure(CACHE_PLACEHOLDER),
            issues: Observation::failure(CACHE_PLACEHOLDER),
            proposals: Observation::failure(CACHE_PLACEHOLDER),
            drafts: Observation::failure(CACHE_PLACEHOLDER),
            published: Observation::failure(CACHE_PLACEHOLDER),
            ci: Observation::failure(CACHE_PLACEHOLDER),
            publication: Observation::failure(CACHE_PLACEHOLDER),
        };
        unavailable.retain_previous(&state)
    } else {
        RemoteState::default()
    }
}

fn save_cache(repo: &Repo, state: &RemoteState) {
    if let Some(path) = cache_path(repo) {
        let result = (|| -> Result<()> {
            use std::io::Write;
            let parent = path.parent().unwrap();
            std::fs::create_dir_all(parent)?;
            let mut file = tempfile::NamedTempFile::new_in(parent)?;
            file.write_all(&serde_json::to_vec(state)?)?;
            file.persist(path)?;
            Ok(())
        })();
        // Cache is optional. A failed write never hides an observed remote error.
        let _ = result;
    }
}

impl App {
    pub fn new(repos: Vec<Repo>) -> Self {
        Self {
            rows: repos
                .into_iter()
                .filter(|r| r.enabled)
                .map(|repo| RowState {
                    remote: cached(&repo),
                    repo,
                    local: Observation::default(),
                    fetched: None,
                    local_busy: false,
                    remote_busy: false,
                    next_remote: Instant::now(),
                    failures: 0,
                })
                .collect(),
            selected: 0,
            filter: String::new(),
            tab: 0,
            scroll: 0,
            log: Vec::new(),
            mode: Mode::Normal,
            action_busy: false,
        }
    }
    fn visible(&self) -> Vec<usize> {
        let query = self.filter.to_lowercase();
        let mut visible: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                format!("{} {}", r.repo.name, r.repo.path.display())
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
        visible.sort_by_cached_key(|&i| {
            let row = &self.rows[i];
            (
                row.attention().0,
                clean(&row.repo.name).to_lowercase(),
                row.repo.id.clone(),
            )
        });
        visible
    }
    fn current(&self) -> Option<usize> {
        self.visible().get(self.selected).copied()
    }
    fn update_rows(&mut self, update: impl FnOnce(&mut Vec<RowState>)) {
        let selected_id = self.current().map(|i| self.rows[i].repo.id.clone());
        update(&mut self.rows);
        let visible = self.visible();
        self.selected = selected_id
            .and_then(|id| visible.iter().position(|&i| self.rows[i].repo.id == id))
            .unwrap_or_else(|| self.selected.min(visible.len().saturating_sub(1)));
    }
    fn record(&mut self, s: impl ToString) {
        self.log.push(s.to_string());
        if self.log.len() > 100 {
            self.log.remove(0);
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum CellState {
    Data,
    Loading,
    Error,
    Unsupported,
}

// Always prefer showing real data when present, even if a stale-cache
// placeholder or a real error is also set (retain_previous backfills data
// from a prior successful fetch without clearing the accompanying error).
fn cell_state<T>(value: &Observation<T>) -> CellState {
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
fn worse_state(a: CellState, b: CellState) -> CellState {
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
fn state_color(cell: CellState, data_color: Color) -> Color {
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
fn count_color<T>(value: &Observation<Vec<T>>, accent: Color) -> Color {
    match cell_state(value) {
        CellState::Data if value.data.as_ref().is_some_and(|v| !v.is_empty()) => accent,
        CellState::Data => Color::DarkGray,
        other => state_color(other, accent),
    }
}

fn count_color_combined<T, U>(
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

fn count<T>(value: &Observation<Vec<T>>) -> String {
    match cell_state(value) {
        CellState::Unsupported => "n/a".into(),
        CellState::Loading => "…".into(),
        CellState::Error => "!".into(),
        CellState::Data => value.data.as_ref().unwrap().len().to_string(),
    }
}

fn spinner_frame(frame: usize) -> &'static str {
    ["·", "∘", "○", "◌", "○", "∘"][frame % 6]
}

fn meter(value: usize, total: usize, width: usize) -> String {
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

fn known_count<T>(value: &Observation<Vec<T>>) -> usize {
    if value.supported && value.error.is_none() {
        value.data.as_ref().map_or(0, Vec::len)
    } else {
        0
    }
}

fn action_message(
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

fn triage(app: &App) -> (usize, usize, usize, usize, usize, usize, String) {
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

fn health_glyph(row: &RowState) -> &'static str {
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

// A repo moving into Problem or Pending from a calmer tier is worth a notification;
// staying within the tier (e.g. one unpushed commit becoming two) is not.
fn entered_notice_tier(before: AttentionPriority, after: AttentionPriority) -> bool {
    after <= AttentionPriority::Pending && before > AttentionPriority::Pending
}

fn applescript_string_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn notify_script(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        applescript_string_literal(body),
        applescript_string_literal(title)
    )
}

async fn notify(repo_name: &str, reason: &str, cwd: &Path) {
    let title = clean(repo_name);
    let body = clean(reason);
    let (program, args) = if cfg!(target_os = "macos") {
        (
            "osascript".to_string(),
            vec!["-e".to_string(), notify_script(&title, &body)],
        )
    } else {
        ("notify-send".to_string(), vec![title, body])
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let _ = crate::command::run(&program, &args, cwd).await;
}

fn notify_on_attention_entry(
    app: &App,
    tasks: &mut JoinSet<()>,
    id: &str,
    before: Option<(AttentionPriority, String)>,
) {
    let Some((before_priority, _)) = before else {
        return;
    };
    let Some(row) = app.rows.iter().find(|r| r.repo.id == id) else {
        return;
    };
    let (after_priority, reason) = row.attention();
    if entered_notice_tier(before_priority, after_priority) {
        let name = row.repo.name.clone();
        let path = row.repo.path.clone();
        tasks.spawn(async move {
            notify(&name, &reason, &path).await;
        });
    }
}

fn animation_frame() -> usize {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .saturating_div(140) as usize
}

fn status_color(row: &RowState) -> Color {
    match health_glyph(row) {
        "!" => Color::Red,
        "◆" | "↓" => Color::Yellow,
        "·" => Color::DarkGray,
        _ => Color::Green,
    }
}

const CI_FAIL_MARKERS: &[&str] = &[
    "failure",
    "timed_out",
    "cancelled",
    "action_required",
    "error",
    "warning",
];

// A confirmed-fresh CI failure, for attention ranking only. Deliberately
// stricter than ci_label(): a stale or currently-errored check is not a
// confirmed attention signal (see README's "Actions" section), even though
// the table still displays its last-known value via ci_label().
fn ci_failed_confirmed(state: &RemoteState) -> bool {
    let observation = &state.ci;
    observation.supported
        && observation.error.is_none()
        && observation.data.as_ref().is_some_and(|v| {
            v.iter()
                .any(|i| CI_FAIL_MARKERS.iter().any(|s| i.detail.contains(s)))
        })
}

fn ci_label(state: &RemoteState) -> String {
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

fn items(label: &str, observation: &Observation<Vec<crate::model::Item>>) -> String {
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

pub fn draw(frame: &mut Frame, app: &App) {
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
            " P H R O U R I O N  |  {} repositories  |  filter: {}",
            app.rows.len(),
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
        .block(Block::bordered().title(" Triage  /  WATCH "))
        .style(Style::default().fg(Color::Gray)),
        areas[1],
    );
    let visible = app.visible();
    let narrow = frame.area().width < 110;
    struct RowText {
        repo_label: String,
        attention: String,
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
            let ci_color = match ci.as_str() {
                "fail" => Color::Red,
                "running" => Color::Yellow,
                "pass" => Color::Green,
                "!" => Color::Red,
                _ => Color::DarkGray, // "n/a", "…", "absent", "other"
            };
            let rel_draft_color =
                count_color_combined(&r.remote.proposals, &r.remote.drafts, Color::Yellow);
            RowText {
                repo_label: format!("{} {}", health_glyph(r), clean(&r.repo.name)),
                attention: r.attention().1,
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
            Cell::from(r.attention),
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
        .block(Block::bordered().title(" Checkouts / attention first "))
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
            "No matching repositories. Press a to add a checkout, or use phrourion discover PATH.",
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
            .block(Block::bordered().title(title))
            .wrap(Wrap { trim: false })
            .scroll((app.scroll, 0)),
        areas[3],
    );
    let hint = match &app.mode {
        Mode::Add(s) => format!("Add: PATH | REMOTE (remote optional): {}  [Enter save / Esc cancel]", clean(s)),
        Mode::Filter => "Type filter, Enter done, Esc clear".into(),
        Mode::Confirm(p) => format!("Pull {} [{}] {} -> {} ({} commits)? y / n", clean(&p.repo.path.display().to_string()), clean(&p.before.branch), &p.before.head[..7.min(p.before.head.len())], &p.target[..7.min(p.target.len())], p.before.behind),
        Mode::Checkout(s) => format!("Checkout PR number: {}  [Enter preview / Esc cancel]", clean(s)),
        Mode::ConfirmCheckout(p) => format!("Checkout PR #{} as {} in {}? y / n", p.number, clean(&p.branch), clean(&p.repo.path.display().to_string())),
        Mode::Remove(name) => format!("Remove {} from registry only? y / n", clean(name)),
        Mode::Help => "j/k move | 1-6 tabs | PgUp/PgDn scroll | a add | d remove | c checkout PR | / filter | r fetch/refresh | R all | p pull | o browser | q quit | Esc close".into(),
        Mode::Normal if app.action_busy => format!("{} action running... monitoring remains available", spinner_frame(animation_frame())),
        Mode::Normal => "a add  d remove  c checkout PR  / filter  1-6 details  r refresh  p pull  o browser  ? help  q quit".into(),
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
}

async fn startup_screen(terminal: &mut DefaultTerminal) -> Result<()> {
    let started = Instant::now();
    let frame_time = Duration::from_millis(STARTUP_FRAME_MS);
    let duration = Duration::from_millis(STARTUP_DURATION_MS);
    terminal.clear()?;
    loop {
        if event::poll(Duration::from_millis(20))?
            && let Event::Key(key) = event::read()?
            && startup_skips(key.code)
        {
            break;
        }
        let elapsed = started.elapsed();
        if elapsed >= duration {
            break;
        }
        let frame = elapsed.as_millis() as usize / frame_time.as_millis() as usize;
        terminal.draw(|area| draw_animation(area, startup_scene(frame)))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    terminal.clear()?;
    Ok(())
}

async fn exit_screen(terminal: &mut DefaultTerminal) -> Result<()> {
    let started = Instant::now();
    let frame_time = Duration::from_millis(STARTUP_FRAME_MS);
    let duration = frame_time * EXIT_FRAMES as u32;
    terminal.clear()?;
    loop {
        if event::poll(Duration::from_millis(20))?
            && let Event::Key(key) = event::read()?
            && exit_skips(key.code)
        {
            break;
        }
        let elapsed = started.elapsed();
        if elapsed >= duration {
            break;
        }
        let frame = elapsed.as_millis() as usize / frame_time.as_millis() as usize;
        terminal.draw(|area| draw_animation(area, exit_scene(frame)))?;
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    terminal.clear()?;
    Ok(())
}

pub async fn run(config: PathBuf) -> Result<()> {
    let mut app = App::new(registry::load(&config)?.repos);
    let mut terminal = ratatui::init();
    let result = async {
        startup_screen(&mut terminal).await?;
        event_loop(&mut terminal, &mut app, &config).await
    }
    .await;
    ratatui::restore();
    result
}

async fn event_loop(terminal: &mut DefaultTerminal, app: &mut App, config: &Path) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    let semaphore = Arc::new(Semaphore::new(4));
    let mut tasks = JoinSet::new();
    let mut last_local = Instant::now() - Duration::from_secs(5);
    loop {
        let local_due = last_local.elapsed() >= Duration::from_secs(5);
        if local_due {
            last_local = Instant::now();
        }
        for row in &mut app.rows {
            if local_due && !row.local_busy {
                row.local_busy = true;
                let repo = row.repo.clone();
                let tx = tx.clone();
                let semaphore = semaphore.clone();
                tasks.spawn(async move {
                    let _permit = semaphore.acquire().await.unwrap();
                    let observation = match git::snapshot(&repo).await {
                        Ok(v) => Observation::success(v),
                        Err(e) => Observation::failure(e),
                    };
                    let _ = tx.send(Message::Local(repo.id, Box::new(observation)));
                });
            }
            if Instant::now() >= row.next_remote && !row.remote_busy {
                row.remote_busy = true;
                let repo = row.repo.clone();
                let tx = tx.clone();
                let semaphore = semaphore.clone();
                tasks.spawn(async move {
                    let _permit = semaphore.acquire().await.unwrap();
                    let state = provider::adapter(&repo.identity.kind).snapshot(&repo).await;
                    let _ = tx.send(Message::Remote(repo.id, Box::new(state)));
                });
            }
        }
        while tasks.try_join_next().is_some() {}
        while let Ok(message) = rx.try_recv() {
            match message {
                Message::Local(id, state) => {
                    let before = app
                        .rows
                        .iter()
                        .find(|r| r.repo.id == id)
                        .map(RowState::attention);
                    app.update_rows(|rows| {
                        if let Some(row) = rows.iter_mut().find(|r| r.repo.id == id) {
                            row.local = (*state).retain_previous(&row.local);
                            row.local_busy = false;
                        }
                    });
                    notify_on_attention_entry(app, &mut tasks, &id, before);
                }
                Message::Remote(id, state) => {
                    let before = app
                        .rows
                        .iter()
                        .find(|r| r.repo.id == id)
                        .map(RowState::attention);
                    app.update_rows(|rows| {
                        if let Some(row) = rows.iter_mut().find(|r| r.repo.id == id) {
                            let failed = state.default_branch.error.is_some()
                                || state.prs.error.is_some()
                                || state.ci.error.is_some();
                            row.failures = if failed { (row.failures + 1).min(4) } else { 0 };
                            row.next_remote =
                                Instant::now() + Duration::from_secs(60 * 2_u64.pow(row.failures));
                            row.remote = state.retain_previous(&row.remote);
                            row.remote_busy = false;
                            save_cache(&row.repo, &row.remote);
                        }
                    });
                    notify_on_attention_entry(app, &mut tasks, &id, before);
                }
                Message::Fetched(id, result) => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.repo.id == id) {
                        if result.is_ok() {
                            row.fetched = Some(Instant::now());
                        }
                        row.next_remote = Instant::now();
                    }
                    app.record(match result {
                        Ok(()) => format!("Fetched {id}"),
                        Err(e) => format!("Fetch {id}: {e}"),
                    });
                    app.action_busy = false;
                    last_local = Instant::now() - Duration::from_secs(5);
                }
                Message::Preview(result) => {
                    app.action_busy = false;
                    match *result {
                        Ok(p) => app.mode = Mode::Confirm(Box::new(p)),
                        Err(e) => app.record(e),
                    }
                }
                Message::CheckoutPreview(result) => {
                    app.action_busy = false;
                    match *result {
                        Ok(p) => app.mode = Mode::ConfirmCheckout(Box::new(p)),
                        Err(e) => app.record(e),
                    }
                }
                Message::Action(result) => {
                    app.action_busy = false;
                    app.record(match result {
                        Ok(s) => s,
                        Err(e) => e.to_string(),
                    });
                    last_local = Instant::now() - Duration::from_secs(5);
                }
                Message::Added(result) => {
                    app.action_busy = false;
                    match result {
                        Ok(()) => {
                            let mut fresh = App::new(registry::load(config)?.repos);
                            app.update_rows(|rows| {
                                let old: HashMap<_, _> =
                                    rows.drain(..).map(|r| (r.repo.id.clone(), r)).collect();
                                for row in &mut fresh.rows {
                                    if let Some(previous) = old.get(&row.repo.id) {
                                        *row = previous.clone();
                                    }
                                }
                                *rows = fresh.rows;
                            });
                            app.record("Registry updated");
                        }
                        Err(e) => app.record(e),
                    }
                }
            }
        }
        app.selected = app.selected.min(app.visible().len().saturating_sub(1));
        terminal.draw(|f| draw(f, app))?;
        if event::poll(Duration::from_millis(50))?
            && let Event::Key(key) = event::read()?
        {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('c')
                && key.modifiers.contains(event::KeyModifiers::CONTROL)
            {
                exit_screen(terminal).await?;
                break;
            }
            match &mut app.mode {
                Mode::Add(input) => match key.code {
                    KeyCode::Esc => app.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) => input.push(c),
                    KeyCode::Enter => {
                        let input = input.clone();
                        let config = config.to_owned();
                        let tx = tx.clone();
                        app.mode = Mode::Normal;
                        app.action_busy = true;
                        tasks.spawn(async move {
                            let (path, remote) = input
                                .split_once('|')
                                .map(|(p, r)| (p.trim(), Some(r.trim())))
                                .unwrap_or((input.trim(), None));
                            let result = match registry::entry(Path::new(path), remote, None).await
                            {
                                Ok(repo) => registry::add(&config, repo),
                                Err(e) => Err(e),
                            };
                            let _ = tx.send(Message::Added(result));
                        });
                    }
                    _ => {}
                },
                Mode::Filter => match key.code {
                    KeyCode::Esc => {
                        app.filter.clear();
                        app.mode = Mode::Normal;
                    }
                    KeyCode::Enter => app.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        app.filter.pop();
                        app.selected = 0;
                    }
                    KeyCode::Char(c) => {
                        app.filter.push(c);
                        app.selected = 0;
                    }
                    _ => {}
                },
                Mode::Confirm(preview) => match key.code {
                    KeyCode::Char('y') => {
                        let preview = preview.clone();
                        let tx = tx.clone();
                        app.mode = Mode::Normal;
                        app.action_busy = true;
                        tasks.spawn(async move {
                            let _ = tx.send(Message::Action(git::apply(&preview).await));
                        });
                    }
                    KeyCode::Esc | KeyCode::Char('n') => app.mode = Mode::Normal,
                    _ => {}
                },
                Mode::Checkout(input) => match key.code {
                    KeyCode::Esc => app.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) if c.is_ascii_digit() => input.push(c),
                    KeyCode::Enter => {
                        let number = input.trim().parse::<u64>().ok();
                        match (number, app.current()) {
                            (Some(number), Some(i)) => {
                                let repo = app.rows[i].repo.clone();
                                let tx = tx.clone();
                                app.mode = Mode::Normal;
                                app.action_busy = true;
                                tasks.spawn(async move {
                                    let _ = tx.send(Message::CheckoutPreview(Box::new(
                                        git::checkout_preview(&repo, number).await,
                                    )));
                                });
                            }
                            _ => {
                                app.record("Enter a PR number to checkout");
                                app.mode = Mode::Normal;
                            }
                        }
                    }
                    _ => {}
                },
                Mode::ConfirmCheckout(preview) => match key.code {
                    KeyCode::Char('y') => {
                        let preview = preview.clone();
                        let tx = tx.clone();
                        app.mode = Mode::Normal;
                        app.action_busy = true;
                        tasks.spawn(async move {
                            let _ = tx.send(Message::Action(git::checkout_apply(&preview).await));
                        });
                    }
                    KeyCode::Esc | KeyCode::Char('n') => app.mode = Mode::Normal,
                    _ => {}
                },
                Mode::Remove(id) => match key.code {
                    KeyCode::Char('y') => {
                        let id = id.clone();
                        match registry::remove(config, &id) {
                            Ok(()) => app.rows.retain(|r| r.repo.id != id),
                            Err(e) => app.record(e),
                        }
                        app.mode = Mode::Normal;
                    }
                    KeyCode::Esc | KeyCode::Char('n') => app.mode = Mode::Normal,
                    _ => {}
                },
                Mode::Help => app.mode = Mode::Normal,
                Mode::Normal => match key.code {
                    KeyCode::Char('q') => {
                        exit_screen(terminal).await?;
                        break;
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.selected =
                            (app.selected + 1).min(app.visible().len().saturating_sub(1));
                        app.scroll = 0;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.selected = app.selected.saturating_sub(1);
                        app.scroll = 0;
                    }
                    KeyCode::Char('/') => app.mode = Mode::Filter,
                    KeyCode::Char('?') => app.mode = Mode::Help,
                    KeyCode::Char('a') if !app.action_busy => app.mode = Mode::Add(String::new()),
                    KeyCode::Char('d') if !app.action_busy => {
                        if let Some(i) = app.current() {
                            app.mode = Mode::Remove(app.rows[i].repo.id.clone());
                        }
                    }
                    KeyCode::Char('c') if !app.action_busy => {
                        if app.current().is_some() {
                            app.mode = Mode::Checkout(String::new());
                        }
                    }
                    KeyCode::Char(c @ '1'..='6') => {
                        app.tab = c as usize - '1' as usize;
                        app.scroll = 0;
                    }
                    KeyCode::Enter | KeyCode::Tab => {
                        app.tab = (app.tab + 1) % 6;
                        app.scroll = 0;
                    }
                    KeyCode::PageDown => app.scroll = app.scroll.saturating_add(8),
                    KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(8),
                    KeyCode::Esc => {
                        app.tab = 0;
                        app.scroll = 0;
                    }
                    KeyCode::Char('p') if !app.action_busy => {
                        if let Some(i) = app.current() {
                            let repo = app.rows[i].repo.clone();
                            let tx = tx.clone();
                            app.action_busy = true;
                            tasks.spawn(async move {
                                let _ =
                                    tx.send(Message::Preview(Box::new(git::preview(&repo).await)));
                            });
                        }
                    }
                    KeyCode::Char(c @ ('r' | 'R')) if !app.action_busy => {
                        let indices = if c == 'R' {
                            app.visible()
                        } else {
                            app.current().into_iter().collect()
                        };
                        app.action_busy = !indices.is_empty();
                        let repos: Vec<_> = indices
                            .into_iter()
                            .map(|i| app.rows[i].repo.clone())
                            .collect();
                        let tx = tx.clone();
                        tasks.spawn(async move {
                            for repo in repos {
                                let result = git::fetch(&repo).await;
                                let _ = tx.send(Message::Fetched(repo.id, result));
                            }
                        });
                    }
                    KeyCode::Char('o') => {
                        if let Some(i) = app.current()
                            && let Some(url) = provider::repo_url(&app.rows[i].repo)
                        {
                            let url = match app.tab {
                                2 => format!("{url}/pulls"),
                                3 => format!("{url}/issues"),
                                4 => format!("{url}/releases"),
                                _ => url,
                            };
                            let path = app.rows[i].repo.path.clone();
                            let tx = tx.clone();
                            tasks.spawn(async move {
                                let program = if cfg!(target_os = "macos") {
                                    "open"
                                } else {
                                    "xdg-open"
                                };
                                let result = crate::command::run(program, &[&url], &path)
                                    .await
                                    .map(|_| format!("Opened {url}"));
                                let _ = tx.send(Message::Action(result));
                            });
                        }
                    }
                    _ => {}
                },
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
