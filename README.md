# Automate

Automate is a Rust-based automation server designed to automate
common manual tasks and use Todoist to request human involvement
when necessary.

It facilitates things like calendar sync, RSS syndication, and
the automatic management of GitHub notifications; as well as
handling webhooks from services like Tailscale and Honeycomb.

## Configuration

Automate is configured via a `config.toml` file. An example
configuration file can be found at `config.example.toml`. You can copy
this file to `config.toml` and modify it to suit your needs.

### Admin interface

The admin endpoints (under `/admin`) are protected by an access-control
filter defined in `[web.admin]`. The `acl` expression is evaluated for
every request and must return `true` for access to be granted; it can
reference the request `method`, `path`, `client_ip`, and `headers.*`.
The admin area is **denied by default** — if you omit `acl` (or the
entire `[web.admin]` section) every request is rejected, so you must opt
in explicitly.

To require single sign-on, add a `[web.admin.oidc]` section pointing at
an OpenID Connect provider. When configured, requests must carry a valid
ID token (stored in an `HttpOnly` session cookie); unauthenticated users
are redirected to the provider to sign in. The token's `aud`, `iss`,
`exp`, and `nbf` claims are validated, and the remaining claims (e.g.
`email`, `groups`) are exposed to the `acl` filter under the `claims.`
prefix, so you can write rules such as
`claims.email == "me@example.com"` or `"admins" in claims.groups`.

State-changing admin actions are protected against CSRF with signed,
time-limited tokens embedded in each form. If you run behind a reverse
proxy and want absolute URLs (and the `Secure` cookie flag) to honour
the forwarded scheme/host, set `web.trust_proxy = true`; only do so when
the proxy is trusted, since these headers can otherwise be spoofed.

## Running

To run Automate, ensure you have Rust installed and then execute:

```bash
cargo run --release
```
