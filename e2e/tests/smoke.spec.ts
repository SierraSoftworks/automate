/**
 * The suite's first question: is there a working application here at all?
 *
 * Everything else assumes a booted UI talking to a live agent. When that
 * assumption is wrong, every other spec fails in its own baroque way — a
 * missing heading, a selector that matches nothing — and none of them say why.
 * These three assertions do.
 */

import { expect, gotoApp, test } from "./helpers";

test("the agent answers its readiness endpoint, which proves it is routing and not merely listening", async ({
  request,
}) => {
  // `/robots.txt` is registered ahead of the SPA catch-all, needs no session
  // and touches no database. Almost any other path would answer 200 with
  // `index.html` whether the routing table had been built or not, so this is
  // the only cheap request whose success means something.
  const response = await request.get("/robots.txt");

  expect(response.status()).toBe(200);
  expect(response.headers()["content-type"]).toContain("text/plain");
  expect(await response.text()).toContain("Disallow: /");
});

test("the root path serves the UI, which proves the bundle was embedded at compile time", async ({
  request,
}) => {
  // The UI is baked into the binary by `include_dir!`, so an agent built while
  // `ui/dist` was empty compiles and runs perfectly and then answers this with
  // a 500. That is the single most common way to end up with a suite full of
  // inexplicable failures, and it is worth one assertion of its own.
  const response = await request.get("/");

  expect(
    response.status(),
    "a 500 here means the agent was built before `trunk build` ran — rebuild the UI, then the agent",
  ).not.toBe(500);
  expect(response.status()).toBe(200);
  expect(await response.text()).toContain("TrunkApplicationStarted");
});

test("the workflows page boots and renders once wasm has started", async ({ page }) => {
  await gotoApp(page, "/admin/workflows");

  await expect(page.getByRole("heading", { level: 1, name: "Workflows" })).toBeVisible();
});
