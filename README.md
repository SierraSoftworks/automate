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

Records are only divided between accounts once you ask for it, with
`multi_tenant = true` under `[web.auth]`. Until then everything belongs to
the installation's own account, whether or not people sign in — an
installation that has been running with an identity provider already has
its workflows and connections in one place, and reading the signed-in
identity as an account name would take all of them away from the people
using them without saying so. Signing in still decides who may do what;
it just does not decide where things are kept.

With it enabled, every stored record belongs to an **account**, named by
the OIDC username claim (`preferred_username` by default; set `username_claim` under
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

The admin UI drives this from its **Accounts** page (and from the *Act
as…* menu on the user chip): choosing somebody sets the header on every
subsequent request and puts a banner across the top of the shell naming
whose records are on screen. The choice lives in the tab's session, so
opening a new one lands back in your own account.

Alongside the people who have signed in, that list includes the
installation's own account (`!local` by default, or `local_user` where
one is configured). Nobody can sign into it, but it owns everything
configured before `multi_tenant` was switched on, so acting as it is the
only way back to those records. The `!system` tenant is deliberately not
offered: it holds the user registry and the webhook indexes, which are
the agent's own bookkeeping rather than anybody's records.

### Connections

Credentials for external services are held as **connections**: one linked
account at one service, owned by the user who linked it. A workflow names
the connection it publishes through, so two people — or one person with a
personal and a work Todoist account — never share a credential.

Manage them under `/api/v1/connections`. Services authorised through
OAuth are linked by the setup wizard, which obtains the credential
itself; services that issue a token you paste in are created directly.

Naming a connection on a workflow is optional. With one account linked to
a service it is used automatically; with several, a workflow that does
not say which is reported as ambiguous rather than guessed at, since
picking one would file your tasks in the wrong account.

If you had `[connections.todoist]` in your configuration, it is imported
once on start-up into the account a single-user installation runs as, so
your workflows keep publishing. Once that has happened you can delete the
section. The import never overwrites a connection you have since replaced.

### Todoist

Register a Todoist app at <https://app.todoist.com/app_console/>, point its
OAuth redirect URL at `https://your-host/integrations/todoist/setup/callback`,
and put its credentials in `[connections.todoist.app]`. Each person then
connects their own Todoist account from the connections page, and the agent
acts as them rather than through one shared token.

To receive events, set the app's webhook callback URL to
`https://your-host/webhooks/todoist` — Todoist requires HTTPS with no port —
and put the app's **Verification Token** in `webhook_secret`. That is what
Todoist signs deliveries with, and it is a different value from the client
secret, so deliveries are refused until it is set. The **Todoist** workflow
type can then react to items being completed, comments being added, projects
being archived, and so on, as they happen. Deliveries are routed by the Todoist
account they name, so each person's events only reach their own workflows.

Access tokens issued to a new Todoist app last an hour and are renewed on
demand, immediately before the token is used, rather than on a schedule.

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

#### Workflows move into the database

Workflows used to live in the `[workflows]` section of your
configuration file. On the first start after upgrading they are copied
into the database, keeping the schedule and settings each one had, and
the schedules the file had pushed are cleared out so nothing runs twice.

After that the `[workflows]` section is no longer read, so you can delete
it. Edit your workflows in the browser instead, or through the
import/export endpoints if you would rather keep them in a file under
version control. The move happens exactly once: a workflow you change
afterwards is not overwritten by the section it came from.

The GitHub notifications workflows are not moved. They are the
installation's own housekeeping rather than anybody's workflow, so they
stay in the configuration file where they are.

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
