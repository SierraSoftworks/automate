# End-to-end tests

Playwright tests that drive the Yew admin UI in a real browser, against a real
agent, with a real database behind it. They cover the things that only show up
when those three are put together: that the UI was embedded into the binary,
that the SPA fallback makes deep links work, that a form the agent described can
be filled in and saved, and that rotating a webhook address actually revokes the
old one.

## Running them

The agent is **not** built by the test run, and the order below matters:

```bash
cd ui && trunk build          # takes a few minutes the first time
cd .. && cargo build -p automate

cd e2e
npm install
npx playwright install chromium
npx playwright test
```

`trunk build` has to come first. The UI is embedded into the agent binary at
compile time by `include_dir!("$CARGO_MANIFEST_DIR/../ui/dist")`, so an agent
built while `ui/dist` was empty compiles and runs perfectly well and then
answers `GET /` with a 500. `agent/build.rs` declares
`cargo:rerun-if-changed=../ui/dist`, so rebuilding the UI does trigger a rebuild
of the agent — you just have to do it in that order.

Useful variants:

```bash
npx playwright test --ui                  # the interactive runner
npx playwright test tests/webhooks.spec.ts
npx playwright test --headed --debug
npx playwright show-report
```

## How the agent under test is started

`playwright.config.ts` runs `scripts/start-agent.mjs`, which makes a throwaway
directory under the system temp directory, writes a minimal `config.toml` into
it, and starts the already-built binary there. Everything the agent writes — the
SQLite database, the encryption key beside it — stays in that directory, and the
directory is removed when the run ends. Your own `config.toml` and
`database.sqlite` are never touched.

The generated configuration disables authentication entirely (`user_acl` and
`admin_acl` of `true` admit everybody), because the suite is testing the admin
UI rather than the identity provider in front of it.

The agent listens on **8099**, not the default 8080, so a run cannot point
itself at the agent you already have running with your real data behind it.
Override with `AUTOMATE_E2E_PORT`, or point the suite somewhere else entirely
with `AUTOMATE_E2E_BASE_URL`. `AUTOMATE_E2E_BINARY` selects a specific binary.

## Two traps worth knowing about

**The repository's root `.env` is a named pipe.** The agent calls
`dotenvy::from_path_override` on whatever `--env` names, defaulting to `.env`,
and `Path::exists()` is true for a FIFO — so an agent started with the
repository root as its working directory blocks forever inside that read. The
launcher therefore always passes `--env` a path that cannot exist and runs the
agent from its scratch directory. Do not remove either. (For the same reason,
never `cat` that file or run a recursive `grep` from the repository root.)

**A debug build prints nothing.** Telemetry is disabled under
`debug_assertions`, which suppresses all tracing output, so an agent that has
started successfully looks exactly like one that has hung. Readiness is
therefore established by polling `GET /robots.txt` — which is registered ahead of
the SPA catch-all, so a 200 proves the server is genuinely routing rather than
just serving `index.html` to everything. Never gate readiness on log output.

## Writing tests here

- Wait for the application with `waitForApp`/`gotoApp` from `tests/helpers.ts`,
  which watch for the `TrunkApplicationStarted` event the bundle dispatches once
  wasm boots. Not `networkidle`: the bundle is megabytes and the app keeps
  polling after it has painted.
- **The SPA fallback answers 200 with `index.html` for any unknown path**, so a
  wrong URL never produces a 404. Do not write assertions that rely on one.
- Prefer `getByRole`, `getByLabel` and `getByText`. The UI has no
  `data-testid` attributes and the suite does not need any; where a control has
  no accessible name of its own (the pause toggle on a workflow row, the webhook
  address field) it is reached by scoping to its container first.
- Every test shares one agent process and one database, so name anything you
  create with `uniqueName()` and delete it again. `purgeWorkflowsNamed` and
  `purgeConnectionsNamed` exist for `afterEach`, as a net for the case where an
  assertion fails before the test's own cleanup runs.
- Name a test as a sentence describing the behaviour and why it matters, to
  match the Rust tests in this repository.
