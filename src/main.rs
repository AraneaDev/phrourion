use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use phrourion::{git, model::ProviderKind, provider, registry, tui};
use std::path::PathBuf;

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
    },
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
                println!("{}\t{}\t{}", repo.name, repo.path.display(), repo.remote);
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
        Some(Action::Status { remote }) => {
            let mut states = Vec::new();
            for repo in registry::load(&config)?
                .repos
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
    }
    Ok(())
}
