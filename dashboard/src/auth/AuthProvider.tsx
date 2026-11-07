import { useQueryClient } from "@tanstack/react-query";
import { createContext, useCallback, useContext, useEffect, useMemo, useRef, type ReactNode, useState } from "react";

import { NetClient, NetError, type Me } from "@/api/client";

import { authStore, hasScope, useAuthSession, type AuthSession } from "./store";

export type AuthStatus = "anonymous" | "authenticated";

interface AuthCtx {
  status: AuthStatus;
  session: AuthSession | null;
  me: Me | null;
  token: string | null;
  /** True when signed in with the operator token. */
  operator: boolean;
  /** Project id in scope: the key's project, or the operator's selected project. */
  projectId: string | null;
  selectProject: (id: string | null) => void;
  signIn: (token: string) => Promise<Me>;
  signOut: (reason?: "expired" | "manual") => void;
  can: (scope: "rooms" | "read" | "admin") => boolean;
  lastSignOutReason: "expired" | "manual" | null;
}

const Ctx = createContext<AuthCtx | null>(null);
const PROJECT_KEY = "aurix-net.project";

export function AuthProvider({ children }: { children: ReactNode }) {
  const session = useAuthSession();
  const qc = useQueryClient();
  const [lastSignOutReason, setLastSignOutReason] = useState<"expired" | "manual" | null>(null);
  const [selected, setSelected] = useState<string | null>(() => {
    try {
      return localStorage.getItem(PROJECT_KEY);
    } catch {
      return null;
    }
  });

  const signOut = useCallback(
    (reason: "expired" | "manual" = "manual") => {
      setLastSignOutReason(reason);
      authStore.clear();
      qc.clear();
    },
    [qc],
  );

  const signIn = useCallback(async (token: string) => {
    const me = await new NetClient(token).get<Me>("/v1/me");
    authStore.set({ token, me });
    setLastSignOutReason(null);
    return me;
  }, []);

  // Re-validate a restored token once per page load; a revoked key signs out.
  const validated = useRef<string | null>(null);
  useEffect(() => {
    if (!session || validated.current === session.token) return;
    validated.current = session.token;
    new NetClient(session.token)
      .get<Me>("/v1/me")
      .then((me) => authStore.set({ token: session.token, me }))
      .catch((err: unknown) => {
        if (err instanceof NetError && err.status === 401) signOut("expired");
      });
  }, [session, signOut]);

  const selectProject = useCallback((id: string | null) => {
    setSelected(id);
    try {
      if (id) localStorage.setItem(PROJECT_KEY, id);
      else localStorage.removeItem(PROJECT_KEY);
    } catch {
      /* ignore */
    }
  }, []);

  const me = session?.me ?? null;
  const operator = me?.kind === "operator";
  const value = useMemo<AuthCtx>(
    () => ({
      status: session ? "authenticated" : "anonymous",
      session,
      me,
      token: session?.token ?? null,
      operator,
      projectId: operator ? selected : (me?.project?.id ?? null),
      selectProject,
      signIn,
      signOut,
      can: (scope) => hasScope(me, scope),
      lastSignOutReason,
    }),
    [session, me, operator, selected, selectProject, signIn, signOut, lastSignOutReason],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useAuth(): AuthCtx {
  const ctx = useContext(Ctx);
  if (!ctx) throw new Error("useAuth outside AuthProvider");
  return ctx;
}
