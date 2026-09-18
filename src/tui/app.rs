use super::cache::cached;
use crate::{
    git::{CheckoutPreview, LocalState, PullPreview},
    model::{Observation, RemoteState, Repo, clean},
    registry::Registry,
};
use std::time::Instant;

pub(super) const CI_FAIL_MARKERS: &[&str] = &[
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
pub(super) fn ci_failed_confirmed(state: &RemoteState) -> bool {
    let observation = &state.ci;
    observation.supported
        && observation.error.is_none()
        && observation.data.as_ref().is_some_and(|v| {
            v.iter()
                .any(|i| CI_FAIL_MARKERS.iter().any(|s| i.detail.contains(s)))
        })
}

pub(super) fn known_count<T>(value: &Observation<Vec<T>>) -> usize {
    if value.supported && value.error.is_none() {
        value.data.as_ref().map_or(0, Vec::len)
    } else {
        0
    }
}

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
pub(super) enum AttentionPriority {
    Problem,
    Pending,
    LocalWork,
    Quiet,
}

impl RowState {
    pub(super) fn attention(&self) -> (AttentionPriority, String) {
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

pub(super) enum Mode {
    Normal,
    Add(String),
    Filter,
    Confirm(Box<PullPreview>),
    Checkout(String),
    ConfirmCheckout(Box<CheckoutPreview>),
    Remove(String),
    Help,
    Workspace(String),
    CreateWorkspace(String),
    AddWorkspace(String),
    RemoveWorkspace(String),
}

pub struct App {
    pub rows: Vec<RowState>,
    pub selected: usize,
    pub filter: String,
    pub tab: usize,
    pub scroll: u16,
    pub log: Vec<String>,
    pub workspaces: Vec<String>,
    pub active_workspace: Option<String>,
    pub(super) mode: Mode,
    pub(super) action_busy: bool,
}

impl App {
    pub fn new(repos: Vec<Repo>) -> Self {
        Self::with_registry(Registry {
            repos,
            ..Registry::default()
        })
    }

    pub fn with_registry(data: Registry) -> Self {
        let active_workspace = data.active_workspace.filter(|active| {
            data.workspaces
                .iter()
                .any(|workspace| workspace.eq_ignore_ascii_case(active))
        });
        Self {
            rows: data
                .repos
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
            workspaces: data.workspaces,
            active_workspace,
            mode: Mode::Normal,
            action_busy: false,
        }
    }
    pub(super) fn visible(&self) -> Vec<usize> {
        let query = self.filter.to_lowercase();
        let mut visible: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                let in_workspace = self.active_workspace.as_deref().is_none_or(|workspace| {
                    r.repo
                        .workspaces
                        .iter()
                        .any(|membership| membership.eq_ignore_ascii_case(workspace))
                });
                in_workspace
                    && format!("{} {}", r.repo.name, r.repo.path.display())
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
    pub(super) fn current(&self) -> Option<usize> {
        self.visible().get(self.selected).copied()
    }
    pub(super) fn update_rows(&mut self, update: impl FnOnce(&mut Vec<RowState>)) {
        let selected_id = self.current().map(|i| self.rows[i].repo.id.clone());
        update(&mut self.rows);
        let visible = self.visible();
        self.selected = selected_id
            .and_then(|id| visible.iter().position(|&i| self.rows[i].repo.id == id))
            .unwrap_or_else(|| self.selected.min(visible.len().saturating_sub(1)));
    }
    pub(super) fn record(&mut self, s: impl ToString) {
        self.log.push(s.to_string());
        if self.log.len() > 100 {
            self.log.remove(0);
        }
    }
}
