import { useSyncExternalStore } from "react";

import type { Me } from "@/api/client";

/** Signed in with either the operator token (fleet) or a project API key (one project). */
export interface AuthSession {
  token: string;
  me: Me;
}

const STORAGE_KEY = "aurix-net.auth";
let current: AuthSession | null = load();
const listeners = new Set<() => void>();

function load(): AuthSession | null {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<AuthSession>;
    if (typeof parsed.token !== "string" || !parsed.me || (parsed.me.kind !== "operator" && parsed.me.kind !== "project")) return null;
    return { token: parsed.token, me: parsed.me };
  } catch {
    return null;
  }
}

function persist(): void {
  try {
    if (current) localStorage.setItem(STORAGE_KEY, JSON.stringify(current));
    else localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* ignore */
  }
}

function emit(): void {
  for (const l of listeners) l();
}

export const authStore = {
  get: (): AuthSession | null => current,
  set: (s: AuthSession): void => {
    current = s;
    persist();
    emit();
  },
  clear: (): void => {
    current = null;
    persist();
    emit();
  },
  subscribe: (l: () => void): (() => void) => {
    listeners.add(l);
    return () => {
      listeners.delete(l);
    };
  },
};

export function useAuthSession(): AuthSession | null {
  return useSyncExternalStore(authStore.subscribe, authStore.get, authStore.get);
}

/** Scopes the session can use on `/v1`: the operator has every project scope through its own routes. */
export function hasScope(me: Me | null | undefined, scope: "rooms" | "read" | "admin"): boolean {
  if (!me) return false;
  if (me.kind === "operator") return scope !== "rooms";
  return me.key?.scopes.includes(scope) ?? false;
}
