import { beforeEach, describe, expect, it, vi } from "vitest";

const mem = new Map<string, string>();
vi.stubGlobal("localStorage", {
  getItem: (k: string) => mem.get(k) ?? null,
  setItem: (k: string, v: string) => void mem.set(k, v),
  removeItem: (k: string) => void mem.delete(k),
  clear: () => mem.clear(),
});

import type { Me } from "@/api/client";

const { authStore } = await import("@/auth/store");

const me: Me = { kind: "operator", region: "local", node_id: "n1" } as unknown as Me;

describe("authStore", () => {
  beforeEach(() => {
    authStore.clear();
    localStorage.clear();
  });

  it("persists a session to local storage and clears it on sign-out", () => {
    authStore.set({ token: "t0k", me });
    expect(authStore.get()?.token).toBe("t0k");
    const raw = localStorage.getItem("aurix-net.auth");
    expect(raw).not.toBeNull();
    expect(JSON.parse(raw!)).toMatchObject({ token: "t0k", me: { kind: "operator" } });
    authStore.clear();
    expect(authStore.get()).toBeNull();
    expect(localStorage.getItem("aurix-net.auth")).toBeNull();
  });

  it("notifies subscribers once per change and stops after unsubscribe", () => {
    const l = vi.fn();
    const off = authStore.subscribe(l);
    authStore.set({ token: "a", me });
    authStore.clear();
    expect(l).toHaveBeenCalledTimes(2);
    off();
    authStore.set({ token: "b", me });
    expect(l).toHaveBeenCalledTimes(2);
  });
});
