/**
 * Getting around the admin area.
 *
 * The shell is shared by all three pages, so the title, the subtitle and the
 * highlighted nav link are all decided from the route rather than by the page —
 * which is exactly the arrangement in which every page can end up claiming to
 * be the same one.
 */

import { expect, gotoApp, test, waitForApp } from "./helpers";

const pages = [
  {
    path: "/admin",
    title: "Admin",
    subtitle: "Browse the key-value store and job queues across every partition.",
    nav: "Data",
  },
  {
    path: "/admin/connections",
    title: "Connections",
    subtitle: "The services this account is linked to, and the credentials used to reach them.",
    nav: "Connections",
  },
  {
    path: "/admin/workflows",
    title: "Workflows",
    subtitle: "The things Automate watches for you, and what it does when they change.",
    nav: "Workflows",
  },
];

for (const { path, title, subtitle, nav } of pages) {
  test(`${path} says which page it is, and the nav agrees`, async ({ page }) => {
    await gotoApp(page, path);

    await expect(page.getByRole("heading", { level: 1, name: title })).toBeVisible();
    await expect(page.getByText(subtitle)).toBeVisible();

    // Only one link may be highlighted. Both spellings of the browser route
    // resolve to the same destination, so it is possible for one to appear
    // unselected while the other is on screen.
    const active = page.locator(".admin-nav__link--active");
    await expect(active).toHaveCount(1);
    await expect(active).toHaveText(nav);
  });
}

test("following a nav link changes the page without a reload", async ({ page }) => {
  await gotoApp(page, "/admin/workflows");

  await page.getByRole("link", { name: "Connections" }).click();

  await expect(page).toHaveURL(/\/admin\/connections$/);
  await expect(page.getByRole("heading", { level: 1, name: "Connections" })).toBeVisible();
  await expect(page.locator(".admin-nav__link--active")).toHaveText("Connections");
});

test("a hard refresh on a deep link lands on the same page it was showing", async ({ page }) => {
  // Client-side routes do not exist on the server, so a refresh asks the agent
  // for `/admin/workflows` directly. The SPA fallback has to answer with
  // `index.html` for the router to then resolve the route in the browser;
  // without it a refresh would be a dead end on every page but the root.
  await gotoApp(page, "/admin/workflows");
  await expect(page.getByRole("heading", { level: 1, name: "Workflows" })).toBeVisible();

  await page.reload();
  await waitForApp(page);

  await expect(page).toHaveURL(/\/admin\/workflows$/);
  await expect(page.getByRole("heading", { level: 1, name: "Workflows" })).toBeVisible();
});

test("the landing page sends a visitor who needs no sign-in straight to the admin area", async ({
  page,
}) => {
  // With no identity provider configured the session resolves immediately, and
  // there is nothing on the landing page for somebody who is already through
  // the door.
  await gotoApp(page, "/");

  await expect(page).toHaveURL(/\/admin$/);
});
