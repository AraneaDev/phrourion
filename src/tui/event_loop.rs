use super::{
    animation::exit_screen,
    app::{App, AttentionPriority, Mode, RowState},
    cache::save_cache,
    draw::draw,
};
use crate::{
    git::{self, CheckoutPreview, LocalState, PullPreview},
    model::{Observation, RemoteState, clean},
    provider, registry,
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::DefaultTerminal;
use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Semaphore, mpsc},
    task::JoinSet,
};

pub(super) enum Message {
    Local(String, Box<Observation<LocalState>>),
    Remote(String, Box<RemoteState>),
    Fetched(String, Result<()>),
    Preview(Box<Result<PullPreview>>),
    CheckoutPreview(Box<Result<CheckoutPreview>>),
    Action(Result<String>),
    Added(Result<()>),
}

fn applescript_string_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

pub(super) fn notify_script(title: &str, body: &str) -> String {
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

// A repo moving into Problem or Pending from a calmer tier is worth a notification;
// staying within the tier (e.g. one unpushed commit becoming two) is not.
pub(super) fn entered_notice_tier(before: AttentionPriority, after: AttentionPriority) -> bool {
    after <= AttentionPriority::Pending && before > AttentionPriority::Pending
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

pub(super) fn is_press(key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
}

pub(super) fn is_quit_hotkey(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

// A remote fetch is "failed" for backoff purposes if any of the three
// always-attempted calls errored — a disabled feature (Observation
// {supported: false}) is not a failure, only an actual error is.
pub(super) fn remote_failed(state: &RemoteState) -> bool {
    state.default_branch.error.is_some() || state.prs.error.is_some() || state.ci.error.is_some()
}

// Consecutive-failure count, capped at 4 so the backoff below tops out at
// 16 minutes rather than growing unbounded.
pub(super) fn next_failure_count(current: u32, failed: bool) -> u32 {
    if failed { (current + 1).min(4) } else { 0 }
}

pub(super) fn remote_backoff(failures: u32) -> Duration {
    Duration::from_secs(60 * 2_u64.pow(failures))
}

pub(super) async fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    config: &Path,
) -> Result<()> {
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
                            row.failures = next_failure_count(row.failures, remote_failed(&state));
                            row.next_remote = Instant::now() + remote_backoff(row.failures);
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
            if !is_press(&key) {
                continue;
            }
            if is_quit_hotkey(&key) {
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
