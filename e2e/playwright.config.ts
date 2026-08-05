import { defineConfig, devices } from "@playwright/test";

/**
 * The port the agent under test listens on.
 *
 * Deliberately not 8080. A developer running this suite very likely has their
 * own agent running on the default port with their real database behind it, and
 * a test run that quietly pointed at it would create workflows in — and delete
 * workflows from — their actual account.
 */
const port = Number(process.env.AUTOMATE_E2E_PORT ?? 8099);
const baseURL = process.env.AUTOMATE_E2E_BASE_URL ?? `http://127.0.0.1:${port}`;

export default defineConfig({
  testDir: "./tests",

  // Every test talks to one agent process backed by one SQLite database, so
  // running them concurrently would have them reading each other's workflows
  // and connections. Names are unique per test as a second line of defence, but
  // serial execution is what makes a failure mean what it says.
  fullyParallel: false,
  workers: 1,

  // A `.only` left in a spec file silently reduces CI to that one test.
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,

  // The wasm bundle is several megabytes and is compiled by the browser on
  // first load, so the first navigation of a run is far slower than the rest.
  timeout: 60_000,
  expect: { timeout: 15_000 },

  reporter: process.env.CI ? [["github"], ["html", { open: "never" }]] : [["list"], ["html", { open: "never" }]],

  use: {
    baseURL,
    trace: "on-first-retry",
    screenshot: "only-on-failure",
    video: "off",
    actionTimeout: 15_000,
    navigationTimeout: 30_000,
  },

  projects: [
    // Chromium only, on purpose. The UI is one wasm bundle rendered by Yew
    // rather than a stack of browser-specific CSS and DOM workarounds, so a
    // second engine would re-run the same assertions against the same code for
    // roughly triple the wall-clock time. Add a browser here when a bug is
    // found that only one of them has.
    {
      name: "chromium",
      use: { ...devices["Desktop Chrome"] },
    },
  ],

  webServer: {
    command: "node scripts/start-agent.mjs",
    // `/robots.txt` is registered before the SPA catch-all, so a 200 here means
    // the server is genuinely routing. Almost any other path would answer 200
    // with `index.html` whether the routes were wired up or not.
    url: `${baseURL}/robots.txt`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
    stdout: "pipe",
    stderr: "pipe",
    // Playwright SIGKILLs the server's process group unless asked otherwise,
    // which would leave the scratch directory — database, encryption key and
    // all — behind after every run. A signal the launcher can catch lets it
    // remove what it made.
    gracefulShutdown: { signal: "SIGTERM", timeout: 5_000 },
    env: {
      AUTOMATE_E2E_PORT: String(port),
    },
  },
});
