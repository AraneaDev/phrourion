use crate::{
    git::{self, LocalState, PullPreview},
    model::{Observation, RemoteState, Repo, clean},
    provider, registry,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Constraint, Layout},
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

enum Message {
    Local(String, Box<Observation<LocalState>>),
    Remote(String, Box<RemoteState>),
    Fetched(String, Result<()>),
    Preview(Box<Result<PullPreview>>),
    Action(Result<String>),
    Added(Result<()>),
}

enum Mode {
    Normal,
    Add(String),
    Filter,
    Confirm(Box<PullPreview>),
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

fn cache_path(repo: &Repo) -> Option<PathBuf> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", repo.identity).hash(&mut hasher);
    repo.release_workflows.hash(&mut hasher);
    repo.release_labels.hash(&mut hasher);
    Some(
        dirs::cache_dir()?
            .join("phrourion")
            .join(format!("{:x}.json", hasher.finish())),
    )
}

fn cached(repo: &Repo) -> RemoteState {
    let state: Option<RemoteState> = cache_path(repo)
        .and_then(|p| std::fs::read(p).ok())
        .and_then(|s| serde_json::from_slice(&s).ok());
    if let Some(state) = state {
        let unavailable = RemoteState {
            default_branch: Observation::failure("Cached; awaiting refresh"),
            branches: Observation::failure("Cached; awaiting refresh"),
            prs: Observation::failure("Cached; awaiting refresh"),
            proposals: Observation::failure("Cached; awaiting refresh"),
            drafts: Observation::failure("Cached; awaiting refresh"),
            published: Observation::failure("Cached; awaiting refresh"),
            ci: Observation::failure("Cached; awaiting refresh"),
            publication: Observation::failure("Cached; awaiting refresh"),
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
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                format!("{} {}", r.repo.name, r.repo.path.display())
                    .to_lowercase()
                    .contains(&query)
            })
            .map(|(i, _)| i)
            .collect()
    }
    fn current(&self) -> Option<usize> {
        self.visible().get(self.selected).copied()
    }
    fn record(&mut self, s: impl ToString) {
        self.log.push(s.to_string());
        if self.log.len() > 100 {
            self.log.remove(0);
        }
    }
}

fn count<T>(value: &Observation<Vec<T>>) -> String {
    if !value.supported {
        "n/a".into()
    } else if value.error.is_some() {
        "unknown".into()
    } else {
        value
            .data
            .as_ref()
            .map(|v| v.len().to_string())
            .unwrap_or_else(|| "...".into())
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
        failures += usize::from(ci_label(&row.remote) == "fail");
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
    if row.local.error.is_some()
        || row.remote.default_branch.error.is_some()
        || row.remote.ci.error.is_some()
    {
        "!"
    } else if row.local_busy || row.remote_busy {
        spinner_frame(animation_frame())
    } else if row.local.data.as_ref().is_some_and(LocalState::dirty) {
        "◆"
    } else if row.local.data.as_ref().is_some_and(|s| s.behind > 0) {
        "↓"
    } else {
        "●"
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
        _ => Color::Green,
    }
}

fn ci_label(state: &RemoteState) -> String {
    let observation = &state.ci;
    if !observation.supported {
        return "n/a".into();
    }
    if observation.error.is_some() {
        return "unknown".into();
    }
    match &observation.data {
        None => "...".into(),
        Some(v) if v.is_empty() => "absent".into(),
        Some(v)
            if v.iter().any(|i| {
                [
                    "failure",
                    "timed_out",
                    "cancelled",
                    "action_required",
                    "error",
                ]
                .iter()
                .any(|s| i.detail.contains(s))
            }) =>
        {
            "fail".into()
        }
        Some(v)
            if v.iter().any(|i| {
                ["queued", "pending", "in_progress", "waiting", "requested"]
                    .iter()
                    .any(|s| i.detail.contains(s))
            }) =>
        {
            "running".into()
        }
        Some(v)
            if v.iter().all(|i| {
                ["success", "neutral", "skipped"]
                    .iter()
                    .any(|s| i.detail.contains(s))
            }) =>
        {
            "pass".into()
        }
        _ => "unknown".into(),
    }
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
            " Phrourion  |  {} repositories  |  filter: {}",
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
        .block(Block::bordered().title(" Triage "))
        .style(Style::default().fg(Color::Gray)),
        areas[1],
    );
    let visible = app.visible();
    let narrow = frame.area().width < 90;
    let rows = visible.iter().map(|&i| {
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
            None => ("?".into(), r.local.label(), "unknown".into()),
        };
        let local = if r.local.error.is_some() {
            r.local.label()
        } else {
            local
        };
        let tone = status_color(r);
        let mut cells = vec![
            Cell::from(format!("{} {}", health_glyph(r), clean(&r.repo.name)))
                .style(Style::default().fg(tone)),
            Cell::from(branch).style(Style::default().fg(Color::Blue)),
            Cell::from(local).style(Style::default().fg(tone)),
            Cell::from(sync).style(Style::default().fg(tone)),
        ];
        if !narrow {
            cells.extend([
                Cell::from(count(&r.remote.prs)).style(Style::default().fg(Color::Magenta)),
                Cell::from(format!(
                    "{} / {}",
                    count(&r.remote.proposals),
                    count(&r.remote.drafts)
                ))
                .style(Style::default().fg(Color::Yellow)),
                Cell::from(ci_label(&r.remote)).style(Style::default().fg(
                    if ci_label(&r.remote) == "fail" {
                        Color::Red
                    } else {
                        Color::Green
                    },
                )),
            ]);
        }
        Row::new(cells)
    });
    let mut headings = vec!["Repository", "Branch", "Local", "Sync*"];
    let mut widths = vec![
        Constraint::Percentage(22),
        Constraint::Percentage(17),
        Constraint::Percentage(20),
        Constraint::Percentage(19),
    ];
    if !narrow {
        headings.extend(["PRs", "Rel / Draft", "CI"]);
        widths.extend([
            Constraint::Length(4),
            Constraint::Length(12),
            Constraint::Length(8),
        ]);
    }
    let table = Table::new(rows, widths)
        .header(Row::new(headings).style(Style::default().fg(Color::Yellow)))
        .block(Block::bordered().title(" Checkouts "))
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
                details.push_str(&format!(
                    "Default branch: {} [{}]\n",
                    clean(
                        row.remote
                            .default_branch
                            .data
                            .as_deref()
                            .unwrap_or("unknown")
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
            2 => details.push_str(&items(
                "Open pull requests (open in browser for head checks)",
                &row.remote.prs,
            )),
            3 => {
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
    let title = ["1 State", "2 Branches", "3 PRs", "4 Releases", "5 Actions"][app.tab];
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
        Mode::Remove(name) => format!("Remove {} from registry only? y / n", clean(name)),
        Mode::Help => "j/k move | 1-5 tabs | PgUp/PgDn scroll | a add | d remove | / filter | r fetch/refresh | R all | p pull | o browser | q quit | Esc close".into(),
        Mode::Normal if app.action_busy => format!("{} action running... monitoring remains available", spinner_frame(animation_frame())),
        Mode::Normal => format!("a add  d remove  / filter  1-5 details  r refresh  p pull  o browser  ? help  q quit\n{}", app.log.last().map(|s| clean(s.lines().next().unwrap_or(""))).unwrap_or_default()),
    };
    frame.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::Cyan)),
        areas[4],
    );
}

pub async fn run(config: PathBuf) -> Result<()> {
    let mut app = App::new(registry::load(&config)?.repos);
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &config).await;
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
                    if let Some(row) = app.rows.iter_mut().find(|r| r.repo.id == id) {
                        row.local = (*state).retain_previous(&row.local);
                        row.local_busy = false;
                    }
                }
                Message::Remote(id, state) => {
                    if let Some(row) = app.rows.iter_mut().find(|r| r.repo.id == id) {
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
                            let old: HashMap<_, _> =
                                app.rows.drain(..).map(|r| (r.repo.id.clone(), r)).collect();
                            let mut fresh = App::new(registry::load(config)?.repos);
                            for row in &mut fresh.rows {
                                if let Some(previous) = old.get(&row.repo.id) {
                                    *row = previous.clone();
                                }
                            }
                            app.rows = fresh.rows;
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
                    KeyCode::Char('q') => break,
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
                    KeyCode::Char(c @ '1'..='5') => {
                        app.tab = c as usize - '1' as usize;
                        app.scroll = 0;
                    }
                    KeyCode::Enter | KeyCode::Tab => {
                        app.tab = (app.tab + 1) % 5;
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
                                3 => format!("{url}/releases"),
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
    fn empty_screen_renders_at_small_and_large_sizes() {
        for (width, height) in [(40, 12), (120, 40)] {
            let backend = ratatui::backend::TestBackend::new(width, height);
            let mut terminal = ratatui::Terminal::new(backend).unwrap();
            terminal.draw(|f| draw(f, &App::new(Vec::new()))).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = buffer.content.iter().map(|c| c.symbol()).collect();
            assert!(text.contains("Phrourion"));
        }
    }
    #[test]
    fn error_counts_never_look_like_zero() {
        let o: Observation<Vec<String>> = Observation::failure("denied");
        assert_eq!(count(&o), "unknown");
        assert_eq!(count(&Observation::<Vec<String>>::success(vec![])), "0");
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
}
