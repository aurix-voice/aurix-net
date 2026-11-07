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

/** Heartbeats are every few seconds; 30 s of silence means the node is gone or partitioned. */
export function nodeStale(lastHeartbeat: string, now = Date.now()): boolean {
  const t = Date.parse(lastHeartbeat);
  return !Number.isFinite(t) || now - t > 30_000;
}
