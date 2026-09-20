use super::{
    animation::exit_screen,
    app::{App, AttentionPriority, AuthField, Mode, PendingPreview, RowState},
    cache::save_cache,
    draw::draw,
};
use crate::{
    auth::{AuthAccount, CredentialStore, KeyringCredentialStore},
    git::{self, CheckoutPreview, LocalState, PullPreview},
    model::{Observation, ProviderKind, RemoteState, clean},
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TabDirection {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HelpAction {
    Close,
    ScrollDown,
    ScrollUp,
    PageDown,
    PageUp,
    Ignore,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum KeyContext {
    Dashboard,
    TextInput,
    Confirmation,
    Help,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Dispatch {
    OpenHelp,
    Help(HelpAction),
    ModeSpecific,
}

pub(super) fn next_tab(current: usize, direction: TabDirection) -> usize {
    match direction {
        TabDirection::Previous => (current + 5) % 6,
        TabDirection::Next => (current + 1) % 6,
    }
}

pub(super) fn tab_direction(key: KeyEvent) -> Option<TabDirection> {
    match key.code {
        KeyCode::Left | KeyCode::Char('h') | KeyCode::BackTab => Some(TabDirection::Previous),
        KeyCode::Right | KeyCode::Char('l') | KeyCode::Tab | KeyCode::Enter => {
            Some(TabDirection::Next)
        }
        _ => None,
    }
}

pub(super) fn opens_help(key: KeyEvent, is_text_input: bool) -> bool {
    key.code == KeyCode::F(1) || (!is_text_input && key.code == KeyCode::Char('?'))
}

pub(super) fn accepts_confirmation(key: KeyEvent) -> bool {
    matches!(key.code, KeyCode::Char('y') | KeyCode::Enter)
}

pub(super) fn help_action(key: KeyEvent) -> HelpAction {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::F(1) | KeyCode::Char('q') => HelpAction::Close,
        KeyCode::Char('j') | KeyCode::Down => HelpAction::ScrollDown,
        KeyCode::Char('k') | KeyCode::Up => HelpAction::ScrollUp,
        KeyCode::PageDown => HelpAction::PageDown,
        KeyCode::PageUp => HelpAction::PageUp,
        _ => HelpAction::Ignore,
    }
}

fn key_context(mode: &Mode) -> KeyContext {
    match mode {
        Mode::Normal => KeyContext::Dashboard,
        Mode::Add(_)
        | Mode::Filter
        | Mode::Checkout(_)
        | Mode::Workspace(_)
        | Mode::CreateWorkspace(_)
        | Mode::AddWorkspace(_)
        | Mode::RemoveWorkspace(_)
        | Mode::Accounts(_) => KeyContext::TextInput,
        Mode::Confirm(_) | Mode::ConfirmCheckout(_) | Mode::Remove(_) => KeyContext::Confirmation,
        Mode::Help { .. } => KeyContext::Help,
    }
}

pub(super) fn dispatch_for(key: KeyEvent, context: KeyContext) -> Dispatch {
    match context {
        KeyContext::Help => Dispatch::Help(help_action(key)),
        KeyContext::Dashboard if opens_help(key, false) => Dispatch::OpenHelp,
        KeyContext::TextInput if opens_help(key, true) => Dispatch::OpenHelp,
        KeyContext::Confirmation if key.code == KeyCode::F(1) => Dispatch::OpenHelp,
        _ => Dispatch::ModeSpecific,
    }
}

fn apply_help_action(app: &mut App, action: HelpAction) {
    if action == HelpAction::Close {
        app.close_help();
        return;
    }
    let Mode::Help { scroll, .. } = &mut app.mode else {
        return;
    };
    match action {
        HelpAction::ScrollDown => *scroll = scroll.saturating_add(1),
        HelpAction::ScrollUp => *scroll = scroll.saturating_sub(1),
        HelpAction::PageDown => *scroll = scroll.saturating_add(8),
        HelpAction::PageUp => *scroll = scroll.saturating_sub(8),
        HelpAction::Close | HelpAction::Ignore => {}
    }
}

pub(super) fn apply_preview_message(app: &mut App, message: Message) {
    app.action_busy = false;
    match message {
        Message::Preview(result) => match *result {
            Ok(preview) => present_preview(app, PendingPreview::Pull(preview)),
            Err(error) => app.record(error),
        },
        Message::CheckoutPreview(result) => match *result {
            Ok(preview) => present_preview(app, PendingPreview::Checkout(preview)),
            Err(error) => app.record(error),
        },
        _ => unreachable!("only preview messages belong here"),
    }
}

fn present_preview(app: &mut App, preview: PendingPreview) {
    if let Mode::Help { pending, .. } = &mut app.mode {
        **pending = Some(preview);
        return;
    }
    app.mode = match preview {
        PendingPreview::Pull(preview) => Mode::Confirm(Box::new(preview)),
        PendingPreview::Checkout(preview) => Mode::ConfirmCheckout(Box::new(preview)),
    };
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

fn reload_registry(app: &mut App, data: registry::Registry) {
    let fresh = App::with_registry(data);
    app.workspaces = fresh.workspaces;
    app.active_workspace = fresh.active_workspace;
    app.auth_accounts = fresh.auth_accounts;
    let new_rows = fresh.rows;
    app.update_rows(|rows| {
        let old: HashMap<_, _> = rows.drain(..).map(|r| (r.repo.id.clone(), r)).collect();
        *rows = new_rows
            .into_iter()
            .map(|row| {
                if let Some(previous) = old.get(&row.repo.id) {
                    let mut restored = previous.clone();
                    restored.repo = row.repo;
                    return restored;
                }
                row
            })
            .collect();
    });
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
                    apply_preview_message(app, Message::Preview(result));
                }
                Message::CheckoutPreview(result) => {
                    apply_preview_message(app, Message::CheckoutPreview(result));
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
                            reload_registry(app, registry::load(config)?);
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
            match dispatch_for(key, key_context(&app.mode)) {
                Dispatch::OpenHelp => {
                    app.open_help();
                    continue;
                }
                Dispatch::Help(action) => {
                    apply_help_action(app, action);
                    continue;
                }
                Dispatch::ModeSpecific => {}
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
                Mode::Workspace(input) => match key.code {
                    KeyCode::Esc => app.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) => input.push(c),
                    KeyCode::Enter => {
                        let name = input.trim().to_string();
                        let result = if name.eq_ignore_ascii_case("All") || name.is_empty() {
                            registry::set_active_workspace(config, None)
                        } else if app
                            .workspaces
                            .iter()
                            .any(|workspace| workspace.eq_ignore_ascii_case(&name))
                        {
                            registry::set_active_workspace(config, Some(&name))
                        } else {
                            anyhow::bail!("Unknown workspace: {name}")
                        };
                        match result {
                            Ok(()) => {
                                app.active_workspace =
                                    if name.is_empty() || name.eq_ignore_ascii_case("All") {
                                        None
                                    } else {
                                        Some(name)
                                    };
                                app.selected = 0;
                            }
                            Err(e) => app.record(e),
                        }
                        app.mode = Mode::Normal;
                    }
                    _ => {}
                },
                Mode::CreateWorkspace(input) => match key.code {
                    KeyCode::Esc => app.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) => input.push(c),
                    KeyCode::Enter => {
                        let name = input.trim().to_string();
                        match registry::create_workspace(config, &name)
                            .and_then(|_| registry::set_active_workspace(config, Some(&name)))
                            .and_then(|_| registry::load(config))
                        {
                            Ok(data) => {
                                reload_registry(app, data);
                                app.record(format!("Created workspace {name}"));
                            }
                            Err(e) => app.record(e),
                        }
                        app.mode = Mode::Normal;
                    }
                    _ => {}
                },
                Mode::AddWorkspace(input) | Mode::RemoveWorkspace(input) => match key.code {
                    KeyCode::Esc => app.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    KeyCode::Char(c) => input.push(c),
                    KeyCode::Enter => {
                        let workspace = if input.trim().is_empty() {
                            app.active_workspace.clone()
                        } else {
                            Some(input.trim().to_string())
                        };
                        let result = match (workspace, app.current()) {
                            (Some(workspace), Some(index)) => {
                                let repo = app.rows[index].repo.id.clone();
                                if matches!(&app.mode, Mode::AddWorkspace(_)) {
                                    registry::add_to_workspace(config, &workspace, &repo)
                                } else {
                                    registry::remove_from_workspace(config, &workspace, &repo)
                                }
                            }
                            (None, _) => anyhow::bail!("Select a workspace first"),
                            (_, None) => anyhow::bail!("No repository selected"),
                        };
                        match result.and_then(|_| registry::load(config)) {
                            Ok(data) => reload_registry(app, data),
                            Err(e) => app.record(e),
                        }
                        app.mode = Mode::Normal;
                    }
                    _ => {}
                },
                Mode::Accounts(form) => match key.code {
                    KeyCode::Esc => app.mode = Mode::Normal,
                    KeyCode::Tab | KeyCode::Right => {
                        form.field = match form.field {
                            AuthField::Provider => AuthField::Host,
                            AuthField::Host => AuthField::Token,
                            AuthField::Token => AuthField::Provider,
                        }
                    }
                    KeyCode::BackTab | KeyCode::Left => {
                        form.field = match form.field {
                            AuthField::Provider => AuthField::Token,
                            AuthField::Host => AuthField::Provider,
                            AuthField::Token => AuthField::Host,
                        }
                    }
                    KeyCode::Backspace => {
                        form.input_mut().pop();
                    }
                    KeyCode::Char(character) => form.input_mut().push(character),
                    KeyCode::Enter if form.field != AuthField::Token => {
                        form.field = match form.field {
                            AuthField::Provider => AuthField::Host,
                            AuthField::Host => AuthField::Token,
                            AuthField::Token => unreachable!(),
                        }
                    }
                    KeyCode::Enter => {
                        let provider = match form.provider.trim() {
                            "github" => Some(ProviderKind::Github),
                            "gitlab" => Some(ProviderKind::Gitlab),
                            "bitbucket" => Some(ProviderKind::Bitbucket),
                            "bitbucket-server" => Some(ProviderKind::BitbucketServer),
                            "forgejo" => Some(ProviderKind::Forgejo),
                            _ => None,
                        };
                        if let Some(provider) = provider {
                            if form.host.trim().is_empty() || form.token.trim().is_empty() {
                                app.record("Provider, host, and token are required");
                                continue;
                            }
                            let host = form.host.trim().to_ascii_lowercase();
                            let result = (|| -> Result<()> {
                                KeyringCredentialStore.set(
                                    provider.clone(),
                                    &host,
                                    form.token.trim(),
                                )?;
                                registry::update(config, |data| {
                                    data.auth_accounts.retain(|account| {
                                        !account.host.eq_ignore_ascii_case(&host)
                                    });
                                    data.auth_accounts.push(AuthAccount {
                                        provider,
                                        host,
                                        label: String::new(),
                                        username: String::new(),
                                    });
                                    Ok(())
                                })?;
                                Ok(())
                            })();
                            match result {
                                Ok(()) => {
                                    reload_registry(app, registry::load(config)?);
                                    app.mode = Mode::Normal;
                                    app.record("Provider credential saved");
                                }
                                Err(error) => app.record(error),
                            }
                        } else {
                            app.record("Provider must be github, gitlab, bitbucket, bitbucket-server, or forgejo");
                        }
                    }
                    _ => {}
                },
                Mode::Confirm(preview) => match key.code {
                    _ if accepts_confirmation(key) => {
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
                    _ if accepts_confirmation(key) => {
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
                    _ if accepts_confirmation(key) => {
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
                Mode::Help { .. } => {}
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
                    KeyCode::Char('w') => app.mode = Mode::Workspace(String::new()),
                    KeyCode::Char('n') if !app.action_busy => {
                        app.mode = Mode::CreateWorkspace(String::new())
                    }
                    KeyCode::Char('m') if !app.action_busy => {
                        app.mode = Mode::AddWorkspace(String::new())
                    }
                    KeyCode::Char('u') if !app.action_busy => {
                        app.mode = Mode::RemoveWorkspace(String::new())
                    }
                    KeyCode::Char('a') if !app.action_busy => app.mode = Mode::Add(String::new()),
                    KeyCode::Char('s') if !app.action_busy => app.open_accounts(),
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
                    _ if tab_direction(key).is_some() => {
                        app.tab = next_tab(app.tab, tab_direction(key).unwrap());
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
                    KeyCode::Char('t') if !app.action_busy => {
                        if let Some(i) = app.current() {
                            let path = app.rows[i].repo.path.clone();
                            let tx = tx.clone();
                            app.action_busy = true;
                            tasks.spawn(async move {
                                let _ =
                                    tx.send(Message::Action(crate::terminal::open(&path).await));
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
