import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The console talks to a node through the same origin: Caddy proxies these prefixes in the
// image (see Caddyfile); in development Vite does the same against NET_API_URL.
const API_PREFIXES = ["/v1", "/healthz"];
const apiUrl = process.env.NET_API_URL ?? "http://127.0.0.1:8090";
const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")) as { version: string };

export default defineConfig({
  plugins: [react(), tailwindcss()],
  define: {
    __DASHBOARD_VERSION__: JSON.stringify(`v${pkg.version}`),
    // Where the source of *this* build lives (AGPL-3.0 §13): a modified deployment points it at its
    // own repository with AURIX_SOURCE_URL at build time.
    __DASHBOARD_SOURCE_URL__: JSON.stringify(process.env.NET_SOURCE_URL ?? "https://github.com/aurix-voice/aurix-net"),
  },
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    port: 5174,
    proxy: Object.fromEntries(
      API_PREFIXES.map((p) => [p, { target: apiUrl, changeOrigin: true, ws: false }]),
    ),
  },
  build: {
    sourcemap: false,
    rolldownOptions: {
      output: {
        codeSplitting: {
          groups: [
            { name: "react", test: /node_modules[/\\](react|react-dom|scheduler)[/\\]/ },
            { name: "charts", test: /node_modules[/\\](recharts|d3-[a-z]+|victory-vendor)[/\\]/ },
          ],
        },
      },
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.{ts,tsx}"],
  },
});
