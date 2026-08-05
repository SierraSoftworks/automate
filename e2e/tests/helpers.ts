/**
 * Shared plumbing for the end-to-end suite.
 *
 * Three things live here: knowing when the wasm application is actually ready
 * to be driven, creating the fixtures a test needs without going through the UI
 * to do it, and naming those fixtures so that two tests can never see each
 * other's.
 */

import {
  expect,
  test as base,
  type APIRequestContext,
  type Locator,
  type Page,
} from "@playwright/test";

/**
 * The base test, with the readiness listener installed on every page.
 *
 * `TrunkApplicationStarted` is dispatched once, the moment wasm boots, which is
 * usually before a test has had any chance to subscribe. An init script runs
 * before any of the page's own scripts on every navigation, so the flag it sets
 * is already true by the time anybody asks — turning a race into a poll.
 */
export const test = base.extend({
  page: async ({ page }, use) => {
    await page.addInitScript(() => {
      (window as unknown as Record<string, unknown>).__automateStarted = false;
      window.addEventListener("TrunkApplicationStarted", () => {
        (window as unknown as Record<string, unknown>).__automateStarted = true;
      });
    });

    await use(page);
  },
});

export { expect };

/**
 * Waits until the wasm application has booted and rendered.
 *
 * Deliberately not `networkidle`. The bundle streams several megabytes and the
 * application keeps talking to the API after it has painted, so "the network
 * went quiet" is both later than readiness and, when a poll is in flight, a
 * moment that may never arrive. The application says when it has started; this
 * listens for it.
 */
export async function waitForApp(page: Page): Promise<void> {
  await page.waitForFunction(
    () => (window as unknown as Record<string, unknown>).__automateStarted === true,
    undefined,
    { timeout: 45_000 },
  );
}

/** Navigates to a path within the application and waits for it to boot. */
export async function gotoApp(page: Page, path: string): Promise<void> {
  await page.goto(path);
  await waitForApp(page);
}

/**
 * A name no other test could produce.
 *
 * Every test shares one database, so a record left behind by a failed run must
 * not be able to satisfy — or break — a later one. A test that asserts "the
 * workflow I just made is in the list" is only telling the truth if the name it
 * looks for could not have come from anywhere else.
 */
export function uniqueName(prefix: string): string {
  return `${prefix} ${Math.random().toString(36).slice(2, 8)}${Date.now().toString(36).slice(-4)}`;
}

/** What the API returns when a service is linked. */
export interface Connection {
  id: string;
  name: string;
  provider: string;
}

/**
 * Links a service directly through the API.
 *
 * Most of the workflow tests need a Todoist account to exist because the
 * workflow types they create insist on one; none of them are testing the act of
 * linking it. Driving the connections form for a precondition would make every
 * one of those tests fail when the connections page breaks, which is what the
 * connections spec is for.
 */
export async function createConnection(
  request: APIRequestContext,
  options: { provider?: string; name: string; token?: string } ,
): Promise<Connection> {
  const response = await request.post("/api/v1/connections", {
    data: {
      provider: options.provider ?? "todoist",
      name: options.name,
      key: options.token ?? `e2e-token-${Math.random().toString(36).slice(2)}`,
    },
  });

  expect(
    response.status(),
    `linking ${options.provider ?? "todoist"} should have succeeded: ${await response.text()}`,
  ).toBe(201);

  return (await response.json()) as Connection;
}

/** Unlinks a service, tolerating one that has already gone. */
export async function deleteConnection(
  request: APIRequestContext,
  id: string,
): Promise<void> {
  const response = await request.delete(`/api/v1/connections/${id}`);
  expect([200, 204, 404]).toContain(response.status());
}

/** The services currently linked. */
export async function listConnections(
  request: APIRequestContext,
): Promise<Connection[]> {
  const response = await request.get("/api/v1/connections");
  expect(response.status()).toBe(200);
  return (await response.json()) as Connection[];
}

/** What the API returns for a configured workflow. */
export interface Workflow {
  id: string;
  name: string;
  type_id: string;
  enabled: boolean;
  webhook_path: string | null;
}

/** The workflows currently configured. */
export async function listWorkflows(
  request: APIRequestContext,
): Promise<Workflow[]> {
  const response = await request.get("/api/v1/workflows");
  expect(response.status()).toBe(200);
  return (await response.json()) as Workflow[];
}

/**
 * Removes every workflow whose name contains `fragment`.
 *
 * A safety net for the tests that delete through the UI: if the assertion
 * before the deletion fails, the record would otherwise survive into the next
 * run. Names are unique per test, so this can only ever reach that test's own.
 */
export async function purgeWorkflowsNamed(
  request: APIRequestContext,
  fragment: string,
): Promise<void> {
  for (const workflow of await listWorkflows(request)) {
    if (workflow.name.includes(fragment)) {
      await request.delete(`/api/v1/workflows/${workflow.id}`);
    }
  }
}

/** Removes every connection whose name contains `fragment`. */
export async function purgeConnectionsNamed(
  request: APIRequestContext,
  fragment: string,
): Promise<void> {
  for (const connection of await listConnections(request)) {
    if (connection.name.includes(fragment)) {
      await request.delete(`/api/v1/connections/${connection.id}`);
    }
  }
}

/**
 * Sets a `Switch` to a given state.
 *
 * The checkbox itself is a one-pixel transparent square — the control a person
 * actually sees and clicks is the `.switch__track` span beside it, and the
 * browser's label activation forwards that click to the input behind it. Going
 * through the track is therefore both what a user does and the only thing that
 * passes an actionability check.
 */
export async function setSwitch(input: Locator, checked: boolean): Promise<void> {
  if ((await input.isChecked()) === checked) {
    return;
  }

  await input
    .locator("xpath=following-sibling::span[contains(@class, 'switch__track')]")
    .click();

  await expect(input).toBeChecked({ checked });
}

/** The shape of a webhook ingress URL: a fixed path and 22 base64url characters. */
export const WEBHOOK_URL = /^https?:\/\/[^/]+\/webhooks\/w\/[A-Za-z0-9_-]{22}$/;
