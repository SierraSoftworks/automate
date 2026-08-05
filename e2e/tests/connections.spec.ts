/**
 * Linking and unlinking the services workflows publish through.
 *
 * The interesting property of this page is not that it can add a row. It is
 * that a credential goes in and never comes back out: the list is built from
 * summaries which have nowhere to carry a token, so the page cannot show one
 * even by accident. That is a claim worth checking rather than trusting.
 */

import {
  expect,
  gotoApp,
  purgeConnectionsNamed,
  test,
  uniqueName,
} from "./helpers";

const NAME_PREFIX = "E2E Connection";

test.afterEach(async ({ request }) => {
  await purgeConnectionsNamed(request, NAME_PREFIX);
});

/** Opens the provider-specific modal for a new Todoist connection. */
async function openTodoistConnection(page: import("@playwright/test").Page) {
  await page.getByRole("button", { name: "Add Connection" }).click();
  await page.getByRole("menuitem", { name: "Todoist" }).click();

  const form = page.getByRole("dialog", { name: "Add Todoist connection" });
  return form;
}

test("credential and account setup methods share one menu", async ({ page }) => {
  await gotoApp(page, "/admin/connections?demo");

  const toolbar = page.locator(".page-toolbar");
  const add = toolbar.getByRole("button", { name: "Add Connection" });
  await expect(add).toHaveCount(1);
  await expect(toolbar.getByRole("button", { name: "Connect", exact: true })).toHaveCount(0);

  await add.click();
  await expect(page.locator(".menu-button__section")).toHaveText([
    "API keys",
    "Authorized accounts",
  ]);
  await expect(page.getByRole("menuitem")).toHaveText([
    "Todoist",
    "GitHub",
    "YNAB",
    "GitHub",
    "Spotify",
  ]);

  await page.getByRole("menuitem", { name: "Spotify" }).click();
  await expect(toolbar.getByRole("status")).toContainText(
    "Some connection methods unavailable",
  );
});

test("provider and account type appear beside connection names", async ({ page }) => {
  await gotoApp(page, "/admin/connections?demo");

  const organization = page.locator(".connection").filter({ hasText: "SierraSoftworks" });
  await expect(organization.locator(".connection__provider")).toHaveText("github/");
  await expect(organization.locator(".connection__title .connection__name")).toHaveText(
    "SierraSoftworks",
  );
  await expect(organization.locator(".connection__account-type")).toHaveText("Organization");
  await expect(organization.locator(".connection__meta")).toHaveText(
    /^app installation · linked /,
  );

  const personal = page.locator(".connection").filter({ hasText: "Personal" });
  await expect(personal.locator(".connection__provider")).toHaveText("todoist/");
  await expect(personal.locator(".connection__account-type")).toHaveCount(0);
});

test("a service linked with a pasted token appears in the list and can be unlinked again", async ({
  page,
}) => {
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/connections");

  const form = await openTodoistConnection(page);
  await form.getByLabel("Name").fill(name);
  await form.getByLabel("Token").fill("e2e-token-listed-and-removed");
  await form.getByRole("button", { name: "Link service" }).click();

  const row = page.locator(".connection").filter({ hasText: name });
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("todoist");
  await expect(row).toContainText("token");
  await expect(page.getByRole("dialog")).toHaveCount(0);

  // The creation flow starts fresh after a successful save, including when the
  // next connection uses the same provider as the one just linked.
  const reopened = await openTodoistConnection(page);
  await expect(reopened).toBeVisible();
  await reopened.getByRole("button", { name: "Cancel", exact: true }).click();

  // Unlinking stops every workflow publishing through the account, so the page
  // asks first. Playwright dismisses dialogs unless told otherwise, which would
  // leave the row in place and the assertion below failing for the wrong reason.
  page.once("dialog", (dialog) => {
    expect(dialog.message()).toContain(name);
    return dialog.accept();
  });
  await row.getByRole("button", { name: "Unlink" }).click();

  await expect(page.locator(".connection").filter({ hasText: name })).toHaveCount(0);
});

test("an API-key connection can be edited without revealing its existing credential", async ({
  page,
  request,
}) => {
  const name = uniqueName(NAME_PREFIX);
  const renamed = uniqueName(`${NAME_PREFIX} renamed`);
  const replacement = `e2e-replacement-${Math.random().toString(36).slice(2)}-do-not-echo`;

  const created = await request.post("/api/v1/connections", {
    data: { provider: "todoist", name, key: "e2e-original-key" },
  });
  expect(created.status()).toBe(201);

  await gotoApp(page, "/admin/connections");
  const row = page.locator(".connection").filter({ hasText: name });
  await row.getByRole("button", { name: "Edit" }).click();

  const form = page.getByRole("dialog", { name: `Edit ${name} connection` });
  const key = form.getByLabel("New API key");
  await expect(key).toHaveAttribute("type", "password");
  await expect(key).toHaveValue("");
  await expect(form.getByText("Write-only. Leave blank to keep the existing API key.")).toBeVisible();

  await form.getByLabel("Name").fill(renamed);
  await key.fill(replacement);
  await form.getByRole("button", { name: "Save changes" }).click();

  await expect(page.locator(".connection").filter({ hasText: renamed })).toHaveCount(1);
  await expect(form).toHaveCount(0);
  expect(await page.locator("body").innerText()).not.toContain(replacement);
  expect(await page.content()).not.toContain(replacement);

  const listed = await request.get("/api/v1/connections");
  expect(await listed.text()).not.toContain(replacement);
});

test("a connection that needs reauthorization offers the reconnect workflow", async ({ page }) => {
  await gotoApp(page, "/admin/connections?demo");

  const row = page.locator(".connection").filter({ hasText: "Spotify" });
  const reconnect = row.getByRole("button", { name: "Reconnect", exact: true });
  await expect(row.locator("span.connection__status--warning")).toHaveText("Needs reconnecting");
  await expect(row.getByRole("button", { name: "Needs reconnecting" })).toHaveCount(0);
  await expect(row.getByRole("button", { name: "Edit" })).toHaveCount(0);

  await reconnect.click();
  await expect(row.getByRole("alert")).toContainText(
    "Connecting an integration needs a running agent",
  );
});

test("a token is never shown again once it has been saved", async ({ page }) => {
  // Credentials travel one way by design. If this ever fails, a token is
  // recoverable by anybody who can load the page — including from a cached
  // response or a browser's back button — which is a disclosure rather than a
  // cosmetic bug.
  const name = uniqueName(NAME_PREFIX);
  const token = `e2e-secret-${Math.random().toString(36).slice(2)}-do-not-echo`;

  await gotoApp(page, "/admin/connections");

  const form = await openTodoistConnection(page);
  await form.getByLabel("Name").fill(name);
  await form.getByLabel("Token").fill(token);
  await form.getByRole("button", { name: "Link service" }).click();

  await expect(page.locator(".connection").filter({ hasText: name })).toHaveCount(1);

  // The form is removed after saving, and nothing rendered from the saved
  // connection carries the token.
  await expect(form).toHaveCount(0);
  expect(await page.locator("body").innerText()).not.toContain(token);

  // Again from a fresh load, which is the case that matters: the first check
  // could pass simply because the page had not re-rendered from the server's
  // answer yet.
  await gotoApp(page, "/admin/connections");
  await expect(page.locator(".connection").filter({ hasText: name })).toHaveCount(1);

  expect(await page.locator("body").innerText()).not.toContain(token);
  expect(
    await page.content(),
    "the token must not survive anywhere in the document, including as an attribute",
  ).not.toContain(token);
});

test("the API never returns a stored credential either", async ({ request }) => {
  // The page can only fail to show a token it was never given, so the promise
  // is really the API's to keep. Checked here so a change that started
  // returning secrets would be caught at the source rather than by whichever
  // page happened to render one.
  const name = uniqueName(NAME_PREFIX);
  const token = `e2e-secret-${Math.random().toString(36).slice(2)}-api-only`;

  const created = await request.post("/api/v1/connections", {
    data: { provider: "todoist", name, key: token },
  });
  expect(created.status()).toBe(201);
  expect(await created.text()).not.toContain(token);

  const listed = await request.get("/api/v1/connections");
  expect(listed.status()).toBe(200);
  expect(await listed.text()).not.toContain(token);
});
