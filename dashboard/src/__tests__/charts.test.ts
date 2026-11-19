import { describe, expect, it } from "vitest";

import { compactTick, tickFormatter } from "@/components/charts";

describe("compactTick", () => {
  it("keeps small values verbatim", () => {
    expect(compactTick(0)).toBe("0");
    expect(compactTick(42)).toBe("42");
    expect(compactTick(0.225)).toBe("0.23");
    expect(compactTick(0.1)).toBe("0.1");
    expect(compactTick(2.5)).toBe("2.5");
  });

  it("abbreviates thousands and beyond", () => {
    expect(compactTick(1200)).toBe("1.2k");
    expect(compactTick(6000)).toBe("6k");
    expect(compactTick(150_000)).toBe("150k");
    expect(compactTick(3_500_000)).toBe("3.5M");
    expect(compactTick(2_000_000_000)).toBe("2G");
    expect(compactTick(-1500)).toBe("-1.5k");
  });
});

describe("tickFormatter", () => {
  const hour = 3_600_000;
  const noon = new Date(2026, 10, 17, 12, 0).getTime();

  it("labels a chart of up to a day with the time", () => {
    expect(tickFormatter("en", 24 * hour)(noon)).toBe("12:00 PM");
  });

  it("adds the date once the chart covers more than a day", () => {
    expect(tickFormatter("en", 47 * hour)(noon)).toBe("Nov 17, 12:00 PM");
    expect(tickFormatter("en", 7 * 24 * hour)(noon)).toBe("Nov 17, 2026");
  });
});
