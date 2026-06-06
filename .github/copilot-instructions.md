# Copilot Instructions for Automate

## Project Overview

Automate is a Rust-based automation server designed to automate common manual tasks and use Todoist to request human involvement when necessary. The project facilitates:

- Calendar sync
- RSS syndication
- Automatic management of GitHub notifications
- Webhook handling from services like Tailscale and Honeycomb

**Core Technologies:**
- Rust (edition 2024)
- actix-web for HTTP server
- tokio for async runtime
- Todoist API for task management
- Various collectors, publishers, and workflow components

## Project Structure

The project is a Cargo workspace with three crates (mirroring the
[grey](https://github.com/SierraSoftworks/grey) project):

```
agent/           - Backend automation server (actix-web); embeds the built UI
  src/
  ├── collectors/     - Components that gather data from external sources
  ├── config.rs       - Configuration file parsing and structures
  ├── db/             - Database abstractions (SQLite is the primary implementation)
  ├── filter/         - Custom filter language with zero-copy semantics and recursive descent parser
  ├── job.rs          - Job management
  ├── parsers/        - Parsers for various data formats
  ├── publishers/     - Components that publish data to external services
  ├── services.rs     - Services abstraction for dependency injection and mocking
  ├── web/            - Web server, REST API (`/api/v1`), static UI serving, OAuth
  ├── webhooks/       - Webhook handlers (Tailscale, Honeycomb, etc.)
  └── workflows/      - Workflow definitions and scheduling
api/             - Pure serde DTOs shared by the agent and the UI (REST contract)
ui/              - Yew client-side SPA compiled to WebAssembly with Trunk
```

The `ui` crate is excluded from the default workspace (`exclude = ["ui"]`)
because it targets `wasm32-unknown-unknown`; build it with `trunk`.

## Building and Running

### Prerequisites
- Rust stable toolchain (currently using Rust 1.91.1)
- Cargo package manager
- For the UI: the `wasm32-unknown-unknown` target and [Trunk](https://trunkrs.dev)
  (`rustup target add wasm32-unknown-unknown` and `cargo install trunk`)

### Build Commands

**Development build (agent + api):**
```bash
cargo build
```

**Release build:**
```bash
cargo build --release -p automate
```

**Build the UI bundle (from `ui/`):**
```bash
cd ui && trunk build --release
```

**Run the UI dev server with live reload (from `ui/`):**
```bash
cd ui && trunk serve          # append ?demo to the URL for offline sample data
```

**Run the application (build the UI first so it can be embedded):**
```bash
(cd ui && trunk build --release)
cargo run --release -p automate
```

### Configuration
The application requires a `config.toml` file for configuration. See `config.example.toml` for reference.

## Testing

**Run all tests:**
```bash
cargo test
```

**Run tests without stopping on first failure:**
```bash
cargo test --no-fail-fast
```

**Run tests with coverage:**
```bash
RUSTFLAGS="-Cinstrument-coverage" cargo test --no-fail-fast
grcov . --binary-path target/debug/deps/ -s . -t lcov --ignore-not-existing --ignore '../**' --ignore '/*' --ignore 'C:/' -o ./lcov.info
```

## Linting and Code Quality

**Format code:**
```bash
cargo fmt --all
```

**Check formatting (CI check):**
```bash
cargo fmt --all --check
```

**Run Clippy linter:**
```bash
cargo clippy --all-targets --all-features
```

**Run Clippy with warnings as errors (CI check):**
```bash
cargo clippy --all-targets --all-features -- -D warnings
```

## Coding Standards and Conventions

### General Guidelines
- Follow standard Rust naming conventions (snake_case for functions/variables, PascalCase for types)
- Use Rust 2024 edition idioms
- Keep modules focused and single-purpose
- Use the prelude module (`use crate::prelude::*`) for common imports

### Error Handling
- Use the `human-errors` crate for error handling
- Return descriptive errors that help with debugging
- Webhook handlers should return `Ok(())` when rejecting requests to prevent retry loops

### Security Practices
- Use `html-escape` crate for HTML sanitization to prevent XSS vulnerabilities
- Use case-insensitive comparison with `eq_ignore_ascii_case()` for HTTP header lookups (per HTTP RFC 7230)
- Use `hmac` and `sha2` crates for HMAC-SHA256 signature verification in webhooks
- Never commit secrets or API keys to the repository

### Async/Await
- This project uses tokio for async runtime with multi-threaded executor
- Use `async-trait` for async trait implementations
- Prefer async/await over manual futures manipulation

### Dependencies
- Check the GitHub advisory database before adding new dependencies
- Prefer well-maintained crates with good security track records
- Use features flags to minimize dependency bloat (see Cargo.toml for examples)

### Testing
- Use `rstest` for parameterized tests
- Use `wiremock` for HTTP mocking in tests
- Tests should be in `#[cfg(test)]` modules within source files
- The `testing.rs` module provides test helpers and utilities
- Aim for meaningful test coverage of business logic

### Documentation
- Document public APIs with doc comments
- Keep README.md up-to-date with major changes
- Update config.example.toml when adding new configuration options
- Update `.github/copilot-instructions.md` when major changes are made

## Acceptance Criteria for Changes

All code changes must meet the following criteria:

1. **Code Quality:**
   - No new warnings from `cargo clippy`
   - Code must pass `cargo fmt --check`
   - No regressions in existing functionality

2. **Testing:**
   - New features must include appropriate tests
   - All tests must pass (`cargo test`)
   - Maintain or improve code coverage

3. **Security:**
   - No introduction of security vulnerabilities
   - Proper input validation and sanitization
   - Secrets management follows best practices

4. **Documentation:**
   - Public APIs must be documented
   - Configuration changes must be reflected in config.example.toml
   - Significant features should update README.md

5. **Performance:**
   - No unnecessary allocations or clones
   - Async operations should not block the executor
   - Resource cleanup should be handled properly

## Workflow and CI/CD

The project uses GitHub Actions for CI/CD:
- **Check job:** Runs formatting and Clippy checks
- **Test job:** Runs test suite with coverage reporting
- **Build job:** Builds for multiple platforms (Linux, Windows, macOS on x86_64 and ARM64)
- **Docker jobs:** Builds and publishes Docker images on release

## Common Tasks

### Adding a New Webhook Handler
1. Create a new module in `src/webhooks/`
2. Implement signature verification if required
3. Add configuration struct in `src/config.rs`
4. Register the webhook route in the web server
5. Add example configuration to `config.example.toml`
6. Add tests using wiremock for HTTP mocking

### Adding a New Workflow
1. Create workflow logic in `src/workflows/`
2. Implement the `CronJob` trait for scheduling
3. Add configuration in `src/config.rs`
4. Add example to `config.example.toml`
5. Add integration tests

### Adding a New Collector/Publisher
1. Create module in `src/collectors/` or `src/publishers/`
2. Implement the `Collector` trait for collectors (publishers use concrete types)
3. Update configuration structures
4. Add tests for the component
5. Document usage patterns

## Additional Notes

- Use `tracing_batteries` for tracing support (available via `use tracing_batteries::prelude::*`, or more simply through `use crate::prelude::*` which re-exports it)
- The web UI is a Yew client-side SPA (WebAssembly) that talks to the agent over the `/api/v1` REST API
- Admin authentication is server-driven OIDC: the agent performs the full Authorization Code + PKCE flow (`/api/v1/auth/login` → provider → `/api/v1/auth/callback`) and stores the ID token in an `HttpOnly` session cookie. Mutating API requests require a double-submit CSRF token (`GET /api/v1/csrf` sets a cookie; the UI echoes it in the `X-CSRF-Token` header). The browser never handles tokens.
- Database operations use `tokio-rusqlite` for multi-threaded SQLite access
- The `filter` module provides an interpreted language operating over `FilterValue`s for configurable filtering
