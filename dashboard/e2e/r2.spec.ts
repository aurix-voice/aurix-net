import { expect, test } from "@playwright/test";

import { api, pinEnglish, uniq } from "./env";

test.beforeEach(async ({ page }) => pinEnglish(page));

test("usage shows the whole fleet for an operator without a selected project", async ({ page }) => {
  await page.goto("/usage");
  await expect(page.getByRole("heading", { name: "Usage · all projects" })).toBeVisible();
  await expect(page.getByText("Room-minutes").first()).toBeVisible();
});

test("overview renders the last-24-hours usage chart", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Last 24 hours" })).toBeVisible();
});

test("nodes page shows region cards and links a node to its rooms", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-r2") });
  const roomName = uniq("r2room");
  const res = await page.request.post("/v1/rooms", {
    headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
    data: { name: roomName, mode: "relay", max_players: 4 },
  });
  expect(res.ok()).toBeTruthy();
  const room = (await res.json()) as { id: string; node: string };
  await page.goto("/nodes");
  await expect(page.getByText(/\d+ live/).first()).toBeVisible();
  await page.goto(`/rooms?node=${room.node}`);
  await expect(page.getByLabel("Node")).toHaveValue(room.node);
  await expect(page.getByText(roomName)).toBeVisible();
  await page.getByLabel("Mode").selectOption("relay");
  await expect(page.getByText(roomName)).toBeVisible();
  await page.goto("/rooms?node=00000000-0000-0000-0000-000000000000");
  await expect(page.getByText("No live rooms")).toBeVisible();
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page.getByText(roomName)).toBeVisible();
  await api(page, "DELETE", `/v1/operator/rooms/${room.id}`);
});

test("audit log filters by action prefix and actor", async ({ page }) => {
  const name = uniq("e2e-audit");
  await api(page, "POST", "/v1/operator/projects", { name });
  await page.goto("/audit");
  await page.getByPlaceholder(/^Action/).fill("project.");
  await expect(page.getByRole("row").filter({ hasText: "project.create" }).first()).toBeVisible();
  await expect(page.getByRole("row").filter({ hasText: "key.create" })).toHaveCount(0);
  await page.getByPlaceholder(/^Actor/).fill("nobody-such");
  await expect(page.getByRole("row").filter({ hasText: "project.create" })).toHaveCount(0);
  await page.getByRole("button", { name: "Clear filters" }).click();
  await expect(page.getByRole("row").filter({ hasText: "project.create" }).first()).toBeVisible();
});

test("project page has a Connect card and lets the operator delete a webhook", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-hook") });
  const res = await page.request.post("/v1/webhooks", {
    headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
    data: { url: "http://127.0.0.1:9/hook", events: ["room.created"] },
  });
  expect(res.ok()).toBeTruthy();
  await page.goto(`/projects/${project.project.id}`);
  await expect(page.getByRole("heading", { name: "Connect" })).toBeVisible();
  await expect(page.getByText("Issue a join grant")).toBeVisible();
  await expect(page.locator("pre")).not.toContainText(project.key.key);
  await page.getByRole("tab", { name: /Webhooks/ }).click();
  await expect(page.getByText("http://127.0.0.1:9/hook")).toBeVisible();
  await page.getByRole("button", { name: "Delete" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByText("http://127.0.0.1:9/hook")).toHaveCount(0);
  await page.goto("/audit");
  await page.getByPlaceholder(/^Action/).fill("webhook.delete");
  await expect(page.getByRole("row").filter({ hasText: "webhook.delete" }).first()).toBeVisible();
});

test("operator can suspend and resume a project; suspended keys cannot issue grants", async ({ page }) => {
  const name = uniq("e2e-susp");
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name });
  const grant = () =>
    page.request.post("/v1/grants", {
      headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
      data: { player_id: "p1", name: "P" },
    });
  expect((await grant()).status()).toBe(200);
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("button", { name: "Suspend" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Suspend" }).click();
  await expect(page.getByText("Suspended", { exact: true })).toBeVisible();
  await expect(page.getByText(/This project is suspended since/)).toBeVisible();
  expect((await grant()).status()).toBe(403);
  await page.goto("/projects");
  await expect(page.getByRole("row", { name: new RegExp(name) }).getByText("Suspended")).toBeVisible();
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("button", { name: "Resume" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Resume" }).click();
  await expect(page.getByText(/This project is suspended since/)).toHaveCount(0);
  expect((await grant()).status()).toBe(200);
  await page.goto("/audit");
  await page.getByPlaceholder(/^Action/).fill("project.suspend");
  await expect(page.getByRole("row").filter({ hasText: "project.suspend" }).first()).toBeVisible();
});
