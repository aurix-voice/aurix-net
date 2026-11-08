# Aurix Net console

Operator console for an Aurix Net cluster: nodes, projects, live rooms, usage, API keys, webhooks,
WASM modules, matchmaking and the audit log. Single-page app (Vite + React + TypeScript + Tailwind)
shipped as its own image; the node never serves it. The SPA talks to the node only through a
same-origin proxy (`/v1/*`, `/healthz`), so the token never leaves the console's origin.

Sign in with the node's operator token (`auth.admin_token`, whole fleet) or a project API key
(`an_…`, that project only). Sessions live in the browser's local storage and are revalidated
against `/v1/me` on load.

```sh
npm ci            # Node >= 22.12 (package.json engines, enforced by .npmrc engine-strict): older Node
                  # skips rolldown's optional native binding and `npm run build` dies with "Cannot find native binding"
NET_API_URL=http://127.0.0.1:8090 npm run dev                  # http://127.0.0.1:5174
npm run check && npm run lint && npm test && npm run build
AURIX_NET_E2E_TOKEN=… npm run e2e                               # Playwright against a live node
```

Production: `docker build -f dashboard/Dockerfile .` from the repository root; run with
`NET_API_URL=http://net:8090` pointing at a node (see `Caddyfile`). The image is non-root and
serves on port 8080.
