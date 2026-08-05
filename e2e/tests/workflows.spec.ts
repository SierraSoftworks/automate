/**
 * The whole life of a workflow, through the UI that draws itself.
 *
 * None of these fields are written down in the browser. The agent describes
 * what an RSS workflow needs collected and the form is rendered from that
 * description, so this spec is really asking whether a descriptor turns into a
 * form a person can fill in and a record the agent accepts back.
 */

import {
  createConnection,
  deleteConnection,
  expect,
  gotoApp,
  purgeWorkflowsNamed,
  setSwitch,
  test,
  uniqueName,
  type Connection,
} from "./helpers";

const NAME_PREFIX = "E2E Workflow";

let connection: Connection;

test.beforeEach(async ({ request }) => {
  // Every workflow type that publishes to Todoist insists on naming the account
  // it publishes into, and with none linked the form renders "Link a todoist
  // account before you can use this." in place of the picker. Created through
  // the API because this spec is not testing the connections page.
  connection = await createConnection(request, {
    name: uniqueName("E2E Connection for workflows"),
  });
});

test.afterEach(async ({ request }) => {
  await purgeWorkflowsNamed(request, NAME_PREFIX);
  await deleteConnection(request, connection.id);
});

/** Opens, fills in, and submits the modal for a new RSS workflow. */
async function addRssWorkflow(page: import("@playwright/test").Page, name: string) {
  await page.getByRole("button", { name: "Add Workflow" }).click();
  await page.getByLabel("What should it watch?").selectOption("rss");

  const form = page.getByRole("dialog", { name: "Add RSS Feed workflow" });
  await expect(page.getByLabel("What should it watch?")).toHaveCount(0);

  await form.getByLabel("Name").fill(name);
  await form.getByLabel("Feed URL").fill("https://example.com/rss/");
  await form.getByLabel("Homepage").fill("https://example.com/");
  await form.getByLabel("Todoist account").selectOption({ label: connection.name });

  // The schedule arrives pre-filled from the type's declared default, so a
  // person adding a feed does not have to have an opinion about polling
  // intervals before they can save. Asserting it saves the bother of setting it.
  await expect(form.getByLabel("Schedule")).toHaveValue("@daily");

  await form.getByRole("button", { name: "Add workflow" }).click();
}

test("a workflow configured through the descriptor-driven form is listed once it is saved", async ({
  page,
}) => {
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");

  await addRssWorkflow(page, name);

  const row = page.locator("li.workflow").filter({ hasText: name });
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("rss");
  await expect(row).toContainText("every day at midnight");

  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByLabel("What should it watch?")).toHaveCount(0);

  // Reopening the flow must start with a fresh picker. In particular, selecting
  // RSS again should open its form immediately rather than requiring a detour
  // through a different workflow type to make the native select emit a change.
  await page.getByRole("button", { name: "Add Workflow" }).click();
  await page.getByLabel("What should it watch?").selectOption("rss");
  await expect(page.getByRole("dialog", { name: "Add RSS Feed workflow" })).toBeVisible();
  await page.getByRole("button", { name: "Cancel", exact: true }).last().click();
});

test("pausing a workflow from its row keeps it listed and says that it is paused", async ({
  page,
}) => {
  // Pausing goes through the same save as any other edit, sending the whole
  // configuration back with only the flag changed. Getting that wrong clears
  // the workflow's settings, which is why this checks the row still describes
  // the same feed afterwards.
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");
  await addRssWorkflow(page, name);

  const row = page.locator("li.workflow").filter({ hasText: name });
  await expect(row).toHaveCount(1);

  const enabled = row.locator(".workflow__summary").getByRole("checkbox");
  await expect(enabled).toBeChecked();

  await setSwitch(enabled, false);

  await expect(row).toContainText("paused");
  await expect(row).toContainText(name);
  await expect(row.locator(".workflow__summary").getByRole("checkbox")).not.toBeChecked();
});

test("renaming a workflow through its edit form updates the entry in the list", async ({ page }) => {
  const name = uniqueName(NAME_PREFIX);
  const renamed = uniqueName(`${NAME_PREFIX} renamed`);

  await gotoApp(page, "/admin/workflows");
  await addRssWorkflow(page, name);

  const row = page.locator("li.workflow").filter({ hasText: name });
  await expect(row).toHaveCount(1);

  await row.getByRole("button", { name: "Edit" }).click();

  const editor = row.locator(".workflow-form");
  await expect(editor.getByLabel("Name")).toHaveValue(name);
  await editor.getByLabel("Name").fill(renamed);
  await editor.getByRole("button", { name: "Save changes" }).click();

  await expect(page.locator("li.workflow").filter({ hasText: renamed })).toHaveCount(1);
  await expect(page.locator("li.workflow").filter({ hasText: name })).toHaveCount(0);
});

test("deleting a workflow removes it from the list", async ({ page }) => {
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/workflows");
  await addRssWorkflow(page, name);

  const row = page.locator("li.workflow").filter({ hasText: name });
  await expect(row).toHaveCount(1);

  await row.getByRole("button", { name: "Delete" }).click();

  await expect(page.locator("li.workflow").filter({ hasText: name })).toHaveCount(0);
});
