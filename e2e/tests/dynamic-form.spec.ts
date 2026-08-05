/**
 * The form that is not written down anywhere in the browser.
 *
 * The agent describes what each workflow type needs collected and the UI draws
 * it, which is what lets a workflow type added to the agent get a working form
 * without the UI being rebuilt. The two things that can go wrong with that
 * arrangement are drawing the wrong type's fields and failing to clear the
 * previous type's, and both look like a form that is merely a bit odd rather
 * than one that is collecting a configuration the agent will refuse.
 */

import {
  createConnection,
  deleteConnection,
  expect,
  gotoApp,
  test,
  uniqueName,
  type Connection,
} from "./helpers";

let connection: Connection;

async function openWorkflowPicker(page: Parameters<typeof gotoApp>[0]) {
  await gotoApp(page, "/admin/workflows");
  await page.getByRole("button", { name: "Add Workflow" }).click();
}

async function openWorkflowForm(
  page: Parameters<typeof gotoApp>[0],
  type: string,
) {
  await openWorkflowPicker(page);
  await page.getByRole("menuitem", { name: type, exact: true }).click();
  return page.getByRole("dialog");
}

test.beforeEach(async ({ request }) => {
  connection = await createConnection(request, {
    name: uniqueName("E2E Connection for forms"),
  });
});

test.afterEach(async ({ request }) => {
  await deleteConnection(request, connection.id);
});

test("no fields are asked for until a workflow type has been chosen", async ({ page }) => {
  await openWorkflowPicker(page);

  await expect(page.getByRole("menu")).toBeVisible();

  // Nothing beyond the picker: there is no such thing as a field common to
  // every workflow type, so a form drawn before a type is chosen would be
  // guessing.
  await expect(page.getByRole("button", { name: "Add workflow", exact: true })).toHaveCount(0);
  await expect(page.getByLabel("Enabled")).toHaveCount(0);
});

test("choosing a workflow type draws the fields that type asked for", async ({ page }) => {
  const form = await openWorkflowForm(page, "RSS Feed");

  await expect(form).toContainText("Watches a feed and files a task for each new entry.");

  for (const label of ["Name", "Feed URL", "Homepage", "Filter", "Todoist account"]) {
    await expect(form.getByLabel(label), `an RSS workflow should ask for "${label}"`).toBeVisible();
  }

  // An RSS feed is polled, so it is asked when — and offered its type's own
  // default so that nobody has to have an opinion about polling intervals
  // before they can save.
  await expect(form.getByLabel("Schedule")).toHaveValue("@daily");
  await expect(form.getByLabel("Enabled")).toBeChecked();
  await expect(form.getByRole("button", { name: "Add workflow" })).toBeVisible();
});

test("filter expressions are checked while they are being edited", async ({ page }) => {
  const dialog = await openWorkflowForm(page, "RSS Feed");
  const filter = dialog.getByLabel("Filter");
  const editor = dialog.locator(".filter-input");

  await expect(editor.locator(".filter-input__fields")).toContainText(
    "Available fields: title, description, link",
  );

  await filter.fill('title contains "release"');
  await expect(editor.locator(".filter-input__message--valid")).toContainText(
    "Valid expression. Uses: title.",
  );
  await expect(editor.locator(".filter-input__message--warning")).toHaveCount(0);

  await filter.fill('author == "Ada"');
  await expect(editor.locator(".filter-input__message--warning")).toHaveText(
    "Unsupported field: author.",
  );

  await filter.fill("title ==");
  await expect(editor.locator(".filter-input__message--error")).toBeVisible();
  await expect(filter).toHaveClass(/field__input--invalid/);
});

test("text inputs preserve spaces while typing and normalize them on blur", async ({ page }) => {
  const form = await openWorkflowForm(page, "RSS Feed");

  const name = form.getByLabel("Name");
  const raw = "  Release notes daily  ";

  // Type one key at a time so a controlled input rerenders between spaces. A
  // change handler that trims eagerly removes those spaces before the next key.
  await name.pressSequentially(raw);
  await expect(name).toHaveValue(raw);

  await form.getByLabel("Feed URL").click();
  await expect(name).toHaveValue(raw.trim());

  await name.fill("   ");
  await expect(name).toHaveValue("   ");
  await form.getByLabel("Feed URL").click();
  await expect(name).toHaveValue("");
});

test("text areas preserve multiline whitespace while editing and normalize it on blur", async ({
  page,
}) => {
  const form = await openWorkflowForm(page, "Webhook");

  const description = form.getByLabel("Task description");
  const raw = "  Started by Ada\n\nDeployment complete  ";

  await description.fill(raw);
  await expect(description).toHaveValue(raw);

  await form.getByLabel("Task title").click();
  await expect(description).toHaveValue(raw.trim());

  await description.fill(" \n ");
  await expect(description).toHaveValue(" \n ");
  await form.getByLabel("Task title").click();
  await expect(description).toHaveValue("");
});

test("choosing a different type replaces the previous type's fields rather than adding to them", async ({
  page,
}) => {
  // The form is keyed by type so that changing the answer starts a fresh one.
  // Without that, fields that happen to share a name carry their values across
  // and fields that do not are left on screen collecting values the new type
  // will never read.
  let form = await openWorkflowForm(page, "RSS Feed");
  await expect(form.getByLabel("Feed URL")).toBeVisible();
  await expect(form.getByLabel("Name")).toBeVisible();

  await form.getByRole("button", { name: "Cancel" }).click();
  form = await openWorkflowForm(page, "XKCD");

  await expect(form).toContainText("Files a task for each new XKCD comic.");
  await expect(
    form.getByLabel("Feed URL"),
    "an XKCD workflow has no feed to be told about, so the RSS field must be gone",
  ).toHaveCount(0);
  await expect(form.getByLabel("Name")).toHaveCount(0);

  // The fields the two types share are still asked for, so this is a redraw
  // rather than an empty form.
  await expect(form.getByLabel("Filter")).toBeVisible();
  await expect(form.getByLabel("Todoist account")).toBeVisible();
});

test("a picker whose choices come from an account says so until an account is chosen", async ({
  page,
}) => {
  // The projects in a Todoist account can only be listed once we know which
  // account. An empty menu would read as a broken form rather than as a step
  // the person has not taken yet, so the form says which step.
  const form = await openWorkflowForm(page, "RSS Feed");

  const unavailable = form.locator(".dynamic-form__unavailable");
  await expect(unavailable.first()).toContainText(
    "Choose an account first, and the options will be loaded from it.",
  );

  // Both the project and the section are scoped by the account, so both should
  // be saying it.
  await expect(unavailable).toHaveCount(2);
  await expect(form.getByLabel("Project")).toHaveCount(0);
  await expect(form.getByLabel("Section")).toHaveCount(0);

  await form.getByLabel("Todoist account").selectOption({ label: connection.name });

  await expect(form.locator(".dynamic-form__unavailable")).toHaveCount(0);
  await expect(form.getByLabel("Project")).toBeVisible();
  await expect(form.getByLabel("Section")).toBeVisible();
});

test("a connection picker offers the accounts that have actually been linked", async ({ page }) => {
  const form = await openWorkflowForm(page, "RSS Feed");

  const picker = form.getByLabel("Todoist account");
  await expect(picker.getByRole("option", { name: connection.name })).toHaveCount(1);
});

test("a workflow type explains how to set one up, without getting in the way of the form", async ({
  page,
}) => {
  // The guidance is the difference between a form somebody can fill in and one
  // they have to guess at — a webhook type in particular is useless until you
  // know where in the provider's interface the address goes. It starts
  // collapsed, because somebody adding their fourth feed should not have to
  // scroll past an explanation of RSS to reach the fields.
  const form = await openWorkflowForm(page, "RSS Feed");

  const guidance = form.locator(".documentation");
  await expect(guidance.getByRole("button", { name: "How does this work?" })).toBeVisible();
  await expect(guidance.locator(".documentation__body")).toBeHidden();

  await guidance.getByRole("button", { name: "How does this work?" }).click();

  const body = guidance.locator(".documentation__body");
  await expect(body).toBeVisible();

  // Rendered as Markdown rather than shown as its source.
  await expect(body.locator("h2, h3, p").first()).toBeVisible();
  await expect(body).not.toContainText("##");

  // Links leave for someone else's site, so they must not take the half-filled
  // form with them.
  const links = body.locator("a");
  for (let i = 0; i < (await links.count()); i++) {
    await expect(links.nth(i)).toHaveAttribute("target", "_blank");
    await expect(links.nth(i)).toHaveAttribute("rel", /noopener/);
  }
});
