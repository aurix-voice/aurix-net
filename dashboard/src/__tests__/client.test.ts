import { afterEach, describe, expect, it, vi } from "vitest";

import { NetClient, NetError, NetNetworkError, errorKind } from "@/api/client";

vi.stubGlobal("window", { location: { origin: "http://console.test" } });

function respond(status: number, body: unknown) {
  return new Response(status === 204 ? null : JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

describe("NetClient", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("sends the bearer token and drops empty query params", async () => {
    const fetchMock = vi.fn().mockResolvedValue(respond(200, { ok: true }));
    vi.stubGlobal("fetch", fetchMock);
    vi.stubGlobal("window", { location: { origin: "http://console.test" } });
    const out = await new NetClient("s3cret").get<{ ok: boolean }>("/v1/operator/audit", { limit: 5, action: "", project: undefined });
    expect(out.ok).toBe(true);
    const [url, init] = fetchMock.mock.calls[0] as [URL, RequestInit];
    expect(url.toString()).toBe("http://console.test/v1/operator/audit?limit=5");
    expect((init.headers as Record<string, string>).authorization).toBe("Bearer s3cret");
  });

  it("maps API errors to NetError with the server code", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(respond(401, { error: "unauthorized", message: "malformed api key" })));
    vi.stubGlobal("window", { location: { origin: "http://console.test" } });
    const err = await new NetClient(null).get("/v1/me").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(NetError);
    expect((err as NetError).status).toBe(401);
    expect((err as NetError).code).toBe("unauthorized");
    expect(errorKind(err)).toBe("unauthorized");
  });

  it("returns undefined for 204 and wraps transport failures", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(respond(204, null)));
    vi.stubGlobal("window", { location: { origin: "http://console.test" } });
    await expect(new NetClient("t").del("/v1/webhooks/x")).resolves.toBeUndefined();
    vi.stubGlobal("fetch", vi.fn().mockRejectedValue(new TypeError("Failed to fetch")));
    const err = await new NetClient("t").get("/healthz").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(NetNetworkError);
    expect(errorKind(err)).toBe("network");
  });
});
