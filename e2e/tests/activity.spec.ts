/**
 * The record of what the agent has been doing.
 *
 * The value of the log is that it survives the thing it describes: a workflow
 * that ran overnight, failed, and was then deleted still has to be explainable
 * in the morning. So these tests drive real activity through the agent and then
 * ask the page about it, rather than asserting against a fixture.
 */

import {
  createConnection,
  deleteConnection,
  expect,
  gotoApp,
  purgeConnectionsNamed,
  purgeWorkflowsNamed,
  test,
  uniqueName,
} from "./helpers";

const PREFIX = "e2e-activity";

test.afterEach(async ({ request }) => {
  await purgeWorkflowsNamed(request, PREFIX);
  await purgeConnectionsNamed(request, PREFIX);
});

test("linking a service shows up in the activity log", async ({ page, request }) => {
  const name = uniqueName(PREFIX);
  const connection = await createConnection(request, { name });

  await gotoApp(page, "/admin/activity");

  const entry = page.locator(".activity-entry").filter({ hasText: name });
  await expect(entry.first()).toBeVisible();
  await expect(entry.first().locator(".status-pill")).toHaveText("Succeeded");

  await deleteConnection(request, connection.id);
});

test("the log can be narrowed by outcome and by subject", async ({ page, request }) => {
  const name = uniqueName(PREFIX);
  const connection = await createConnection(request, { name });

  await gotoApp(page, "/admin/activity");
  await expect(page.locator(".activity-entry").filter({ hasText: name }).first()).toBeVisible();

  // Everything the agent records for a linked service succeeded, so asking for
  // the failures must leave this one out rather than merely reorder it.
  await page.locator("input[type=search]").fill("outcome:failure");
  await expect(page.locator(".activity-entry").filter({ hasText: name })).toHaveCount(0);

  await page.locator("input[type=search]").fill(`subject:${name}`);
  await expect(page.locator(".activity-entry").filter({ hasText: name }).first()).toBeVisible();
  await expect(page.locator(".activity-entry")).toHaveCount(
    await page.locator(".activity-entry").filter({ hasText: name }).count(),
  );

  await deleteConnection(request, connection.id);
});

test("the search bar completes the fields this page offers", async ({ page }) => {
  await gotoApp(page, "/admin/activity");

  await page.locator("input[type=search]").fill("out");

  const suggestions = page.locator(".app-bar__suggestion");
  await expect(suggestions).toHaveCount(1);
  await expect(suggestions.first()).toContainText("outcome:");
});
