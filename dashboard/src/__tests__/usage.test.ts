import { describe, expect, it } from "vitest";

import { toPoints } from "@/pages/Usage";

describe("toPoints", () => {
  it("groups hourly metric rows into one point per hour", () => {
    const pts = toPoints({
      from: "2026-01-01T00:00:00Z",
      to: "2026-01-01T03:00:00Z",
      usage: [
        { hour: "2026-01-01T00:00:00Z", metric: "room_minutes", quantity: 10 },
        { hour: "2026-01-01T00:00:00Z", metric: "player_minutes", quantity: 40 },
        { hour: "2026-01-01T02:00:00Z", metric: "bytes_in", quantity: 1024 },
        { hour: "2026-01-01T00:00:00Z", metric: "bytes_in", quantity: 1 },
      ],
    });
    expect(pts).toHaveLength(2);
    expect(pts[0]!.t).toBe(Date.parse("2026-01-01T00:00:00Z"));
    expect(pts[0]!.room_minutes).toBe(10);
    expect(pts[0]!.player_minutes).toBe(40);
    expect(pts[0]!.bytes_in).toBe(1);
    expect(pts[1]!.bytes_in).toBe(1024);
    expect(pts[0]!.t).toBeLessThan(pts[1]!.t);
  });

  it("returns an empty series when there is no data", () => {
    expect(toPoints(undefined)).toEqual([]);
    expect(toPoints({ from: "", to: "", usage: [] })).toEqual([]);
  });
});
