import type { Page } from "@playwright/test";

function required(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is required for the console e2e suite`);
  return v;
}

export const operatorToken = (): string => required("AURIX_NET_E2E_TOKEN");

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

/** Same-origin API call with the operator token, for fixtures the UI does not create itself. */
export async function api<T>(page: Page, method: string, path: string, body?: unknown): Promise<T> {
  const res = await page.request.fetch(path, {
    method,
    headers: { authorization: `Bearer ${operatorToken()}`, "content-type": "application/json" },
    data: body === undefined ? undefined : JSON.stringify(body),
  });
  if (!res.ok()) throw new Error(`${method} ${path} → ${res.status()} ${await res.text()}`);
  return res.status() === 204 ? (undefined as T) : ((await res.json()) as T);
}
