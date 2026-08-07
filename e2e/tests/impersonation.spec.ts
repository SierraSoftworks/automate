/**
 * Acting as another account.
 *
 * Driven in demo mode, because the agent the suite starts has no identity
 * provider: there is nobody to be an administrator, so the real build never
 * offers the controls at all. What is worth holding here is the browser half —
 * that choosing an account announces itself, redirects what the pages ask for,
 * and can be undone — and the demo store answers the impersonation header the
 * same way the agent does, so that half is genuinely exercised.
 */

import { expect, gotoApp, test } from "./helpers";

const banner = ".impersonation";

test("the accounts page lists the installation's own account alongside the people", async ({
  page,
}) => {
  await gotoApp(page, "/admin/users?demo");

  await expect(page.getByRole("heading", { level: 1, name: "Accounts" })).toBeVisible();

  // Nobody signs into it, so it has no dates to report — and saying so is the
  // point, since an empty column would read as "never seen".
  const installation = page.locator(".account", { hasText: "!local" });
  await expect(installation).toContainText("Nobody signs into this account");

  // The account the browser is already in cannot be acted as, and must not
  // offer a control that would do nothing.
  await expect(page.locator(".account", { hasText: "demo" }).first()).toContainText(
    "You are here",
  );
});

test("acting as somebody announces it, and can be undone", async ({ page }) => {
  await gotoApp(page, "/admin/users?demo");

  await expect(page.locator(banner)).toHaveCount(0);

  await page.locator(".account", { hasText: "rmuir" }).getByRole("button", { name: "Act as" }).click();

  await expect(page.locator(banner)).toContainText("You are acting as");
  await expect(page.locator(banner)).toContainText("rmuir");

  // The chip has to follow, or the two halves of the screen disagree about
  // whose records are on it.
  await expect(page.locator(".user-chip__name")).toHaveText("Rowan Muir");

  await page.locator(banner).getByRole("button", { name: "Back to my own account" }).click();

  await expect(page.locator(banner)).toHaveCount(0);
  await expect(page.locator(".user-chip__name")).toHaveText("Demo User");
});

test("the choice survives a navigation, because it is the session and not the page", async ({
  page,
}) => {
  await gotoApp(page, "/admin/users?demo");
  await page.locator(".account", { hasText: "rmuir" }).getByRole("button", { name: "Act as" }).click();
  await expect(page.locator(banner)).toBeVisible();

  await page.goto("/admin/workflows?demo");

  await expect(page.locator(banner)).toContainText("rmuir");
});

test("a suspended account cannot be acted as", async ({ page }) => {
  await gotoApp(page, "/admin/users?demo");

  // The agent refuses it, so offering the choice would only produce an error
  // somebody has to read to find out it was never possible.
  const suspended = page.locator(".account", { hasText: "tbelrose" });
  await expect(suspended).toContainText("Suspended");
  await expect(suspended.getByRole("button", { name: "Act as" })).toHaveCount(0);
});

test("suspending an account is reflected in its row", async ({ page }) => {
  await gotoApp(page, "/admin/users?demo");

  const row = page.locator(".account", { hasText: "rmuir" });
  await row.getByRole("button", { name: "Suspend" }).click();

  await expect(row).toContainText("Suspended");
  await expect(row.getByRole("button", { name: "Restore" })).toBeVisible();
});
