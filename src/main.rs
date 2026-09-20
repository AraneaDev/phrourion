use anyhow::{Result, bail};
use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use phrourion::{
    auth::{self, CredentialSource, CredentialStore, KeyringCredentialStore},
    git,
    model::ProviderKind,
    provider, registry, terminal, tui,
};
use std::{io::Read, path::PathBuf};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Override the external registry file.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Option<Action>,
}

#[derive(Clone, ValueEnum)]
enum Kind {
    Github,
    Gitlab,
    Bitbucket,
    BitbucketServer,
    Forgejo,
    Local,
}
impl From<Kind> for ProviderKind {
    fn from(v: Kind) -> Self {
        match v {
            Kind::Github => Self::Github,
            Kind::Gitlab => Self::Gitlab,
            Kind::Bitbucket => Self::Bitbucket,
            Kind::BitbucketServer => Self::BitbucketServer,
            Kind::Forgejo => Self::Forgejo,
            Kind::Local => Self::Local,
        }
    }
}

#[derive(Subcommand)]
enum Action {
    /// Register an existing checkout. Multiple remotes require --remote.
    Add {
        path: PathBuf,
        #[arg(long)]
        remote: Option<String>,
        #[arg(long, value_enum)]
        provider: Option<Kind>,
        #[arg(long)]
        release_workflow: Vec<String>,
    },
    /// Remove an entry, leaving the checkout untouched.
    Remove { name: String },
    /// Show the registered names and canonical folders.
    List,
    /// Find immediate child checkouts. Add them only with --add.
    Discover {
        path: PathBuf,
        #[arg(long)]
        add: bool,
    },
    /// Print a JSON snapshot for scripts. Does not fetch or pull.
    Status {
        #[arg(long)]
        remote: bool,
        #[arg(long)]
        workspace: Option<String>,
    },
    /// Manage named repository workspaces.
    Workspace {
        #[command(subcommand)]
        command: WorkspaceAction,
    },
    /// Open a registered repository in a new terminal.
    OpenTerminal { name: String },
    /// Manage provider credentials.
    Auth {
        #[command(subcommand)]
        command: AuthAction,
    },
}

#[derive(Subcommand)]
enum AuthAction {
    /// List configured provider hosts and effective credential sources.
    List,
    /// Save a token in the OS keyring. Read it from stdin without echoing.
    Set {
        #[arg(long, value_enum)]
        provider: Kind,
        #[arg(long)]
        host: String,
        #[arg(long, action = ArgAction::SetTrue)]
        token_stdin: bool,
        #[arg(long)]
        label: Option<String>,
    },
    /// Test a configured credential against its provider identity endpoint.
    Test {
        #[arg(long)]
        host: String,
        #[arg(long, value_enum)]
        provider: Option<Kind>,
    },
    /// Remove a saved credential and its non-secret metadata.
    Remove {
        #[arg(long)]
        host: String,
        #[arg(long, value_enum)]
        provider: Option<Kind>,
    },
}

#[derive(Subcommand)]
enum WorkspaceAction {
    /// Show all named workspaces.
    List,
    /// Create an empty workspace.
    Create { name: String },
    /// Delete a workspace and its memberships.
    Delete { name: String },
    /// Add one or more repositories to a workspace.
    Add { name: String, repos: Vec<String> },
    /// Remove one or more repositories from a workspace.
    Remove { name: String, repos: Vec<String> },
    /// Show repositories in a workspace.
    Repos { name: String },
}

fn selected_workspace<'a>(data: &'a registry::Registry, name: &'a str) -> Result<Option<&'a str>> {
    if name.eq_ignore_ascii_case("All") {
        return Ok(None);
    }
    data.workspaces
        .iter()
        .find(|workspace| workspace.eq_ignore_ascii_case(name))
        .map(String::as_str)
        .map(Some)
        .ok_or_else(|| anyhow::anyhow!("Unknown workspace: {name}"))
}

fn repo_workspace_labels(repo: &phrourion::model::Repo) -> String {
    if repo.workspaces.is_empty() {
        "-".into()
    } else {
        repo.workspaces.join(",")
    }
}

fn provider_name(provider: &ProviderKind) -> &'static str {
    match provider {
        ProviderKind::Github => "github",
        ProviderKind::Gitlab => "gitlab",
        ProviderKind::Bitbucket => "bitbucket",
        ProviderKind::BitbucketServer => "bitbucket-server",
        ProviderKind::Forgejo => "forgejo",
        ProviderKind::Local => "local",
    }
}

fn find_auth_provider(
    data: &registry::Registry,
    host: &str,
    provider: Option<Kind>,
) -> Result<ProviderKind> {
    if let Some(provider) = provider {
        return Ok(provider.into());
    }
    data.auth_accounts
        .iter()
        .find(|account| account.host.eq_ignore_ascii_case(host))
        .map(|account| account.provider.clone())
        .ok_or_else(|| anyhow::anyhow!("No configured provider for host {host}; pass --provider"))
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = cli.config.map(Ok).unwrap_or_else(registry::config_path)?;
    match cli.command {
        None => tui::run(config).await?,
        Some(Action::Add {
            path,
            remote,
            provider,
            release_workflow,
        }) => {
            let mut repo =
                registry::entry(&path, remote.as_deref(), provider.map(Into::into)).await?;
            repo.release_workflows = release_workflow;
            registry::add(&config, repo)?;
            println!("Registered {}", path.display());
        }
        Some(Action::Remove { name }) => {
            registry::remove(&config, &name)?;
            println!("Removed registry entry; checkout preserved");
        }
        Some(Action::List) => {
            for repo in registry::load(&config)?.repos {
                println!(
                    "{}\t{}\t{}\t{}",
                    repo.name,
                    repo.path.display(),
                    repo.remote,
                    repo_workspace_labels(&repo)
                );
            }
        }
        Some(Action::Discover { path, add }) => {
            let mut children: Vec<_> = std::fs::read_dir(path)?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.join(".git").exists())
                .collect();
            children.sort();
            let mut errors = Vec::new();
            for path in children {
                println!("{}", path.display());
                if add
                    && !registry::load(&config)?
                        .repos
                        .iter()
                        .any(|r| std::fs::canonicalize(&path).ok().as_ref() == Some(&r.path))
                {
                    match registry::entry(&path, None, None)
                        .await
                        .and_then(|r| registry::add(&config, r))
                    {
                        Ok(()) => {}
                        Err(e) => errors.push(format!("{}: {e}", path.display())),
                    }
                }
            }
            if !errors.is_empty() {
                bail!("{}", errors.join("\n"));
            }
        }
        Some(Action::Status { remote, workspace }) => {
            let data = registry::load(&config)?;
            let workspace = workspace
                .as_deref()
                .map(|name| selected_workspace(&data, name))
                .transpose()?
                .flatten();
            let mut states = Vec::new();
            for repo in registry::repos_in_workspace(&data, workspace)
                .into_iter()
                .filter(|r| r.enabled)
            {
                let local = match git::snapshot(&repo).await {
                    Ok(v) => serde_json::json!({"data":v}),
                    Err(e) => serde_json::json!({"error":e.to_string()}),
                };
                let remote_state = if remote {
                    Some(provider::adapter(&repo.identity.kind).snapshot(&repo).await)
                } else {
                    None
                };
                states.push(serde_json::json!({"repo":repo,"local":local,"remote":remote_state}));
            }
            println!("{}", serde_json::to_string_pretty(&states)?);
        }
        Some(Action::Workspace { command }) => match command {
            WorkspaceAction::List => {
                for workspace in registry::workspace_names(&registry::load(&config)?) {
                    println!("{workspace}");
                }
            }
            WorkspaceAction::Create { name } => {
                registry::create_workspace(&config, &name)?;
                println!("Created workspace {name}");
            }
            WorkspaceAction::Delete { name } => {
                registry::delete_workspace(&config, &name)?;
                println!("Deleted workspace {name}");
            }
            WorkspaceAction::Add { name, repos } => {
                if repos.is_empty() {
                    bail!("At least one repository is required");
                }
                registry::add_many_to_workspace(&config, &name, &repos)?;
                for repo in repos {
                    println!("Added {repo} to {name}");
                }
            }
            WorkspaceAction::Remove { name, repos } => {
                if repos.is_empty() {
                    bail!("At least one repository is required");
                }
                registry::remove_many_from_workspace(&config, &name, &repos)?;
                for repo in repos {
                    println!("Removed {repo} from {name}");
                }
            }
            WorkspaceAction::Repos { name } => {
                let data = registry::load(&config)?;
                let workspace = selected_workspace(&data, &name)?;
                for repo in registry::repos_in_workspace(&data, workspace) {
                    println!("{}\t{}", repo.name, repo.path.display());
                }
            }
        },
        Some(Action::Auth { command }) => match command {
            AuthAction::List => {
                let data = registry::load(&config)?;
                let store = KeyringCredentialStore;
                for account in data.auth_accounts {
                    let resolved = auth::resolve_credential(
                        &store,
                        account.provider.clone(),
                        &account.host,
                        None,
                    )?;
                    let source = match resolved.source() {
                        CredentialSource::Environment => "environment",
                        CredentialSource::Keyring => "keyring",
                        CredentialSource::Gh => "gh",
                        CredentialSource::Anonymous => "anonymous",
                    };
                    println!(
                        "{}\t{}\t{}\t{}",
                        provider_name(&account.provider),
                        account.host,
                        if account.label.is_empty() {
                            "-"
                        } else {
                            &account.label
                        },
                        source
                    );
                }
            }
            AuthAction::Set {
                provider,
                host,
                token_stdin,
                label,
            } => {
                if !token_stdin {
                    bail!("Pass --token-stdin so the token is not exposed in process arguments");
                }
                let provider: ProviderKind = provider.into();
                if provider == ProviderKind::Local {
                    bail!("Local repositories do not have provider credentials");
                }
                let mut token = String::new();
                std::io::stdin().read_to_string(&mut token)?;
                let token = token.trim().to_string();
                let store = KeyringCredentialStore;
                store.set(provider.clone(), &host, &token)?;
                registry::update(&config, |data| {
                    data.auth_accounts
                        .retain(|account| !account.host.eq_ignore_ascii_case(&host));
                    data.auth_accounts.push(auth::AuthAccount {
                        provider: provider.clone(),
                        host: host.to_ascii_lowercase(),
                        label: label.unwrap_or_default(),
                        username: String::new(),
                    });
                    data.auth_accounts
                        .sort_by_key(|account| account.host.clone());
                    Ok(())
                })?;
                println!("Saved {} credential for {}", provider_name(&provider), host);
            }
            AuthAction::Test { host, provider } => {
                let data = registry::load(&config)?;
                let provider = find_auth_provider(&data, &host, provider)?;
                let username = auth::test_connection(provider.clone(), &host).await?;
                println!(
                    "Authenticated as {username} on {} ({})",
                    host,
                    provider_name(&provider)
                );
            }
            AuthAction::Remove { host, provider } => {
                let data = registry::load(&config)?;
                let provider = find_auth_provider(&data, &host, provider)?;
                KeyringCredentialStore.remove(provider.clone(), &host)?;
                registry::update(&config, |data| {
                    data.auth_accounts.retain(|account| {
                        !(account.provider == provider && account.host.eq_ignore_ascii_case(&host))
                    });
                    Ok(())
                })?;
                println!(
                    "Removed {} credential for {}",
                    provider_name(&provider),
                    host
                );
            }
        },
        Some(Action::OpenTerminal { name }) => {
            let data = registry::load(&config)?;
            let matches: Vec<_> = data
                .repos
                .iter()
                .filter(|repo| repo.id == name || repo.name == name)
                .collect();
            let repo = match matches.as_slice() {
                [repo] => repo,
                [] => bail!("Unknown repository: {name}"),
                _ => bail!("Expected one matching repository; use its full ID"),
            };
            println!("{}", terminal::open(&repo.path).await?);
        }
    }
    Ok(())
}
