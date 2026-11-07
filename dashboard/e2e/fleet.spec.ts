import { expect, test } from "@playwright/test";

import { api, pinEnglish, uniq } from "./env";

test.beforeEach(async ({ page }) => pinEnglish(page));

test("overview and nodes show the live fleet", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByText("This node", { exact: true })).toBeVisible();
  await expect(page.getByText("Ready")).toBeVisible();
  await page.getByRole("navigation").getByRole("link", { name: "Nodes" }).click();
  await expect(page.getByRole("heading", { name: "Nodes" })).toBeVisible();
  await expect(page.getByRole("row").filter({ hasText: "Live" }).first()).toBeVisible();
});

test("projects list opens project detail with keys and quotas", async ({ page }) => {
  const name = uniq("e2e");
  const created = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name, plan: "self_hosted" });
  await page.goto("/projects");
  await page.getByRole("row", { name: new RegExp(name) }).click();
  await expect(page).toHaveURL(new RegExp(`/projects/${created.project.id}`));
  await expect(page.getByRole("heading", { name })).toBeVisible();
  await expect(page.getByRole("tab", { name: /API keys/ })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Quotas" })).toBeVisible();
});

test("rooms list shows a live room and its detail", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-rooms") });
  const roomName = uniq("room");
  const res = await page.request.post("/v1/rooms", {
    headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
    data: { name: roomName, mode: "relay", max_players: 4 },
  });
  expect(res.ok()).toBeTruthy();
  const room = (await res.json()) as { id: string };
  await page.goto("/rooms");
  await expect(page.getByText(roomName)).toBeVisible();
  await page.getByText(roomName).click();
  await expect(page).toHaveURL(new RegExp(`/rooms/${room.id}`));
  await expect(page.getByText("relay")).toBeVisible();
  await expect(page.getByText("0 / 4")).toBeVisible();
  await page.goto("/rooms");
  await page.getByRole("row", { name: new RegExp(roomName) }).getByRole("button", { name: "Close room" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Close room" }).click();
  await expect(page.getByText("Room closed")).toBeVisible();
  await expect(page.getByText(roomName)).toHaveCount(0);
});

test("audit log lists operator mutations", async ({ page }) => {
  const name = uniq("e2e-audit");
  await api(page, "POST", "/v1/operator/projects", { name });
  await page.goto("/audit");
  await expect(page.getByRole("heading", { name: "Audit log" })).toBeVisible();
  await expect(page.getByRole("row").filter({ hasText: "project.create" }).first()).toBeVisible();
});
