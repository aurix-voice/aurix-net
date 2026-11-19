import { expect, type Page } from "@playwright/test";

function required(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is required for the console e2e suite`);
  return v;
}

export const operatorToken = (): string => required("AURIX_NET_E2E_TOKEN");

/** `http_url` of the second node of the stand under test, for specs that act on another node.
 * The node list also holds whatever else shares the stand's database for a moment. */
export const peerNodeUrl = (): string | undefined => process.env.AURIX_NET_E2E_PEER;

/** With NET_LIVE_REQUIRED set, a spec the stand cannot run fails instead of being skipped. */
export const liveRequired = !!process.env.NET_LIVE_REQUIRED;

/** API address of the Toxiproxy the stand's node reaches Redis through, for the spec that takes
 * Redis away. */
export function redisProxyUrl(): string | undefined {
  const url = process.env.AURIX_NET_E2E_TOXIPROXY;
  if (!url && liveRequired) throw new Error("NET_LIVE_REQUIRED is set and AURIX_NET_E2E_TOXIPROXY is not");
  return url;
}

/** Takes Redis away from the node or gives it back: Toxiproxy's proxy `redis`. Its API refuses a
 * browser's user agent, so the request does not go through the page. */
export async function setRedis(up: boolean): Promise<void> {
  const res = await fetch(`${redisProxyUrl()}/proxies/redis`, { method: "PATCH", body: JSON.stringify({ enabled: up }) });
  if (!res.ok) throw new Error(`PATCH /proxies/redis → ${res.status} ${await res.text()}`);
}

export const OPERATOR_STATE = "e2e/.auth/operator.json";

/** Short unique suffix so re-runs against the same node never collide on names. */
export function uniq(prefix: string): string {
  return `${prefix}-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 6)}`;
}

/** Pins the UI language so text selectors are stable regardless of the browser locale. */
export async function pinEnglish(page: Page): Promise<void> {
  await page.addInitScript(() => {
    try {
      localStorage.setItem("aurix.locale", "en");
      localStorage.setItem("aurix.theme", "light");
    } catch {
      /* storage disabled */
    }
  });
}

export async function signIn(page: Page, token: string): Promise<void> {
  await page.goto("/login");
  await page.getByLabel("Token").fill(token);
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.waitForURL((u) => !u.pathname.startsWith("/login"));
}

/** Clicks `name` in the open dialog and waits for the dialog to go. It stays open while its
 * request is in flight, and role selectors do not see the page behind it: a check for a row
 * that is gone would pass before the request has done anything. */
export async function confirm(page: Page, name: string): Promise<void> {
  await page.getByRole("dialog").getByRole("button", { name, exact: true }).click();
  await expect(page.getByRole("dialog")).toHaveCount(0);
}

/** Closes the dialog that shows a new secret once. It opens when the create request is back;
 * until then the form is still there, with a Close button of its own. */
export async function closeSecret(page: Page): Promise<void> {
  const dialog = page.getByRole("dialog");
  await expect(dialog.locator("pre")).toBeVisible();
  await dialog.getByRole("button", { name: "Close", exact: true }).last().click();
}

const createdProjects: string[] = [];

/** Same-origin API call with the operator token, for fixtures the UI does not create itself.
 * Projects created through it are deleted again by `cleanupProjects` so the fleet stays tidy. */
export async function api<T>(page: Page, method: string, path: string, body?: unknown): Promise<T> {
  const res = await page.request.fetch(path, {
    method,
    headers: { authorization: `Bearer ${operatorToken()}`, "content-type": "application/json" },
    data: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok()) throw new Error(`${method} ${path} → ${res.status()} ${await res.text()}`);
  const out = res.status() === 204 ? (undefined as T) : ((await res.json()) as T);
  if (method === "POST" && path === "/v1/operator/projects") {
    const id = (out as { project?: { id?: string } }).project?.id;
    if (id) createdProjects.push(id);
  }
  return out;
}

/** Soft-deletes every project a test created (closes rooms, revokes keys). Safe to call twice. */
export async function cleanupProjects(page: Page): Promise<void> {
  const ids = createdProjects.splice(0);
  for (const id of ids) {
    await page.request.fetch(`/v1/operator/projects/${id}`, { method: "DELETE", headers: { authorization: `Bearer ${operatorToken()}` } }).catch(() => undefined);
  }
}
