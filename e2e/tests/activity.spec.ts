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

/** Adds a webhook-triggered workflow and returns the address it was issued. */
async function addWebhookWorkflow(
  page: import("@playwright/test").Page,
  name: string,
  connection: string,
): Promise<string> {
  await page.getByRole("button", { name: "Add Workflow" }).click();
  await page.getByRole("menuitem", { name: "Webhook" }).click();

  const form = page.getByRole("dialog", { name: "Add Webhook workflow" });
  await form.getByLabel("Name").fill(name);
  await form.getByLabel("Task title").fill("Deployed ${{ environment }}");
  await form.getByLabel("Todoist account").selectOption({ label: connection });
  await form.getByRole("button", { name: "Add workflow" }).click();

  const row = page.locator("li.workflow").filter({ hasText: name });
  await expect(row).toHaveCount(1);

  // The address is folded away until the card is opened.
  await row.getByRole("button", { name: new RegExp(name) }).click();
  return row.locator(".webhook-address").getByRole("textbox").inputValue();
}

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

test("a busy webhook does not crowd the log out", async ({ page, request }) => {
  // The reason the arrangement changed. A GitHub App on a large organisation
  // delivers thousands of times a day; if each one were an entry, the change
  // somebody made this morning would be off the end of the first page by lunch.
  const name = uniqueName(PREFIX);
  const connection = await createConnection(request, { name });

  await gotoApp(page, "/admin/workflows");
  const address = await addWebhookWorkflow(page, name, connection.name);

  const deliver = async (times: number) => {
    for (let i = 0; i < times; i++) {
      const delivery = await request.post(new URL(address).pathname, {
        data: { environment: "production", run: i },
      });
      expect(delivery.status(), "the address should accept a delivery").toBe(204);
    }
  };

  /** How many entries the log holds about this workflow, once it has settled. */
  const entries = async () => {
    await gotoApp(page, "/admin/activity");
    await page.locator("input[type=search]").fill(`subject:${name}`);
    await expect(page.locator(".activity-entry").first()).toBeVisible();
    return page.locator(".activity-entry").count();
  };

  await deliver(12);
  const after12 = await entries();

  await deliver(12);
  const after24 = await entries();

  // The property, rather than a number: what reaches the log is what changed,
  // so twice the traffic is not twice the entries.
  expect(
    after24,
    "the log must not grow with the number of deliveries a workflow takes",
  ).toBe(after12);

  await deleteConnection(request, connection.id);
});

test("a workflow says how it is getting on, and what its last run was handed", async ({
  page,
  request,
}) => {
  // The other half of the trade: what leaves the log has to turn up somewhere,
  // and "it failed" is only actionable with the payload it failed on.
  const name = uniqueName(PREFIX);
  const connection = await createConnection(request, { name });

  await gotoApp(page, "/admin/workflows");
  const address = await addWebhookWorkflow(page, name, connection.name);

  const delivery = await request.post(new URL(address).pathname, {
    data: { environment: "production" },
  });
  expect(delivery.status()).toBe(204);

  const row = page.locator("li.workflow").filter({ hasText: name });

  // The run finishes after the delivery has been answered, so the row only
  // learns how it went on a later read.
  await expect
    .poll(
      async () => {
        await page.reload();
        return row.locator(".status-pill").textContent().catch(() => null);
      },
      { message: "the row should report how its last run went", timeout: 30_000 },
    )
    .toMatch(/Working|Failing/);

  await row.getByRole("button", { name: new RegExp(name) }).click();

  const runs = row.locator(".workflow-runs");
  await expect(runs).toContainText("Last run");

  await runs.locator("summary").first().click();
  await expect(runs.locator("code").first()).toContainText("production");

  await deleteConnection(request, connection.id);
});
