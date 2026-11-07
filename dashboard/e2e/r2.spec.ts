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
  await expect(page.getByText("Suspended", { exact: true }).first()).toBeVisible();
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

test("overview attention panel lists a suspended project", async ({ page }) => {
  const name = uniq("e2e-att");
  const project = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name });
  await api(page, "POST", `/v1/operator/projects/${project.project.id}/suspend`, { suspended: true });
  await page.goto("/");
  await expect(page.getByText("Needs a look")).toBeVisible();
  await expect(page.getByRole("link", { name })).toBeVisible({ timeout: 20_000 });
});

test("operator sees the live matchmaking queue of a project", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-mm") });
  const filed = await page.request.post("/v1/matchmaking/tickets", {
    headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
    data: { player_id: "waiter-1", max_players: 8, min_players: 4, max_wait_secs: 120 },
  });
  expect(filed.ok()).toBeTruthy();
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("tab", { name: /Matchmaking/ }).click();
  await expect(page.getByRole("row").filter({ hasText: "waiter-1" })).toBeVisible();
  await expect(page.getByText("Searching now")).toBeVisible();
});

test("operator looks up a player and bans from the lookup", async ({ page }) => {
  const project = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-lookup") });
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("tab", { name: /Players/ }).click();
  await page.getByLabel("Player id").fill("ghost-7");
  await page.getByRole("button", { name: "Find" }).click();
  await expect(page.getByText("Not in any live room on any node.")).toBeVisible();
  await expect(page.getByText("Not banned")).toBeVisible();
  await page.getByRole("button", { name: "Ban player" }).click();
  await expect(page.getByText("Banned", { exact: true })).toBeVisible();
  await page.getByRole("tab", { name: /Bans/ }).click();
  await expect(page.getByRole("row").filter({ hasText: "ghost-7" })).toBeVisible();
});

test("operator exports fleet usage as CSV", async ({ page }) => {
  await page.goto("/usage");
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export CSV" }).click()]);
  expect(download.suggestedFilename()).toMatch(/^aurix-net-usage-fleet-\d{8}-\d{8}\.csv$/);
  const path = await download.path();
  expect(path).toBeTruthy();
  const fs = await import("node:fs/promises");
  const text = await fs.readFile(path, "utf8");
  expect(text.split("\n")[0]).toBe("hour,metric,quantity");
});

test("operator rotates a webhook secret from the project page", async ({ page }) => {
  const project = await api<{ project: { id: string }; key: { key: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-rot") });
  const created = await page.request.post("/v1/webhooks", {
    headers: { authorization: `Bearer ${project.key.key}`, "content-type": "application/json" },
    data: { url: "http://127.0.0.1:9/rotate-me", events: [] },
  });
  expect(created.ok()).toBeTruthy();
  const oldSecret = ((await created.json()) as { secret: string }).secret;
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("tab", { name: /Webhooks/ }).click();
  await page.getByRole("button", { name: "Rotate secret" }).first().click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Old secret keeps signing for").selectOption("1");
  await dialog.getByRole("button", { name: "Rotate secret" }).click();
  await expect(page.getByRole("dialog").getByText(/New signing secret/)).toBeVisible();
  const shown = await page.getByRole("dialog").locator("pre, code").first().innerText();
  expect(shown.trim()).not.toBe(oldSecret);
  expect(shown.trim().length).toBeGreaterThan(20);
});

test("operator opens live stats of a node", async ({ page }) => {
  await page.goto("/nodes");
  await page.getByRole("button", { name: "Live stats" }).first().click();
  const dialog = page.getByRole("dialog");
  await expect(dialog.getByText("Control sessions")).toBeVisible();
  await expect(dialog.getByText("Memory (RSS)")).toBeVisible();
  await expect(dialog.getByText(/\d+ MB/)).toBeVisible();
});

test("operator issues a named token on Settings, it authenticates and is named in the audit log, then revokes it", async ({ page }) => {
  const name = uniq("ci-bot");
  await page.goto("/settings");
  await page.getByRole("button", { name: "New token" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name").fill(name);
  await dialog.getByRole("button", { name: "Create" }).click();
  const token = (await page.getByRole("dialog").locator("pre, code").first().innerText()).trim();
  expect(token.length).toBeGreaterThan(20);
  await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).last().click();
  const me = await page.request.get("/v1/me", { headers: { authorization: `Bearer ${token}` } });
  expect(me.ok()).toBeTruthy();
  expect(((await me.json()) as { actor: string }).actor).toBe(`op:${name}`);
  const audit = await page.request.get("/v1/operator/audit?action=operator_token.create", { headers: { authorization: `Bearer ${token}` } });
  expect(((await audit.json()) as { audit: { details: { name: string } }[] }).audit.some((a) => a.details.name === name)).toBeTruthy();
  const row = page.getByRole("row").filter({ hasText: name });
  await row.getByRole("button", { name: "Revoke" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Revoke" }).click();
  await expect(row.getByText("Revoked")).toBeVisible();
  const after = await page.request.get("/v1/me", { headers: { authorization: `Bearer ${token}` } });
  expect(after.status()).toBe(401);
});

test("operator broadcasts a notice to a project from its page", async ({ page }) => {
  const project = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-notice") });
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("button", { name: "Broadcast" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Message").fill("Maintenance in 5 minutes");
  await dialog.getByLabel("Level").selectOption("warn");
  await dialog.getByRole("button", { name: "Send" }).click();
  await expect(page.getByText(/Sent to 0 player\(s\) in 0 room\(s\)/)).toBeVisible();
  const audit = await api<{ audit: { action: string; details: { text: string } }[] }>(page, "GET", `/v1/operator/audit?action=project.notice&project=${project.project.id}`);
  expect(audit.audit.some((a) => a.details.text === "Maintenance in 5 minutes")).toBeTruthy();
});

test("operator exports the audit log as CSV", async ({ page }) => {
  await page.goto("/audit");
  const [download] = await Promise.all([page.waitForEvent("download"), page.getByRole("button", { name: "Export CSV" }).click()]);
  expect(download.suggestedFilename()).toMatch(/^aurix-net-audit-.*\.csv$/);
  const fs = await import("node:fs/promises");
  const text = await fs.readFile((await download.path()), "utf8");
  expect(text.split("\n")[0]).toBe("id,created_at,project_id,actor,action,target,details");
});

test("operator issues a test grant from the project page", async ({ page }) => {
  const project = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-grant") });
  await page.goto(`/projects/${project.project.id}`);
  await expect(page.getByTestId("project-health")).toContainText("Active");
  await page.getByRole("button", { name: "Test grant" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Player ID").fill("qa-player");
  await dialog.getByRole("button", { name: "Issue" }).click();
  await expect(dialog.getByTestId("test-grant")).not.toBeEmpty();
  const audit = await api<{ audit: { action: string; target: string }[] }>(page, "GET", `/v1/operator/audit?action=grant.issue&project=${project.project.id}`);
  expect(audit.audit.some((a) => a.target === "qa-player")).toBeTruthy();
});

test("operator sets a monthly player-minutes cap and the health strip shows it", async ({ page }) => {
  const project = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-cap") });
  await page.goto(`/projects/${project.project.id}`);
  await page.getByLabel("Monthly cap: player-minutes").fill("100");
  await page.getByRole("button", { name: "Save quotas" }).click();
  await expect(page.getByTestId("project-health")).toContainText("0 / 100");
  const detail = await api<{ effective_limits: { monthly_player_minutes: number | null } }>(page, "GET", `/v1/operator/projects/${project.project.id}`);
  expect(detail.effective_limits.monthly_player_minutes).toBe(100);
});

test("operator issues a key restricted to networks and the table shows it", async ({ page }) => {
  const project = await api<{ project: { id: string } }>(page, "POST", "/v1/operator/projects", { name: uniq("e2e-cidr") });
  await page.goto(`/projects/${project.project.id}`);
  await page.getByRole("button", { name: "Issue key" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("Name").fill("backend");
  await dialog.getByLabel("Allowed IPs").fill("203.0.113.0/24, 198.51.100.7");
  await dialog.getByRole("button", { name: "Create" }).click();
  await dialog.getByRole("button", { name: "Close", exact: true }).last().click();
  await expect(page.getByRole("row", { name: /backend/ })).toContainText("2 network(s)");
});

test("operator adds and removes a fleet alert webhook", async ({ page }) => {
  await page.goto("/settings");
  await page.getByRole("button", { name: "New alert webhook" }).click();
  const dialog = page.getByRole("dialog");
  await dialog.getByLabel("URL").fill("http://127.0.0.1:9/e2e-alert");
  await dialog.getByLabel("node.stale").check();
  await dialog.getByRole("button", { name: "Create" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).last().click();
  const row = page.getByRole("row", { name: /e2e-alert/ });
  await expect(row).toContainText("node.stale");
  await row.getByRole("button", { name: "Delete" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Delete" }).click();
  await expect(page.getByRole("row", { name: /e2e-alert/ })).toHaveCount(0);
});

test("operator sees the month billing table on the fleet usage page", async ({ page }) => {
  const name = uniq("e2e-bill");
  await api(page, "POST", "/v1/operator/projects", { name });
  await page.goto("/usage");
  const card = page.getByTestId("billing");
  await expect(card).toContainText(name);
  await expect(card).toContainText("Fleet total");
});
