// Offline checks for the transport link: `node --test test/`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { Link } from "../dist/link.js";
import { Channel } from "../dist/packet.js";

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

// One datagram per message on `channel`, and what the far side delivers when they arrive in `order`.
function delivered(channel, order) {
  const near = new Link(), far = new Link();
  const datagrams = [];
  for (let i = 0; i < 3; i++) { near.send(channel, Uint8Array.of(i)); near.flushBatch(); datagrams.push(...near.drain()); }
  return order.flatMap((i) => far.receive(datagrams[i])).map((message) => [message.channel, message.payload[0]]);
}

test("the unsequenced channel delivers what arrives out of order, the sequenced one drops it", () => {
  assert.deepEqual(delivered(Channel.UnreliableUnsequenced, [2, 0, 1]), [[4, 2], [4, 0], [4, 1]]);
  assert.deepEqual(delivered(Channel.Unreliable, [2, 0, 1]), [[0, 2]]);
});

test("the unsequenced channel has a datagram and a counter of its own", () => {
  const near = new Link(), far = new Link();
  near.send(Channel.Unreliable, Uint8Array.of(1)); near.send(Channel.UnreliableUnsequenced, Uint8Array.of(2));
  near.send(Channel.UnreliableUnsequenced, Uint8Array.of(3)); near.flushBatch();
  const datagrams = near.drain();
  // version 1, kind | batch flag, seq 0: each channel counts from 0
  assert.deepEqual(datagrams.map((d) => [...d.subarray(0, 4)]), [[1, 0x20, 0, 0], [1, 0x24, 0, 0]]);
  assert.deepEqual(datagrams.flatMap((d) => far.receive(d)).map((m) => [m.channel, m.payload[0]]), [[0, 1], [4, 2], [4, 3]]);
});

test("a message too large for one datagram keeps the unsequenced channel", () => {
  const near = new Link(), far = new Link();
  const message = new Uint8Array(3000).map((_, i) => i % 251);
  near.send(Channel.UnreliableUnsequenced, message);
  const got = near.drain().flatMap((d) => far.receive(d));
  assert.equal(got.length, 1);
  assert.equal(got[0].channel, Channel.UnreliableUnsequenced);
  assert.deepEqual(got[0].payload, message);
});
