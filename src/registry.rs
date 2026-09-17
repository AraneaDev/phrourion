use crate::{
    git,
    model::{ProviderKind, Remote, Repo},
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub repos: Vec<Repo>,
}

pub fn config_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot find user configuration directory")?
        .join("phrourion/repos.toml"))
}

pub fn load(path: &Path) -> Result<Registry> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text).context("Invalid repository registry"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry::default()),
        Err(e) => Err(e.into()),
    }
}

#[allow(clippy::incompatible_msrv)]
pub fn update(path: &Path, change: impl FnOnce(&mut Registry) -> Result<()>) -> Result<()> {
    let parent = path
        .parent()
        .context("Config must have a parent directory")?;
    fs::create_dir_all(parent)?;
    {
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))?;
        lock.try_lock_exclusive()
            .context("Registry is being edited by another Phrourion process")?;
        let result = (|| -> Result<()> {
            let mut registry = load(path)?;
            change(&mut registry)?;
            let mut temp = tempfile::NamedTempFile::new_in(parent)?;
            temp.write_all(toml::to_string_pretty(&registry)?.as_bytes())?;
            temp.as_file().sync_all()?;
            temp.persist(path)?;
            Ok(())
        })();
        let _ = lock.unlock();
        result
    }
}

pub fn parse_remote(value: &str, override_kind: Option<ProviderKind>) -> Result<Remote> {
    if value.starts_with('/') || value.starts_with('.') || value.starts_with("file://") {
        return Ok(Remote {
            kind: ProviderKind::Local,
            host: String::new(),
            project: value.into(),
        });
    }
    let url = if value.contains("://") {
        url::Url::parse(value)?
    } else {
        let (host, path) = value
            .split_once(':')
            .context("Remote must be an HTTPS or SSH URL")?;
        url::Url::parse(&format!("ssh://{host}/{path}"))?
    };
    let host = url
        .host_str()
        .context("Remote URL has no host")?
        .to_lowercase();
    let project = url
        .path()
        .trim_matches('/')
        .trim_end_matches(".git")
        .to_string();
    if project.split('/').count() < 2
        || project
            .split('/')
            .any(|s| s.is_empty() || s == "." || s == "..")
    {
        bail!("Remote must identify an owner and repository");
    }
    let kind = match override_kind {
        Some(kind) => kind,
        None => match host.as_str() {
            "github.com" => ProviderKind::Github,
            "gitlab.com" => ProviderKind::Gitlab,
            "bitbucket.org" => ProviderKind::Bitbucket,
            "codeberg.org" => ProviderKind::Forgejo,
            _ => bail!("Unknown host {host}; specify --provider for a self-hosted service"),
        },
    };
    Ok(Remote {
        kind,
        host,
        project,
    })
}

pub async fn entry(path: &Path, remote: Option<&str>, kind: Option<ProviderKind>) -> Result<Repo> {
    let root = git::run(path, &["rev-parse", "--show-toplevel"]).await?;
    let path = fs::canonicalize(root)?;
    let names = git::run(&path, &["remote"]).await?;
    let names: Vec<_> = names.lines().collect();
    let remote = match remote {
        Some(name) if names.contains(&name) => name,
        Some(_) => bail!("Selected remote does not exist"),
        None if names.len() == 1 => names[0],
        None if names.is_empty() => "",
        _ => bail!("Choose a remote with --remote: {}", names.join(", ")),
    };
    let identity = if remote.is_empty() {
        Remote {
            kind: ProviderKind::Local,
            host: String::new(),
            project: String::new(),
        }
    } else {
        parse_remote(
            &git::run(&path, &["remote", "get-url", remote]).await?,
            kind,
        )?
    };
    let name = path
        .file_name()
        .context("Checkout has no directory name")?
        .to_string_lossy()
        .to_string();
    // The canonical path is a stable, collision-free identifier on this machine.
    let id = path.to_string_lossy().to_string();
    Ok(Repo {
        id,
        name,
        path,
        remote: remote.into(),
        identity,
        enabled: true,
        release_workflows: Vec::new(),
        release_labels: Vec::new(),
        issue_labels: Vec::new(),
    })
}

pub fn add(config: &Path, repo: Repo) -> Result<()> {
    update(config, |r| {
        if r.repos.iter().any(|x| x.path == repo.path) {
            bail!("Checkout is already registered");
        }
        r.repos.push(repo);
        r.repos.sort_by_key(|x| x.name.to_lowercase());
        Ok(())
    })
}

pub fn remove(config: &Path, key: &str) -> Result<()> {
    update(config, |r| {
        let matches: Vec<_> = r
            .repos
            .iter()
            .enumerate()
            .filter(|(_, x)| x.id == key || x.name == key)
            .map(|(i, _)| i)
            .collect();
        if matches.len() != 1 {
            bail!("Expected one matching repository; use its full ID");
        }
        r.repos.remove(matches[0]);
        Ok(())
    })
}
