# Automate

Automate is a Rust-based automation server designed to automate
common manual tasks and use Todoist to request human involvement
when necessary.

It facilitates things like calendar sync, RSS syndication, and
the automatic management of GitHub notifications, as well as
keeping YNAB stock accounts up to date with live market prices;
it also handles webhooks from services like Tailscale, Honeycomb,
and [Grey](https://github.com/SierraSoftworks/grey) (raising a Todoist
task when a monitor becomes unhealthy and completing it once it recovers).

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
an OpenID Connect provider. When configured, the agent drives the entire
Authorization Code flow with PKCE server-side: `GET /api/v1/auth/login`
redirects the browser to the provider, the provider returns to
`GET /api/v1/auth/callback`, and the agent performs the confidential
token exchange and stores the resulting ID token in an `HttpOnly` session
cookie. The browser never handles tokens or the client secret. The
token's `aud`, `iss`, `exp`, and `nbf` claims are validated, and the
remaining claims (e.g. `email`, `groups`) are exposed to the `acl` filter
under the `claims.` prefix, so you can write rules such as
`claims.email == "me@example.com"` or `"admins" in claims.groups`. The
provider's `redirect_uri` must point at the agent's
`/api/v1/auth/callback` route.

Mutating requests (`POST`/`PUT`/`PATCH`/`DELETE`) additionally require a
double-submit CSRF token: the UI fetches one from `GET /api/v1/csrf`
(which sets a matching cookie) and echoes it back in the `X-CSRF-Token`
header. `POST /api/v1/auth/logout` clears the session cookie.


If you run behind a reverse proxy and want absolute URLs to honour the
forwarded scheme/host, set `web.trust_proxy = true`; only do so when the
proxy is trusted, since these headers can otherwise be spoofed. The same flag
governs whether `X-Forwarded-For` is trusted when the admin `acl` evaluates
`client_ip`.

### OAuth setup wizard

Some workflows act on third-party accounts (for example Spotify) that you link
by walking through an OAuth flow at `/oauth/<provider>/`. The agent drives the
flow server-side and stores the resulting refresh token. The wizard is protected
like the admin area: by default it requires you to be signed in as an admin, and
renders an HTML sign-in prompt (rather than a bare error) if you are not. A
provider can instead opt into self-service access by setting its own `acl` under
`[oauth2.<provider>]`, evaluated just like the admin ACL — for example
`acl = 'true'` lets anyone connect their own account without signing in. Each
flow is bound to the browser that began it by a single-use `state` value to
prevent login CSRF.

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

`trunk serve` proxies `/api/v1` and `/oauth` to a locally running agent
(see `ui/Trunk.toml`). To preview the interface **without** a backend,
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
