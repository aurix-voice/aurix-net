import { createRootRoute, createRoute, createRouter, Outlet, redirect } from "@tanstack/react-router";

import { AuthLayout } from "@/auth/AuthLayout";
import { LoginPage } from "@/auth/LoginPage";
import { authStore } from "@/auth/store";
import { AuditPage } from "@/pages/Audit";
import { KeysPage } from "@/pages/Keys";
import { MatchmakingPage } from "@/pages/Matchmaking";
import { ModulesPage } from "@/pages/Modules";
import { NodesPage } from "@/pages/Nodes";
import { OverviewPage } from "@/pages/Overview";
import { ProjectDetailPage } from "@/pages/ProjectDetail";
import { ProjectsPage } from "@/pages/Projects";
import { RoomDetailPage } from "@/pages/RoomDetail";
import { RoomsPage } from "@/pages/Rooms";
import { UsagePage } from "@/pages/Usage";
import { WebhooksPage } from "@/pages/Webhooks";
import { NotFound } from "@/shell/NotFound";
import { Shell } from "@/shell/Shell";


const rootRoute = createRootRoute({ component: Outlet, notFoundComponent: NotFound });

const authRoute = createRoute({
  getParentRoute: () => rootRoute,
  id: "auth",
  component: AuthLayout,
  beforeLoad: () => {
    if (authStore.get()) throw redirect({ to: "/" });
  },
});
const loginRoute = createRoute({
  getParentRoute: () => authRoute,
  path: "/login",
  validateSearch: (s: Record<string, unknown>): { redirect?: string } => ({ redirect: typeof s.redirect === "string" ? s.redirect : undefined }),
  component: LoginPage,
});

const shellRoute = createRoute({ getParentRoute: () => rootRoute, id: "shell", component: Shell });
export const overviewRoute = createRoute({ getParentRoute: () => shellRoute, path: "/", component: OverviewPage });
export const nodesRoute = createRoute({ getParentRoute: () => shellRoute, path: "/nodes", component: NodesPage });
export const projectsRoute = createRoute({ getParentRoute: () => shellRoute, path: "/projects", component: ProjectsPage });
export const projectDetailRoute = createRoute({ getParentRoute: () => shellRoute, path: "/projects/$projectId", component: ProjectDetailPage });
export const roomsRoute = createRoute({
  getParentRoute: () => shellRoute,
  path: "/rooms",
  component: RoomsPage,
  validateSearch: (s: Record<string, unknown>): { node?: string; mode?: string; state?: string } => ({
    node: typeof s.node === "string" && s.node ? s.node : undefined,
    mode: typeof s.mode === "string" && s.mode ? s.mode : undefined,
    state: typeof s.state === "string" && s.state ? s.state : undefined,
  }),
});
export const roomDetailRoute = createRoute({ getParentRoute: () => shellRoute, path: "/rooms/$roomId", component: RoomDetailPage });
export const usageRoute = createRoute({ getParentRoute: () => shellRoute, path: "/usage", component: UsagePage });
export const keysRoute = createRoute({ getParentRoute: () => shellRoute, path: "/keys", component: KeysPage });
export const webhooksRoute = createRoute({ getParentRoute: () => shellRoute, path: "/webhooks", component: WebhooksPage });
export const modulesRoute = createRoute({ getParentRoute: () => shellRoute, path: "/modules", component: ModulesPage });
export const matchmakingRoute = createRoute({ getParentRoute: () => shellRoute, path: "/matchmaking", component: MatchmakingPage });
export const auditRoute = createRoute({ getParentRoute: () => shellRoute, path: "/audit", component: AuditPage });

const routeTree = rootRoute.addChildren([
  authRoute.addChildren([loginRoute]),
  shellRoute.addChildren([overviewRoute, nodesRoute, projectsRoute, projectDetailRoute, roomsRoute, roomDetailRoute, usageRoute, keysRoute, webhooksRoute, modulesRoute, matchmakingRoute, auditRoute]),
]);

export const router = createRouter({ routeTree, defaultPreload: "intent" });

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}
