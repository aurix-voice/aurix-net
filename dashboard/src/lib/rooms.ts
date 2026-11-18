import type { Tone } from "@/ui/Primitives";

export function modeTone(mode: string): Tone {
  switch (mode) {
    case "relay":
      return "neutral";
    case "sync":
      return "accent";
    case "lockstep":
      return "ok";
    default:
      return "warn";
  }
}

/**
 * A node is stale once it has missed two of its own heartbeats in a row (the interval is per
 * node, `server.heartbeat_secs`, 30 s by default); after the third its peers remove it.
 */
export function nodeStale(node: { last_heartbeat: string; heartbeat_secs?: number }, now = Date.now()): boolean {
  const t = Date.parse(node.last_heartbeat);
  return !Number.isFinite(t) || now - t > 2 * (node.heartbeat_secs ?? 30) * 1000;
}
