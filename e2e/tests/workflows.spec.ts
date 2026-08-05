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

/** Fills in and submits the "Add a workflow" form for an RSS feed. */
async function addRssWorkflow(page: import("@playwright/test").Page, name: string) {
  const form = page.locator(".workflows__form");

  await form.getByLabel("What should it watch?").selectOption("rss");

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

  // The form empties itself so the next workflow starts from a blank one rather
  // than from the last one's answers.
  await expect(page.locator(".workflows__form").getByLabel("What should it watch?")).toHaveValue("");
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

  // Scoped to the row: the "Add a workflow" form is still on the page below,
  // and it has a field called "Name" too.
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
