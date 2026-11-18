// Offline checks for the transport link: `node --test test/`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { Link } from "../dist/link.js";

test("a lost packet in a burst longer than the ack window is still retransmitted and acked", () => {
  const a = new Link(), b = new Link();
  const msg = new Uint8Array(60000).map((_, i) => i % 251);
  let now = 1000, lost = false, got = null;
  const step = () => {
    for (const d of a.drain()) {
      // lose the first transmission of the first fragment packet (kind 3, not ack-only)
      if (!lost && (d[1] & 0x1f) === 3 && !(d[1] & 0x80)) { lost = true; continue; }
      for (const r of b.receive(d, now)) got = r;
    }
    for (const d of b.drain()) a.receive(d, now);
  };
  a.send(1, msg); step();
  for (let i = 0; i < 2000 && !got; i++) {
    now += 10;
    assert.ok(a.poll(now), "link reported dead");
    step();
  }
  assert.ok(lost && got, "message never arrived");
  assert.equal(got.channel, 1);
  assert.deepEqual(got.payload, msg);
});
