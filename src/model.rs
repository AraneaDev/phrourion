use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderKind {
    Github,
    Gitlab,
    Bitbucket,
    BitbucketServer,
    Forgejo,
    Local,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Remote {
    pub kind: ProviderKind,
    pub host: String,
    pub project: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Repo {
    pub id: String,
    pub name: String,
    pub path: PathBuf,
    pub remote: String,
    pub identity: Remote,
    #[serde(default = "enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub release_workflows: Vec<String>,
    #[serde(default)]
    pub release_labels: Vec<String>,
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Item {
    pub title: String,
    pub url: String,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Observation<T> {
    pub data: Option<T>,
    pub observed: Option<u64>,
    pub error: Option<String>,
    pub supported: bool,
}

impl<T> Default for Observation<T> {
    fn default() -> Self {
        Self {
            data: None,
            observed: None,
            error: None,
            supported: true,
        }
    }
}

impl<T> Observation<T> {
    pub fn success(data: T) -> Self {
        Self {
            data: Some(data),
            observed: Some(now()),
            error: None,
            supported: true,
        }
    }
    pub fn failure(error: impl ToString) -> Self {
        Self {
            error: Some(error.to_string()),
            ..Self::default()
        }
    }
    pub fn unsupported() -> Self {
        Self {
            supported: false,
            ..Self::default()
        }
    }
    pub fn label(&self) -> String {
        if !self.supported {
            return "unsupported".into();
        }
        match (self.observed, &self.error) {
            (Some(t), Some(_)) => format!("stale {}s", now().saturating_sub(t)),
            (_, Some(_)) => "error".into(),
            (Some(t), None) => format!("{}s ago", now().saturating_sub(t)),
            _ => "loading".into(),
        }
    }
    pub fn retain_previous(mut self, previous: &Self) -> Self
    where
        T: Clone,
    {
        if self.error.is_some() && self.data.is_none() {
            self.data = previous.data.clone();
            self.observed = previous.observed;
        }
        self
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RemoteState {
    pub default_branch: Observation<String>,
    pub branches: Observation<Vec<Item>>,
    pub prs: Observation<Vec<Item>>,
    pub issues: Observation<Vec<Item>>,
    pub proposals: Observation<Vec<Item>>,
    pub drafts: Observation<Vec<Item>>,
    pub published: Observation<Vec<Item>>,
    pub ci: Observation<Vec<Item>>,
    pub publication: Observation<Vec<Item>>,
}

impl RemoteState {
    pub fn retain_previous(self, old: &Self) -> Self {
        Self {
            default_branch: self.default_branch.retain_previous(&old.default_branch),
            branches: self.branches.retain_previous(&old.branches),
            prs: self.prs.retain_previous(&old.prs),
            issues: self.issues.retain_previous(&old.issues),
            proposals: self.proposals.retain_previous(&old.proposals),
            drafts: self.drafts.retain_previous(&old.drafts),
            published: self.published.retain_previous(&old.published),
            ci: self.ci.retain_previous(&old.ci),
            publication: self.publication.retain_previous(&old.publication),
        }
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn clean(text: &str) -> String {
    text.chars()
        .filter(|c| {
            !c.is_control() && !matches!(*c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect()
}
