import { describe, expect, it } from "vitest";

import { nodeStale } from "@/lib/rooms";

describe("nodeStale", () => {
  const at = Date.parse("2026-11-18T12:00:00Z");
  const node = (heartbeat_secs?: number) => ({ last_heartbeat: "2026-11-18T12:00:00Z", heartbeat_secs });

  it("does not flag a healthy node between two slow heartbeats", () => {
    expect(nodeStale(node(40), at + 34_000)).toBe(false);
    expect(nodeStale(node(40), at + 81_000)).toBe(true);
  });
  it("uses the default 30 s interval when the row carries none", () => {
    expect(nodeStale(node(), at + 45_000)).toBe(false);
    expect(nodeStale(node(), at + 61_000)).toBe(true);
  });
  it("flags a row without a heartbeat time", () => {
    expect(nodeStale({ last_heartbeat: "" }, at)).toBe(true);
  });
});
