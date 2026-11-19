import { describe, expect, it } from "vitest";

import { sortRows, type Column } from "@/ui/Table";

interface Row {
  name: string;
  at: string | null;
}

const columns: Column<Row>[] = [
  { key: "name", header: "Name", sort: (r) => r.name, cell: (r) => r.name },
  { key: "at", header: "At", sort: (r) => (r.at ? Date.parse(r.at) : null), cell: (r) => r.at },
];

const rows: Row[] = [
  { name: "b", at: "2026-11-18T19:09:40.5Z" },
  { name: "a", at: "2026-11-18T19:09:40.123456Z" },
  { name: "d", at: null },
  { name: "c", at: "2026-11-18T19:09:40Z" },
];

const names = (r: Row[]) => r.map((x) => x.name).join("");

describe("sortRows", () => {
  it("leaves the rows as they came without a sort", () => {
    expect(names(sortRows(rows, columns, null))).toBe("badc");
  });
  it("orders dates of one second by their fraction, whatever its length", () => {
    expect(names(sortRows(rows, columns, { key: "at", dir: "asc" }))).toBe("cabd");
    expect(names(sortRows(rows, columns, { key: "at", dir: "desc" }))).toBe("bacd");
  });
  it("orders text", () => {
    expect(names(sortRows(rows, columns, { key: "name", dir: "desc" }))).toBe("dcba");
  });
});
