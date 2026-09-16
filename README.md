<div align="center">

# Phrourion

**A small watch post for your repositories.**

[![CI](https://github.com/AraneaDev/phrourion/actions/workflows/ci.yml/badge.svg)](https://github.com/AraneaDev/phrourion/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

</div>

Phrourion (φρουριον) means watch post or small garrison. It is a personal
terminal dashboard for keeping an eye on local checkouts and their remotes.

## TL;DR

Phrourion shows local repository state beside remote branches, pull requests,
checks, and releases. It reads local Git directly and uses provider adapters for
remote data, starting with GitHub through the `gh` CLI. A small external
registry makes it easy to add more checkouts without changing any repository.

## Status

Pre-release. The GitHub workflow and safe fast-forward pull path are ready. GitLab,
Bitbucket, and Forgejo adapters are planned behind the same provider boundary.

## Install

Build from source with Rust 1.85 or newer:

```sh
cargo install --path .
```

GitHub monitoring uses an authenticated `gh` installation. Phrourion does not
store credentials.

## Use

Discover Git repositories below the current directory:

```sh
phrourion discover --add
```

Start the dashboard:

```sh
phrourion
```

The registry lives outside the repositories at
`$XDG_CONFIG_HOME/phrourion/repos.toml`, or the platform configuration
directory when XDG is not available. Use `phrourion --help` for the complete
command list.

Phrourion supports up to the first 13 repositories without special setup, and
you can add more at any time. Pull actions require confirmation and only allow
a clean, attached branch with a configured upstream and a fast-forward update.
Phrourion never switches branches, stashes changes, resets, rebases, or fixes
conflicts for you.

## Development

```sh
cargo fmt -- --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

See [the design notes](docs/design.md) for the architecture and provider
boundary.

## License

MIT. See [LICENSE](LICENSE).
