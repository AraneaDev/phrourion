# Provider authentication

Phrourion reads local Git directly and uses the registered remote to select a
provider adapter. Public repositories can be monitored without credentials
when the provider allows anonymous API access. Private repositories need a
token with read access to the data Phrourion displays.

## Choose a setup method

For an interactive desktop session:

1. Start `phro`.
2. Press `s` to open Provider accounts.
3. Enter the provider name, host, and token.
4. Press Tab to move between fields and Enter to save.

The token is masked and saved in the OS keyring. `repos.toml` contains only
provider/host metadata.

For a shell or CI environment, pipe the token to the CLI so it does not appear
in process arguments:

```bash
printf '%s\n' "$TOKEN" | phro auth set \
  --provider gitlab \
  --host gitlab.com \
  --token-stdin

phro auth list
phro auth test --provider gitlab --host gitlab.com
phro auth remove --provider gitlab --host gitlab.com
```

For ephemeral or headless use, environment variables are preferred:

```bash
export PHROURION_GITHUB_TOKEN='...'
export PHROURION_GITLAB_TOKEN='...'
export PHROURION_BITBUCKET_TOKEN='...'
export PHROURION_TOKEN_FORGE_EXAMPLE_COM='...'
```

Never put real tokens in shell history, repository files, screenshots, or
issue reports. Use your shell's secret-injection mechanism for CI.

## Credential precedence

Phrourion resolves credentials in this order:

1. provider-specific environment variable;
2. normalized-host environment variable;
3. OS-keyring credential;
4. authenticated `gh` fallback for GitHub;
5. anonymous access.

The provider-specific variables are:

| Provider | Default host | Provider-specific variable | API auth |
| --- | --- | --- | --- |
| GitHub | `github.com` | `PHROURION_GITHUB_TOKEN` | `Authorization: Bearer` |
| GitLab | `gitlab.com` | `PHROURION_GITLAB_TOKEN` | `Authorization: Bearer` |
| Bitbucket Cloud | `bitbucket.org` | `PHROURION_BITBUCKET_TOKEN` | `Authorization: Bearer` |
| Forgejo | `codeberg.org` or explicit host | none | `Authorization: token` |

The normalized-host form is `PHROURION_TOKEN_` followed by the uppercase host
with every non-alphanumeric character replaced by `_`. For example,
`forge.example.com:3000` becomes
`PHROURION_TOKEN_FORGE_EXAMPLE_COM_3000`.

## GitHub and GitHub Enterprise

GitHub does not require `gh`. Configure a token through the Accounts modal,
`phro auth set`, or `PHROURION_GITHUB_TOKEN`. Phrourion then uses the GitHub
REST API directly. If no direct token is configured, an authenticated `gh`
installation remains a fallback:

```bash
gh auth login
```

For a fine-grained token, grant only the repository read permissions needed by
your repositories. Phrourion reads repository metadata, branches, pull
requests, issues, releases, checks, workflow runs, and the authenticated user;
the exact permission needed can vary with repository visibility and provider
configuration. See the [GitHub permissions reference](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens)
and [personal-token guide](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens).

GitHub Enterprise hosts are registered with an explicit provider override:

```bash
phro add ~/src/project --provider github
printf '%s\n' "$GHE_TOKEN" | phro auth set \
  --provider github \
  --host github.example.com \
  --token-stdin
```

## GitLab

GitLab's default host is `gitlab.com`. A personal access token with `read_api`
is a narrow starting point for monitoring API data; use the token scopes
appropriate to the instance and project visibility. The
[GitLab token-scope reference](https://docs.gitlab.com/security/tokens/access_token_scopes/)
and [personal access token documentation](https://docs.gitlab.com/user/profile/personal_access_tokens/)
are authoritative.

For a self-hosted instance, register the checkout with an explicit provider
and set the host credential:

```bash
phro add ~/src/project --provider gitlab
printf '%s\n' "$TOKEN" | phro auth set \
  --provider gitlab \
  --host gitlab.example.com \
  --token-stdin
```

If the instance uses a non-default API base path, set
`PHROURION_GITLAB_BASE_URL` for the process running Phrourion.

## Forgejo

Forgejo uses a per-host credential. Codeberg is detected automatically;
self-hosted Forgejo requires `--provider forgejo` when the remote host is not
known:

```bash
phro add ~/src/project --provider forgejo
printf '%s\n' "$TOKEN" | phro auth set \
  --provider forgejo \
  --host forge.example.com \
  --token-stdin
```

Create an access token in the Forgejo user settings. Start with read access to
the repository and user scopes; add other read scopes only for data enabled on
your installation. Forgejo's API accepts the `Authorization: token ...`
header; its [API usage documentation](https://docs.gitea.com/development/api-usage)
describes token creation, permissions, and authentication compatibility.

Public repositories may work without a token. A self-hosted installation with
a private or internal CA is not automatically trusted by the bundled Rustls
root store; install a publicly trusted certificate or use the provider's
supported TLS setup.

## Bitbucket Cloud

Bitbucket Cloud uses `bitbucket.org` and a workspace/repository remote. Create
an API token with the minimum read permissions for the data you want to see.
Repository read access does not automatically include pull-request or pipeline
read access, so add those scopes when needed. Atlassian documents the available
[API token permissions](https://support.atlassian.com/bitbucket-cloud/docs/api-token-permissions/)
and [API-token usage](https://support.atlassian.com/bitbucket-cloud/docs/using-api-tokens/).

```bash
printf '%s\n' "$TOKEN" | phro auth set \
  --provider bitbucket \
  --host bitbucket.org \
  --token-stdin
```

## Bitbucket Server/Data Center

Bitbucket Server/Data Center is a separate provider kind because its API and
authentication differ from Bitbucket Cloud. Register it explicitly with
`--provider bitbucket-server` and use the self-hosted host in `phro auth set`.
Set `PHROURION_BITBUCKET_BASE_URL` when the server's API is not served from the
default API base used by the adapter. Confirm the token type and required read
permissions with your administrator's Bitbucket version documentation before
configuring it.

## Troubleshooting

- `No configured provider for host`: pass `--provider` when adding the remote
  or when using `phro auth test`/`remove`.
- `401` or `403`: test the token with `phro auth test`, then check repository
  access and provider-specific scopes.
- The environment variable wins unexpectedly: environment credentials always
  override keyring values. Unset the variable to test the saved keyring token.
- `gh` is unavailable: configure `PHROURION_GITHUB_TOKEN` or use the Accounts
  modal; direct GitHub API access does not require `gh`.
- Keyring errors on a server: use the documented environment variables instead
  of saving a credential in the unavailable desktop keyring.
- HTTP is rejected for a credentialed host: use HTTPS. HTTP is accepted only
  for loopback test fixtures.
