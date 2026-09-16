<div align="center">

# Phrourion

**A small watch post for your repositories.**
**The state of every checkout, before it becomes a surprise.**

[![Release](https://img.shields.io/github/v/release/AraneaDev/phrourion?label=release&include_prereleases)](https://github.com/AraneaDev/phrourion/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/AraneaDev/phrourion/ci.yml?label=CI)](https://github.com/AraneaDev/phrourion/actions/workflows/ci.yml)
[![Tests](https://img.shields.io/badge/tests-13%20passing-2b8a3e)](tests/)
[![License](https://img.shields.io/github/license/AraneaDev/phrourion?label=license&color=yellow)](./LICENSE)
[![Language](https://img.shields.io/github/languages/top/AraneaDev/phrourion)](https://github.com/AraneaDev/phrourion)
[![Last commit](https://img.shields.io/github/last-commit/AraneaDev/phrourion?label=last%20commit)](https://github.com/AraneaDev/phrourion/commits/main)
[![Conventional Commits](https://img.shields.io/badge/commits-conventional-fe5196?logo=conventionalcommits&logoColor=white)](https://www.conventionalcommits.org/)
[![Status](https://img.shields.io/badge/status-pre--release-orange)](#status)

</div>

---

> **Phrourion** (φρουριον) means watch post or small garrison. It is the place
> that keeps the road in sight. This tool keeps local repositories, their remote
> branches, and the work waiting around them in sight too.

**TL;DR:** Phrourion is a terminal dashboard for local repository state, remote
branches, pull requests, checks, and releases. It reads local Git directly and
uses provider adapters for remote data, starting with GitHub through the
authenticated `gh` CLI. A user-level registry lets you add checkouts without
writing configuration into them.

It is for the moment before you start work and the moment before you commit.
One screen shows whether a checkout is clean, whether its branch is behind, what
is open upstream, and whether a release is waiting for attention.

## Status

> **Status:** pre-release. GitHub monitoring and the safe fast-forward pull path
> are ready. GitLab, Bitbucket, and Forgejo adapters are planned behind the same
> provider boundary. Phrourion requires Rust 1.85 or newer and GitHub monitoring
> requires an authenticated [GitHub CLI](https://cli.github.com/).

## What it shows

For every registered repository, Phrourion reports:

- **local state**, including the current branch, clean or dirty working tree,
  staged changes, untracked files, conflicts, and Git operations in progress
- **remote state**, including the default branch, branch position, and provider
  reachability
- **open work**, including pull requests and release proposals detected from
  explicit branch, label, and title evidence
- **pending releases**, separating proposed, draft, and published release data
- **checks**, including the configured workflow runs and their current result

Unknown, unavailable, and unsupported data stay distinct from a real zero. A
failed remote query never turns into an empty list, and cached data remains
visible with its stale state while a refresh is retried.

## How it works

Local observations use Git commands in the registered checkout. Remote
observations use a provider adapter with a neutral result model, so the TUI does
not need to know whether data came from GitHub, GitLab, Bitbucket, or Forgejo.
The GitHub adapter calls `gh api` and validates paginated responses before using
them.

The registry lives outside the repositories at
`$XDG_CONFIG_HOME/phrourion/repos.toml`, or the platform configuration directory
when XDG is not available. It stores paths and remote identities, not tokens.

## Actions

The dashboard is read-only until you choose an action. It can refresh local and
remote state, open a repository in the browser, add or remove a checkout, and
pull the selected repository after confirmation.

A pull is allowed only when the checkout is still the registered one, the branch
is attached and unchanged, the working tree is clean, the upstream is configured,
and the update is fast-forward only. Phrourion never switches branches, stashes
changes, resets, rebases, or resolves conflicts.

```text
r       refresh the selected repository
R       refresh all repositories
p       preview and confirm a fast-forward pull
o       open the repository in a browser
a       add a repository
d       remove a repository
/       filter repositories
?       show help
q       quit
```

## Install

Build from source with Rust 1.85 or newer:

```bash
cargo install --path .
```

Authenticate the GitHub CLI if remote GitHub data is wanted:

```bash
gh auth login
```

## Use

Discover Git repositories below the current directory and add them to the
registry:

```bash
phro discover --add
```

Start the dashboard:

```bash
phro
```

`phro` is the short command name. The full `phrourion` binary is installed too.
Both support `add`, `remove`, `list`, and `status`. Run `phro --help` for all
options.

## Development

```bash
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

See [the design notes](docs/design.md) for the architecture, safety model, and
provider boundary.

## License

MIT.

---

Built by [Tim Schipper](https://tim-schipper.nl/en) and released as open source under
[Aranea Development](https://aranea-development.nl).
