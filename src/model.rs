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
    #[serde(default)]
    pub issue_labels: Vec<String>,
    #[serde(default)]
    pub workspaces: Vec<String>,
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
    pub review_requests: Observation<Vec<Item>>,
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
            review_requests: self.review_requests.retain_previous(&old.review_requests),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_enabled_defaults_to_true() {
        assert!(enabled());
    }

    #[test]
    fn label_distinguishes_unsupported_stale_error_fresh_and_loading() {
        let unsupported: Observation<()> = Observation::unsupported();
        assert_eq!(unsupported.label(), "unsupported");

        // Backfilled by retain_previous: a real prior fetch (observed set)
        // whose current attempt errored (error set too).
        let stale = Observation::<()> {
            data: None,
            observed: Some(now()),
            error: Some("boom".into()),
            supported: true,
        };
        assert!(stale.label().starts_with("stale "));
        assert!(stale.label().ends_with('s'));

        // Never had a successful fetch, only ever errored.
        let error_only = Observation::<()> {
            data: None,
            observed: None,
            error: Some("boom".into()),
            supported: true,
        };
        assert_eq!(error_only.label(), "error");

        let fresh: Observation<()> = Observation::success(());
        assert!(fresh.label().ends_with("s ago"));

        let loading: Observation<()> = Observation::default();
        assert_eq!(loading.label(), "loading");
    }

    #[test]
    fn retain_previous_backfills_only_when_errored_with_no_data_of_its_own() {
        let previous = Observation::success(vec!["old".to_string()]);

        // Errored AND no data of its own: backfill from the previous value.
        let errored_no_data = Observation::<Vec<String>>::failure("boom");
        let backfilled = errored_no_data.retain_previous(&previous);
        assert_eq!(backfilled.data.as_deref(), Some(&["old".to_string()][..]));
        assert_eq!(backfilled.error.as_deref(), Some("boom"));

        // Errored but already has its own data: must not be overwritten.
        let mut errored_with_data = Observation::<Vec<String>>::failure("boom");
        errored_with_data.data = Some(vec!["own".to_string()]);
        let kept = errored_with_data.retain_previous(&previous);
        assert_eq!(kept.data.as_deref(), Some(&["own".to_string()][..]));

        // No error at all: must not touch data, regardless of `previous`.
        let clean: Observation<Vec<String>> = Observation::default();
        let untouched = clean.retain_previous(&previous);
        assert!(untouched.data.is_none());
    }

    #[test]
    fn now_returns_a_real_unix_timestamp() {
        // Any date after 2001 is > 1e9 seconds since the epoch, so this
        // distinguishes the real clock from a stubbed 0 or 1.
        assert!(now() > 1_000_000_000);
    }

    #[test]
    fn clean_strips_control_characters_and_bidi_overrides_but_keeps_normal_text() {
        assert_eq!(clean("hello world"), "hello world");
        assert_eq!(clean("bad\u{7}bell"), "badbell");
        assert_eq!(clean("evil\u{202e}gnp.exe"), "evilgnp.exe");
        assert_eq!(clean("emoji-safe-🎉"), "emoji-safe-🎉");
    }
}
