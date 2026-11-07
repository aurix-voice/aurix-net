import { expect, test } from "@playwright/test";

import { api, pinEnglish, uniq } from "./env";

test.beforeEach(async ({ page }) => pinEnglish(page));

test("operator issues and revokes a project key; the audit log records it", async ({ page }) => {
  const name = uniq("e2e-keys");
  const created = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name });
  await page.goto(`/projects/${created.project.id}`);
  await page.getByRole("button", { name: "Issue key" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name").fill("console-e2e");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog.getByText(/^an_/)).toBeVisible();
  await dialog.getByRole("button", { name: "Close", exact: true }).last().click();
  await expect(page.getByRole("row", { name: /console-e2e/ })).toBeVisible();

  await page.goto("/audit");
  await expect(page.getByRole("row").filter({ hasText: "key.create" }).first()).toBeVisible();
});

test.describe("project API key session", () => {
  test.use({ storageState: { cookies: [], origins: [] } });

  test("a project API key signs in with project scope only", async ({ page }) => {
    const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-scope") });
    await page.goto("/login");
    await page.getByLabel("Token").fill(project.key.key);
    await page.getByRole("button", { name: "Sign in" }).click();
    await page.waitForURL((u) => !u.pathname.startsWith("/login"));
    await expect(page.getByRole("link", { name: "API keys" })).toBeVisible();
    await expect(page.getByRole("link", { name: "Projects" })).toHaveCount(0);
    await page.goto("/projects");
    await expect(page.getByText(/operator/i).first()).toBeVisible();
  });
});
