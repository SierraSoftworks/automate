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

test("a service linked with a pasted token appears in the list and can be unlinked again", async ({
  page,
}) => {
  const name = uniqueName(NAME_PREFIX);
  await gotoApp(page, "/admin/connections");

  const form = page.locator(".connections__form");
  await form.getByLabel("Service").selectOption("todoist");
  await form.getByLabel("Name").fill(name);
  await form.getByLabel("Token").fill("e2e-token-listed-and-removed");
  await form.getByRole("button", { name: "Link service" }).click();

  const row = page.locator(".connection").filter({ hasText: name });
  await expect(row).toHaveCount(1);
  await expect(row).toContainText("todoist");
  await expect(row).toContainText("token");

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

test("a token is never shown again once it has been saved", async ({ page }) => {
  // Credentials travel one way by design. If this ever fails, a token is
  // recoverable by anybody who can load the page — including from a cached
  // response or a browser's back button — which is a disclosure rather than a
  // cosmetic bug.
  const name = uniqueName(NAME_PREFIX);
  const token = `e2e-secret-${Math.random().toString(36).slice(2)}-do-not-echo`;

  await gotoApp(page, "/admin/connections");

  const form = page.locator(".connections__form");
  await form.getByLabel("Service").selectOption("todoist");
  await form.getByLabel("Name").fill(name);
  await form.getByLabel("Token").fill(token);
  await form.getByRole("button", { name: "Link service" }).click();

  await expect(page.locator(".connection").filter({ hasText: name })).toHaveCount(1);

  // The form should have emptied itself, and nothing rendered from the saved
  // connection should carry the token.
  await expect(form.getByLabel("Token")).toHaveValue("");
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
