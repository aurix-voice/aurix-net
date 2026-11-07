import { expect, test } from "@playwright/test";

import { api, cleanupProjects, pinEnglish, uniq } from "./env";

test.beforeEach(async ({ page }) => pinEnglish(page));
test.afterEach(async ({ page }) => cleanupProjects(page));

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
  await page.getByRole("button", { name: "Delete", exact: true }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete", exact: true }).click();
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

test("operator can drain another node from the console and put it back", async ({ page }) => {
  const me = await api<{ node_id: string }>(page, "GET", "/v1/me");
  const { nodes } = await api<{ nodes: { id: string; version: string; public_host: string; quic_port: number; draining: boolean; last_heartbeat: string }[] }>(page, "GET", "/v1/operator/nodes");
  const self = nodes.find((n) => n.id === me.node_id);
  const other = nodes.find((n) => n.id !== me.node_id && n.version === self?.version && !n.draining && Date.now() - Date.parse(n.last_heartbeat) < 30_000);
  test.skip(!other, "needs a second live node");
  const row = page.getByRole("row", { name: new RegExp(`${other!.public_host}:${other!.quic_port}`) });
  await page.goto("/nodes");
  await row.getByRole("button", { name: "Drain" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Drain" }).click();
  await expect(row.getByText("Draining")).toBeVisible();
  await row.getByRole("button", { name: "Back in rotation" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Back in rotation" }).click();
  await expect(row.getByText("Draining")).toHaveCount(0);
  await page.goto("/audit");
  await page.getByPlaceholder(/^Action/).fill("node.");
  await expect(page.getByRole("row").filter({ hasText: "node.drain" }).first()).toBeVisible();
});

test("operator deletes a project after typing its name; keys stop working and history stays", async ({ page }) => {
  const name = uniq("e2e-del");
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name });
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("button", { name: "Delete project" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByRole("button", { name: "Delete project" })).toBeDisabled();
  await dialog.getByLabel("Name").fill(name);
  await dialog.getByRole("button", { name: "Delete project" }).click();
  await expect(page).toHaveURL(/\/projects$/);
  await expect(page.getByRole("row", { name: new RegExp(name) })).toHaveCount(0);
  const res = await page.request.get("/v1/rooms", { headers: { authorization: `Bearer ${project.key.key}` } });
  expect(res.status()).toBe(401);
  await page.goto("/audit");
  await page.getByPlaceholder(/^Action/).fill("project.delete");
  await expect(page.getByRole("row").filter({ hasText: project.project.id.slice(0, 8) }).first()).toBeVisible();
});

test("webhook deliveries dialog shows the outcome of a test send", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-deliv") });
  const res = await page.request.post("/v1/webhooks", {
    headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
    data: { url: "http://127.0.0.1:9/dead", events: [] },
  });
  expect(res.ok()).toBeTruthy();
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("tab", { name: /Webhooks/ }).click();
  await page.getByRole("button", { name: "Deliveries" }).click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("No deliveries yet")).toBeVisible();
  await dialog.getByRole("button", { name: "Send test" }).click();
  await expect(dialog.getByRole("row").filter({ hasText: "test" }).first()).toBeVisible({ timeout: 20_000 });
  await expect(dialog.getByText("failed").first()).toBeVisible();
});

test("project page counts rejected grants while the project is suspended", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-rej") });
  await api(page, "POST", `/v1/operator/projects/${project.project.id}/suspend`, { suspended: true });
  for (let i = 0; i < 2; i++) {
    const res = await page.request.post("/v1/grants", {
      headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
      data: { player_id: "p1", name: "P" },
    });
    expect(res.status()).toBe(403);
  }
  await page.goto(`/projects/${project.project.id}`);
  await expect(page.getByText(/Rejections · last 24 h/)).toBeVisible();
  const row = page.locator("dl div").filter({ hasText: "Project suspended" });
  await expect(row.locator("dd")).toHaveText("2", { timeout: 15_000 });
});

test("operator bans a player from the project page; grants for that id are refused until lifted", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-ban") });
  const grant = () =>
    page.request.post("/v1/grants", {
      headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
      data: { player_id: "cheater-1", name: "C" },
    });
  expect((await grant()).status()).toBe(200);
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("tab", { name: /Bans/ }).click();
  await page.getByRole("button", { name: "Ban player" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByRole("textbox").nth(0).fill("cheater-1");
  await dialog.getByRole("textbox").nth(1).fill("speedhack");
  await dialog.getByRole("button", { name: "Ban player" }).click();
  await expect(page.getByRole("row").filter({ hasText: "cheater-1" })).toBeVisible();
  const refused = await grant();
  expect(refused.status()).toBe(403);
  expect(await refused.text()).toContain("speedhack");
  await page.getByRole("row").filter({ hasText: "cheater-1" }).getByRole("button", { name: "Lift" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Lift" }).click();
  await expect(page.getByRole("row").filter({ hasText: "cheater-1" })).toHaveCount(0);
  expect((await grant()).status()).toBe(200);
});

test("operator issues a key with an expiry and the table shows it", async ({ page }) => {
  const project = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-exp") });
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("button", { name: "Issue key" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name").fill("short-lived");
  await dialog.getByLabel("Expires").selectOption("30");
  await dialog.getByRole("button", { name: "Create" }).click();
  await expect(dialog.getByText(/an_/)).toBeVisible();
  await dialog.getByRole("button", { name: "Close", exact: true }).last().click();
  const row = page.getByRole("row").filter({ hasText: "short-lived" });
  await expect(row).toBeVisible();
  await expect(row.getByText("Never")).toHaveCount(0);
  await expect(page.getByRole("row").filter({ hasText: "default" }).getByText("Never")).toBeVisible();
});
