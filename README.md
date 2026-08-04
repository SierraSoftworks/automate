# Automate

Automate is a Rust-based automation server designed to automate
common manual tasks and use Todoist to request human involvement
when necessary.

It facilitates things like calendar sync, RSS syndication, and
the automatic management of GitHub notifications, as well as
keeping YNAB stock accounts up to date with live market prices;
it also handles webhooks from services like Tailscale, Honeycomb,
and [Grey](https://github.com/SierraSoftworks/grey) (raising a Todoist
task when a monitor stays unhealthy, then walking it through recovering
and recovered once the monitor comes back).

## Installation

Install with [Homebrew](https://brew.sh):

```sh
brew install sierrasoftworks/tap/automate
```

## Configuration

Automate is configured via a `config.toml` file. An example
configuration file can be found at `config.example.toml`. You can copy
this file to `config.toml` and modify it to suit your needs.

### Admin interface

The admin REST API (under `/api/v1`) is protected by an access-control
filter defined in `[web.admin]`. The `acl` expression is evaluated for
every request and must return `true` for access to be granted; it can
reference the request `method`, `path`, `client_ip`, and `headers.*`.
The admin area is **denied by default** — if you omit `acl` (or the
entire `[web.admin]` section) every request is rejected, so you must opt
in explicitly.

To require single sign-on, add a `[web.admin.oidc]` section pointing at
an OpenID Connect provider. When configured, the admin SPA runs the
Authorization Code request in a **popup**: it reads the provider's
authorization endpoint, client id, and scopes from
`GET /api/v1/auth/metadata`, opens the provider in a popup, and the popup
POSTs the returned `code` to `POST /api/v1/auth/token`. The agent performs
the confidential token exchange with its `client_secret` and returns the
ID token (and a refresh token, if issued) to the SPA. The browser never
holds the client secret. The SPA stores the ID token in `sessionStorage`
and sends it as an `Authorization: Bearer` header on every API request;
when the token expires (HTTP 401) it transparently renews it via
`POST /api/v1/auth/refresh` and retries once, so an active session is
restored without prompting. The token's `aud`, `iss`, `exp`, and `nbf`
claims are validated, and the remaining claims (e.g. `email`, `groups`)
are exposed to the `acl` filter under the `claims.` prefix, so you can
write rules such as `claims.email == "me@example.com"` or
`"admins" in claims.groups`. Register `{origin}/auth/callback` as the
provider's `redirect_uri`. Because the credential is a bearer header
rather than an automatically-attached cookie, there is no CSRF surface
and no CSRF token to manage; signing out simply discards the stored
token. Include `offline_access` (or your provider's equivalent) in
`scopes` so a refresh token is issued and sessions can renew silently.


If you run behind a reverse proxy and want absolute URLs to honour the
forwarded scheme/host, set `web.trust_proxy = true`; only do so when the
proxy is trusted, since these headers can otherwise be spoofed. The same flag
governs whether `X-Forwarded-For` is trusted when the admin `acl` evaluates
`client_ip`.

### Accounts and isolation

Every stored record belongs to an **account**, named by the OIDC username
claim (`preferred_username` by default; set `username_claim` under
`[web.auth.oidc]` to use a different one). Accounts are isolated at the
storage layer rather than by convention: the handle a job or request
handler holds is scoped to one account and has no method that names
another, so reaching across accounts is not something code can express.

Two account names are reserved for the installation itself and cannot be
claimed by a person: `!system`, which holds the user registry and the
caches that map an inbound webhook to its owner, and `!local`.

`[web.auth]` supersedes `[web.admin]` and splits the single old gate in
two:

- `user_acl` decides who may sign in at all.
- `admin_acl` decides who may administer the installation, which includes
  acting as another user.

Both are evaluated on every request against the same filter surface, so a
change takes effect immediately. Where either is omitted it falls back to
`[web.admin] acl`, which under the old model granted full access on its
own — so an existing configuration keeps behaving exactly as it did.

An administrator can act as another user by sending
`X-Impersonate-User: {username}`. Permission checks and audit entries
continue to name the administrator, so administrator status never follows
the impersonated account and any change made this way is recorded against
the account it affected.

### Encryption of stored credentials

API tokens, OAuth refresh tokens and webhook signing secrets are
encrypted with AES-256-GCM before being stored. Each value is bound to
the record holding it, so a ciphertext moved to another account's record
will not decrypt.

If you do not set `secret_key` under `[web.auth]`, a key is generated on
first run into `<database>.key` with owner-only permissions. **Back that
file up** — without it, stored credentials cannot be recovered. To manage
the key yourself, generate one with `openssl rand -base64 32` and set
`secret_key`. When rotating, move the old key into
`previous_secret_keys` and leave it there until every record that used it
has been rewritten.

### Upgrading an existing installation

Schema migrations run automatically when the agent starts. Existing
records are assigned to the `!local` account, which is what an
installation with no identity provider configured continues to run as, so
a single-user install keeps working with no configuration changes.

If you later adopt an identity provider, set `local_user` under
`[web.auth]` to the username you will sign in as **before** enabling it,
so your existing workflows are already filed under the right account.

### OAuth setup wizard

Some workflows act on third-party accounts (for example Spotify) that you link
by walking through an OAuth flow. The agent drives the confidential exchange
server-side and stores the resulting refresh token.

Every external service you connect — an OAuth2 provider, the GitHub App — goes
through the same setup wizard, so what follows applies to all of them. The agent
lists what is configured at `GET /api/v1/integrations`.

Admin-gated integrations (the default) are launched from the **Connect** dropdown
in the admin area's toolbar: the SPA calls the bearer-authenticated
`POST /api/v1/integrations/<id>/setup/start`, which returns a provider
authorization URL the SPA opens in a popup. The provider redirects back to the
agent's server-rendered callback, which records the connection. An integration
can instead opt into self-service access by setting its own `acl` — under
`[oauth2.<provider>]` or `[connections.github.app]` — evaluated just like the
admin ACL, so `acl = 'true'` lets anyone connect their own account without
signing in. A self-service integration can also be linked directly at
`/integrations/<id>/setup` as a top-level navigation (no admin bearer required);
an admin-gated one opened that way is directed to the admin area instead, except
when OIDC is disabled, in which case the admin ACL is evaluated on the request
directly. Each flow is bound to the browser that began it by a single-use
`state` value (held in a transient cookie scoped to the integration's callback
path) to prevent login CSRF.

The accounts connected to an integration are listed at
`GET /api/v1/integrations/<id>/connections` and shown in the admin area, where
each can be severed. Disconnecting is not undoable from Automate: for GitHub it
uninstalls the App from the account, and for an OAuth2 provider it discards the
stored credential.

OAuth2 callbacks keep their original `/oauth/<provider>/callback` path, since the
redirect URI is registered with each provider and cannot be moved without
reconfiguring the provider's application.

## Project layout

The project is a Cargo workspace split into three crates, mirroring the
[grey](https://github.com/SierraSoftworks/grey) project:

- `agent/` — the backend automation server (actix-web). It also serves
  the compiled UI as static assets (embedded at build time from
  `ui/dist`).
- `api/` — pure serde data-transfer types shared by the agent and the UI
  so the REST contract cannot drift between them.
- `ui/` — a [Yew](https://yew.rs) client-side single-page app, compiled
  to WebAssembly with [Trunk](https://trunkrs.dev). It talks to the agent
  exclusively over the `/api/v1` REST API.

## Web UI development

The UI is a pure client-side app, so it can be developed independently of
a running agent. Install the toolchain once:

```bash
rustup target add wasm32-unknown-unknown
cargo install trunk
```

Then, from the `ui/` directory, start the dev server with live reload:

```bash
trunk serve
```

`trunk serve` proxies `/api/v1`, `/integrations` and `/oauth` to a locally
running agent (see `ui/Trunk.toml`). To preview the interface **without** a backend,
append `?demo` to the URL — the app then renders baked-in sample data.

To produce the production bundle that the agent embeds:

```bash
trunk build --release
```

## Running

To run Automate, ensure you have Rust installed and then execute:

```bash
# Build the UI bundle first so the agent can embed it.
(cd ui && trunk build --release)

# Then build and run the agent.
cargo run --release -p automate
```
