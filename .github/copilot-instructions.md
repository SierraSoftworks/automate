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

```
src/
├── collectors/     - Components that gather data from external sources
├── config.rs       - Configuration file parsing and structures
├── db/             - Database operations
├── filter/         - Filtering logic for data processing
├── job.rs          - Job management
├── parsers/        - Parsers for various data formats
├── publishers/     - Components that publish data to external services
├── services.rs     - Service definitions and management
├── ui/             - User interface components (Yew SSR)
├── web/            - Web server and routing
├── webhooks/       - Webhook handlers (Tailscale, Honeycomb, etc.)
└── workflows/      - Workflow definitions and scheduling
```

## Building and Running

### Prerequisites
- Rust stable toolchain (currently using Rust 1.91.1)
- Cargo package manager

### Build Commands

**Development build:**
```bash
cargo build
```

**Release build:**
```bash
cargo build --release
```

**Run the application:**
```bash
cargo run --release
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
- Tests should be in `#[cfg(test)]` modules or the `testing.rs` file
- Aim for meaningful test coverage of business logic

### Documentation
- Document public APIs with doc comments
- Keep README.md up-to-date with major changes
- Update config.example.toml when adding new configuration options

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
2. Implement appropriate traits
3. Update configuration structures
4. Add tests for the component
5. Document usage patterns

## Additional Notes

- The project uses `inventory` crate for plugin-style registration
- Tracing is configured with OpenTelemetry, Sentry, and Medama integrations
- The web UI uses Yew with server-side rendering
- Database operations use both `rusqlite` and `fjall` (embedded key-value store)
