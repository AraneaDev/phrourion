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

const DEFAULT_WORKSPACE: &str = "AraneaDev";

#[derive(Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub repos: Vec<Repo>,
    #[serde(default)]
    pub workspaces: Vec<String>,
    #[serde(default)]
    pub active_workspace: Option<String>,
}

pub fn config_path() -> Result<PathBuf> {
    Ok(dirs::config_dir()
        .context("Cannot find user configuration directory")?
        .join("phrourion/repos.toml"))
}

pub fn load(path: &Path) -> Result<Registry> {
    match fs::read_to_string(path) {
        Ok(text) => {
            let mut registry: Registry =
                toml::from_str(&text).context("Invalid repository registry")?;
            if !registry.repos.is_empty()
                && !text
                    .lines()
                    .any(|line| line.trim_start().starts_with("workspaces ="))
            {
                registry.workspaces.push(DEFAULT_WORKSPACE.into());
                for repo in &mut registry.repos {
                    repo.workspaces.push(DEFAULT_WORKSPACE.into());
                }
            }
            registry.active_workspace = registry.active_workspace.as_deref().and_then(|active| {
                registry
                    .workspaces
                    .iter()
                    .find(|workspace| workspace_eq(workspace, active))
                    .cloned()
            });
            Ok(registry)
        }
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
        workspaces: Vec::new(),
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

pub fn workspace_names(registry: &Registry) -> Vec<String> {
    let mut names = registry.workspaces.clone();
    names.sort_by_key(|name| name.to_lowercase());
    names.insert(0, "All".into());
    names
}

pub fn repos_in_workspace(registry: &Registry, workspace: Option<&str>) -> Vec<Repo> {
    match workspace.map(str::trim) {
        None => registry.repos.clone(),
        Some(workspace) if workspace_eq(workspace, "All") => registry.repos.clone(),
        Some(workspace) => registry
            .repos
            .iter()
            .filter(|repo| {
                repo.workspaces
                    .iter()
                    .any(|membership| workspace_eq(membership, workspace))
            })
            .cloned()
            .collect(),
    }
}

pub fn create_workspace(config: &Path, name: &str) -> Result<()> {
    let name = valid_workspace_name(name)?.to_string();
    update(config, |registry| {
        if registry
            .workspaces
            .iter()
            .any(|workspace| workspace_eq(workspace, &name))
        {
            bail!("Workspace already exists: {name}");
        }
        registry.workspaces.push(name);
        registry
            .workspaces
            .sort_by_key(|workspace| workspace.to_lowercase());
        Ok(())
    })
}

pub fn delete_workspace(config: &Path, name: &str) -> Result<()> {
    let name = valid_workspace_name(name)?;
    update(config, |registry| {
        let position = registry
            .workspaces
            .iter()
            .position(|workspace| workspace_eq(workspace, name))
            .with_context(|| format!("Unknown workspace: {name}"))?;
        let removed = registry.workspaces.remove(position);
        for repo in &mut registry.repos {
            repo.workspaces
                .retain(|workspace| !workspace_eq(workspace, &removed));
        }
        if registry
            .active_workspace
            .as_deref()
            .is_some_and(|active| workspace_eq(active, &removed))
        {
            registry.active_workspace = None;
        }
        Ok(())
    })
}

pub fn add_to_workspace(config: &Path, workspace: &str, repo: &str) -> Result<()> {
    update(config, |registry| {
        let workspace = find_workspace(registry, workspace)?.to_string();
        let repo = find_repo(registry, repo)?;
        if !registry.repos[repo]
            .workspaces
            .iter()
            .any(|membership| workspace_eq(membership, &workspace))
        {
            registry.repos[repo].workspaces.push(workspace);
            registry.repos[repo]
                .workspaces
                .sort_by_key(|membership| membership.to_lowercase());
        }
        Ok(())
    })
}

pub fn add_many_to_workspace(config: &Path, workspace: &str, repos: &[String]) -> Result<()> {
    update(config, |registry| {
        let workspace = find_workspace(registry, workspace)?.to_string();
        let indexes = repos
            .iter()
            .map(|repo| find_repo(registry, repo))
            .collect::<Result<Vec<_>>>()?;
        for index in indexes {
            if !registry.repos[index]
                .workspaces
                .iter()
                .any(|membership| workspace_eq(membership, &workspace))
            {
                registry.repos[index].workspaces.push(workspace.clone());
                registry.repos[index]
                    .workspaces
                    .sort_by_key(|membership| membership.to_lowercase());
            }
        }
        Ok(())
    })
}

pub fn remove_from_workspace(config: &Path, workspace: &str, repo: &str) -> Result<()> {
    update(config, |registry| {
        let workspace = find_workspace(registry, workspace)?.to_string();
        let repo = find_repo(registry, repo)?;
        registry.repos[repo]
            .workspaces
            .retain(|membership| !workspace_eq(membership, &workspace));
        Ok(())
    })
}

pub fn remove_many_from_workspace(config: &Path, workspace: &str, repos: &[String]) -> Result<()> {
    update(config, |registry| {
        let workspace = find_workspace(registry, workspace)?.to_string();
        let indexes = repos
            .iter()
            .map(|repo| find_repo(registry, repo))
            .collect::<Result<Vec<_>>>()?;
        for index in indexes {
            registry.repos[index]
                .workspaces
                .retain(|membership| !workspace_eq(membership, &workspace));
        }
        Ok(())
    })
}

pub fn set_active_workspace(config: &Path, workspace: Option<&str>) -> Result<()> {
    update(config, |registry| {
        registry.active_workspace = match workspace.map(str::trim) {
            None => None,
            Some(workspace) if workspace_eq(workspace, "All") => None,
            Some(workspace) => Some(find_workspace(registry, workspace)?.to_string()),
        };
        Ok(())
    })
}

fn valid_workspace_name(name: &str) -> Result<&str> {
    let name = name.trim();
    if name.is_empty() {
        bail!("Workspace name cannot be empty");
    }
    if workspace_eq(name, "All") {
        bail!("All is reserved for the implicit workspace view");
    }
    Ok(name)
}

fn find_workspace<'a>(registry: &'a Registry, name: &str) -> Result<&'a str> {
    let name = valid_workspace_name(name)?;
    registry
        .workspaces
        .iter()
        .find(|workspace| workspace_eq(workspace, name))
        .map(String::as_str)
        .with_context(|| format!("Unknown workspace: {name}"))
}

fn find_repo(registry: &Registry, selector: &str) -> Result<usize> {
    let ids: Vec<_> = registry
        .repos
        .iter()
        .enumerate()
        .filter(|(_, repo)| repo.id == selector)
        .map(|(index, _)| index)
        .collect();
    if ids.len() == 1 {
        return Ok(ids[0]);
    }
    if ids.len() > 1 {
        bail!("Expected one matching repository; use its full ID");
    }

    let names: Vec<_> = registry
        .repos
        .iter()
        .enumerate()
        .filter(|(_, repo)| repo.name == selector)
        .map(|(index, _)| index)
        .collect();
    match names.as_slice() {
        [index] => Ok(*index),
        [] => bail!("Unknown repository: {selector}"),
        _ => bail!("Expected one matching repository; use its full ID"),
    }
}

fn workspace_eq(left: &str, right: &str) -> bool {
    left.to_lowercase() == right.to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(id: &str, name: &str, path: PathBuf) -> Repo {
        Repo {
            id: id.into(),
            name: name.into(),
            path,
            remote: "origin".into(),
            identity: Remote {
                kind: ProviderKind::Local,
                host: String::new(),
                project: String::new(),
            },
            enabled: true,
            release_workflows: Vec::new(),
            release_labels: Vec::new(),
            issue_labels: Vec::new(),
            workspaces: Vec::new(),
        }
    }

    #[test]
    fn old_registry_toml_defaults_all_workspace_fields() {
        let registry: Registry = toml::from_str(
            r#"
                [[repos]]
                id = "/tmp/demo"
                name = "demo"
                path = "/tmp/demo"
                remote = "origin"

                [repos.identity]
                kind = "local"
                host = ""
                project = ""
            "#,
        )
        .unwrap();

        assert!(registry.workspaces.is_empty());
        assert_eq!(registry.active_workspace, None);
        assert!(registry.repos[0].workspaces.is_empty());
    }

    #[test]
    fn repo_workspace_memberships_round_trip_through_toml() {
        let registry = Registry {
            repos: vec![Repo {
                workspaces: vec!["AraneaDev".into()],
                ..repo("one", "demo", "/tmp/demo".into())
            }],
            workspaces: vec!["AraneaDev".into()],
            active_workspace: Some("AraneaDev".into()),
        };

        let text = toml::to_string_pretty(&registry).unwrap();
        assert!(text.contains("workspaces = [\"AraneaDev\"]"));
        let decoded: Registry = toml::from_str(&text).unwrap();
        assert_eq!(decoded.repos[0].workspaces, ["AraneaDev"]);
        assert_eq!(decoded.workspaces, ["AraneaDev"]);
        assert_eq!(decoded.active_workspace.as_deref(), Some("AraneaDev"));
    }

    #[test]
    fn create_workspace_trims_sorts_and_rejects_invalid_or_duplicate_names() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("repos.toml");

        create_workspace(&config, "  Zebra  ").unwrap();
        create_workspace(&config, "alpha").unwrap();

        assert_eq!(load(&config).unwrap().workspaces, ["alpha", "Zebra"]);
        assert!(create_workspace(&config, "ALPHA").is_err());
        assert!(create_workspace(&config, "  ").is_err());
        assert!(create_workspace(&config, "all").is_err());
        assert_eq!(load(&config).unwrap().workspaces, ["alpha", "Zebra"]);
    }

    #[test]
    fn deleting_a_workspace_removes_only_its_memberships_and_resets_active_selection() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = dir.path().join("checkout");
        fs::create_dir(&checkout).unwrap();
        fs::write(checkout.join("keep.txt"), "precious").unwrap();
        let config = dir.path().join("config/repos.toml");
        add(&config, repo("one", "demo", checkout.clone())).unwrap();
        create_workspace(&config, "Primary").unwrap();
        create_workspace(&config, "Secondary").unwrap();
        add_to_workspace(&config, "Primary", "one").unwrap();
        add_to_workspace(&config, "Secondary", "one").unwrap();
        set_active_workspace(&config, Some("PRIMARY")).unwrap();

        delete_workspace(&config, " primary ").unwrap();

        let registry = load(&config).unwrap();
        assert_eq!(registry.workspaces, ["Secondary"]);
        assert_eq!(registry.repos.len(), 1);
        assert_eq!(registry.repos[0].workspaces, ["Secondary"]);
        assert_eq!(registry.active_workspace, None);
        assert_eq!(
            fs::read_to_string(checkout.join("keep.txt")).unwrap(),
            "precious"
        );
        assert!(delete_workspace(&config, "missing").is_err());
        assert!(delete_workspace(&config, "All").is_err());
    }

    #[test]
    fn membership_changes_are_idempotent_and_allow_multiple_workspaces() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("repos.toml");
        add(&config, repo("one", "demo", dir.path().join("demo"))).unwrap();
        create_workspace(&config, "Beta").unwrap();
        create_workspace(&config, "Alpha").unwrap();

        add_to_workspace(&config, "beta", "one").unwrap();
        add_to_workspace(&config, "BETA", "one").unwrap();
        add_to_workspace(&config, "Alpha", "demo").unwrap();
        assert_eq!(
            load(&config).unwrap().repos[0].workspaces,
            ["Alpha", "Beta"]
        );

        remove_from_workspace(&config, "BETA", "one").unwrap();
        remove_from_workspace(&config, "Beta", "one").unwrap();
        assert_eq!(load(&config).unwrap().repos[0].workspaces, ["Alpha"]);
    }

    #[test]
    fn membership_changes_reject_unknown_workspaces_repositories_and_ambiguous_names() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("repos.toml");
        add(&config, repo("one", "same", dir.path().join("one"))).unwrap();
        add(&config, repo("two", "same", dir.path().join("two"))).unwrap();
        create_workspace(&config, "Team").unwrap();

        let unknown_workspace = add_to_workspace(&config, "missing", "one")
            .unwrap_err()
            .to_string();
        assert!(unknown_workspace.contains("Unknown workspace"));
        let unknown_repo = add_to_workspace(&config, "Team", "missing")
            .unwrap_err()
            .to_string();
        assert!(unknown_repo.contains("repository"));
        let ambiguous = add_to_workspace(&config, "Team", "same")
            .unwrap_err()
            .to_string();
        assert!(ambiguous.contains("full ID"));

        add_to_workspace(&config, "Team", "one").unwrap();
        assert_eq!(load(&config).unwrap().repos[0].workspaces, ["Team"]);
    }

    #[test]
    fn legacy_registries_migrate_existing_repositories_into_araneadev() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("repos.toml");
        let legacy = toml::to_string_pretty(&Registry {
            repos: vec![repo("one", "demo", dir.path().join("demo"))],
            ..Registry::default()
        })
        .unwrap();
        let legacy = legacy
            .lines()
            .filter(|line| !line.starts_with("workspaces") && !line.starts_with("active_workspace"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&config, legacy).unwrap();

        let registry = load(&config).unwrap();

        assert_eq!(registry.workspaces, ["AraneaDev"]);
        assert_eq!(registry.repos[0].workspaces, ["AraneaDev"]);
    }

    #[test]
    fn batch_membership_changes_validate_every_repository_before_mutating() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("repos.toml");
        add(&config, repo("one", "one", dir.path().join("one"))).unwrap();
        create_workspace(&config, "Team").unwrap();

        assert!(add_many_to_workspace(&config, "Team", &["one".into(), "missing".into()]).is_err());
        assert!(load(&config).unwrap().repos[0].workspaces.is_empty());

        assert!(
            remove_many_from_workspace(&config, "Team", &["one".into(), "missing".into()]).is_err()
        );
        assert!(load(&config).unwrap().repos[0].workspaces.is_empty());
    }

    #[test]
    fn workspace_names_and_filtering_include_the_implicit_all_view() {
        let registry = Registry {
            repos: vec![
                Repo {
                    workspaces: vec!["Alpha".into(), "Beta".into()],
                    ..repo("one", "one", "/tmp/one".into())
                },
                Repo {
                    workspaces: vec!["Beta".into()],
                    ..repo("two", "two", "/tmp/two".into())
                },
                repo("three", "three", "/tmp/three".into()),
            ],
            workspaces: vec!["Alpha".into(), "Beta".into()],
            active_workspace: None,
        };

        assert_eq!(workspace_names(&registry), ["All", "Alpha", "Beta"]);
        assert_eq!(
            repos_in_workspace(&registry, None)
                .into_iter()
                .map(|repo| repo.id)
                .collect::<Vec<_>>(),
            ["one", "two", "three"]
        );
        assert_eq!(
            repos_in_workspace(&registry, Some("bEtA"))
                .into_iter()
                .map(|repo| repo.id)
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(repos_in_workspace(&registry, Some("missing")).len(), 0);
    }

    #[test]
    fn active_workspace_persists_canonical_name_and_falls_back_for_invalid_saved_value() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("repos.toml");
        create_workspace(&config, "AraneaDev").unwrap();

        set_active_workspace(&config, Some("araneadev")).unwrap();
        assert_eq!(
            load(&config).unwrap().active_workspace.as_deref(),
            Some("AraneaDev")
        );
        set_active_workspace(&config, Some("All")).unwrap();
        assert_eq!(load(&config).unwrap().active_workspace, None);
        assert!(set_active_workspace(&config, Some("missing")).is_err());

        fs::write(
            &config,
            "workspaces = [\"AraneaDev\"]\nactive_workspace = \"Missing\"\n",
        )
        .unwrap();
        assert_eq!(load(&config).unwrap().active_workspace, None);
    }
}
