# Phrourion

**TL;DR:** A terminal dashboard for your repositories. Phrourion compares local Git state with remote branches, pull requests, checks, and releases, then lets you act from the same screen.

Status: design approved; implementation has not started.

Built around Rust and Ratatui, with Git for local operations and the authenticated GitHub CLI for the first remote provider. GitLab, Bitbucket, and Forgejo are planned adapters.

Each registered repository has an explicit local folder. Pull updates that folder using fast-forward only, after checking its branch, remote, and working tree.

See [the design](docs/design.md) for behavior, layout, and implementation order.
