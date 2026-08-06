/**
 * Webhook addresses, and what happens to the old one when a new one is issued.
 *
 * A webhook URL is the entire credential. There is no signature to check and no
 * second factor behind it: whoever holds the address can post into somebody's
 * account. So the only thing that makes a leaked address recoverable is that
 * issuing a new one *revokes* the old one, immediately and completely.
 *
 * That is a claim about the agent which the UI reports on, and the two halves
 * are easy to get separately right and jointly wrong — a button that shows a
 * new address while the old one still works looks completely correct from the
 * screen. This spec is the one place both halves are checked together: it reads
 * the address the UI displays, rotates through the UI, and then puts real
 * requests to both addresses to see which of them the agent actually honours.
 */

import {
  WEBHOOK_URL,
  createConnection,
  deleteConnection,
  expect,
  gotoApp,
  purgeWorkflowsNamed,
  test,
  uniqueName,
  workflowAction,
  type Connection,
} from "./helpers";

const NAME_PREFIX = "E2E Webhook";

let connection: Connection;

test.beforeEach(async ({ request }) => {
  connection = await createConnection(request, {
    name: uniqueName("E2E Connection for webhooks"),
  });
});

test.afterEach(async ({ request }) => {
  await purgeWorkflowsNamed(request, NAME_PREFIX);
  await deleteConnection(request, connection.id);
});

/** Adds a webhook-triggered workflow and returns its row. */
async function addWebhookWorkflow(page: import("@playwright/test").Page, name: string) {
  await page.getByRole("button", { name: "Add Workflow" }).click();
  await page.getByRole("menuitem", { name: "Webhook" }).click();

  const form = page.getByRole("dialog", { name: "Add Webhook workflow" });

  await form.getByLabel("Name").fill(name);
  await form.getByLabel("Task title").fill("Deployed ${{ environment }}");
  await form.getByLabel("Todoist account").selectOption({ label: connection.name });

  // A webhook workflow runs when something posts to it, so there is no schedule
  // to collect and the form must not be asking for one.
  await expect(form.getByLabel("Schedule")).toHaveCount(0);

  await form.getByRole("button", { name: "Add workflow" }).click();

  const row = page.locator("li.workflow").filter({ hasText: name });
  await expect(row).toHaveCount(1);

  // The address lives in the card's folded-away detail, so every test that
  // wants it has to open the card first.
  await row.getByRole("button", { name: new RegExp(name) }).click();
  await expect(row.locator(".webhook-address")).toBeVisible();

  return row;
}

test("a webhook workflow is given an address of its own to receive deliveries on", async ({
  page,
}) => {
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");

  const row = await addWebhookWorkflow(page, name);

  const address = row.locator(".webhook-address");
  await expect(address).toBeVisible();
  await expect(address).toContainText("Send deliveries to");

  // Shown in full, and readable rather than write-only: its owner has to paste
  // it into whatever will be calling it, and an address you can only see once
  // is an address that gets written down somewhere worse.
  const field = address.getByRole("textbox");
  await expect(field).not.toBeEditable();
  expect(await field.inputValue()).toMatch(WEBHOOK_URL);

  await expect(address.getByRole("button", { name: "Copy" })).toBeVisible();
  await expect(address.getByRole("button", { name: "Issue a new address" })).toBeVisible();
});

test("a card's details fold away until its header is clicked", async ({ page }) => {
  // A row that showed everything it knows would be several inches tall on a
  // page whose job is to let somebody scan a dozen of them.
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");

  const row = await addWebhookWorkflow(page, name);
  const header = row.getByRole("button", { name: new RegExp(name) });

  await expect(header).toHaveAttribute("aria-expanded", "true");

  await header.click();
  await expect(header).toHaveAttribute("aria-expanded", "false");
  await expect(row.locator(".webhook-address")).toHaveCount(0);

  await header.click();
  await expect(row.locator(".webhook-address")).toBeVisible();
});

test("the row's own controls do not open the card", async ({ page }) => {
  // The whole row is clickable, so the controls sitting in it have to be
  // exceptions — otherwise pausing a workflow also unfolds it.
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");

  const row = await addWebhookWorkflow(page, name);
  const header = row.getByRole("button", { name: new RegExp(name) });

  await header.click();
  await expect(header).toHaveAttribute("aria-expanded", "false");

  await row.locator(".switch__track").click();
  await expect(row).toContainText("paused");
  await expect(header).toHaveAttribute("aria-expanded", "false");

  await row.getByRole("button", { name: "Edit", exact: true }).click();
  await expect(row.locator(".workflow-form")).toBeVisible();
  await expect(header).toHaveAttribute("aria-expanded", "false");
});

test("issuing a new address warns before it does it, and can be backed out of", async ({ page }) => {
  // Rotating breaks whatever is already sending to the old address, so it has
  // to be a deliberate act rather than one click. The address must be unchanged
  // if the confirmation is declined.
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");

  const row = await addWebhookWorkflow(page, name);
  const address = row.locator(".webhook-address");
  const before = await address.getByRole("textbox").inputValue();

  await address.getByRole("button", { name: "Issue a new address" }).click();

  await expect(address).toContainText("A new address takes effect at once");
  await address.getByRole("button", { name: "Keep this one" }).click();

  await expect(address).not.toContainText("A new address takes effect at once");
  expect(await address.getByRole("textbox").inputValue()).toBe(before);
});

test("a rotated webhook address stops working immediately, and the new one starts", async ({
  page,
  request,
}) => {
  // The reason this suite exists. Everything up to the rotation is the UI
  // reporting what it believes; the two requests at the end are the only part
  // that can tell the difference between an address that was revoked and one
  // that was merely redrawn.
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");

  const row = await addWebhookWorkflow(page, name);
  const address = row.locator(".webhook-address");

  const original = await address.getByRole("textbox").inputValue();
  expect(original).toMatch(WEBHOOK_URL);

  // The original has to work first, or "the old one stopped working" would be
  // satisfied by an address that never worked at all.
  const beforeRotation = await request.post(new URL(original).pathname, {
    data: { environment: "production" },
  });
  expect(
    beforeRotation.status(),
    "a freshly issued address should accept a delivery",
  ).toBe(204);

  await address.getByRole("button", { name: "Issue a new address" }).click();
  await expect(address).toContainText("A new address takes effect at once");
  await address
    .locator(".webhook-address__confirm")
    .getByRole("button", { name: "Issue a new address" })
    .click();

  // The list is refetched after a rotation, so wait for the displayed address
  // to actually be the replacement rather than reading the old one back.
  await expect
    .poll(async () => address.getByRole("textbox").inputValue(), {
      message: "the displayed address should be replaced once a new one is issued",
    })
    .not.toBe(original);

  const replacement = await address.getByRole("textbox").inputValue();
  expect(replacement).toMatch(WEBHOOK_URL);

  const toOld = await request.post(new URL(original).pathname, {
    data: { environment: "production" },
  });
  expect(
    toOld.status(),
    "a rotated address must be refused — anything else means a leaked URL is still live",
  ).toBe(404);

  const toNew = await request.post(new URL(replacement).pathname, {
    data: { environment: "production" },
  });
  expect(toNew.status(), "the replacement address should accept deliveries").toBe(204);
});

test("the address of a deleted workflow stops working", async ({ page, request }) => {
  // Deleting is the other way an address ought to stop existing, and it goes
  // through a different path in the agent than rotation does.
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");

  const row = await addWebhookWorkflow(page, name);
  const path = new URL(await row.locator(".webhook-address").getByRole("textbox").inputValue())
    .pathname;

  expect((await request.post(path, { data: { environment: "production" } })).status()).toBe(204);

  await workflowAction(row, "Delete");
  await expect(page.locator("li.workflow").filter({ hasText: name })).toHaveCount(0);

  expect(
    (await request.post(path, { data: { environment: "production" } })).status(),
    "an address whose workflow has gone must not still accept deliveries",
  ).toBe(404);
});

test("a guessed webhook address is refused", async ({ request }) => {
  // The endpoint is anonymous by necessity, so the only thing standing between
  // a stranger and somebody's account is that the address cannot be guessed.
  for (const guess of [
    "/webhooks/w/AAAAAAAAAAAAAAAAAAAAAA",
    "/webhooks/w/not-a-token",
    "/webhooks/w/AAAA",
  ]) {
    const response = await request.post(guess, { data: {} });
    expect(response.status(), `${guess} should have been refused`).toBe(404);
  }
});
