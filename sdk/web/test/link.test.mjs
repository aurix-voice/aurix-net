// Offline checks for the transport link: `node --test test/`.
import { test } from "node:test";
import assert from "node:assert/strict";
import { IDLE_TIMEOUT, Link, RELIABLE_BACKLOG_BYTES } from "../dist/link.js";
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

test("a link dies when its oldest reliable packet goes unacknowledged for the idle timeout", () => {
  const near = new Link();
  assert.ok(near.poll(4 * IDLE_TIMEOUT), "nothing waits for the peer");
  const start = 4 * IDLE_TIMEOUT;
  near.send(Channel.Reliable, Uint8Array.of(1), start);
  assert.ok(near.poll(start + IDLE_TIMEOUT - 1));
  assert.equal(near.poll(start + IDLE_TIMEOUT), false);
  assert.ok(near.isDead);
});

test("a link dies when a packet stays unacknowledged while the peer talks", () => {
  const near = new Link(), far = new Link();
  near.send(Channel.Reliable, Uint8Array.of(1), 0);
  let now = 0;
  for (;;) {
    // the reliable packet never arrives; the peer keeps sending
    near.drain();
    far.send(Channel.Unreliable, Uint8Array.of(2), now);
    far.poll(now);
    for (const datagram of far.drain()) near.receive(datagram, now);
    if (!near.poll(now)) break;
    now += 100;
  }
  assert.equal(now, IDLE_TIMEOUT);
  assert.ok(near.isDead);
});

test("a packet is repeated 250 ms after it left while the round trip is unknown", () => {
  const near = new Link();
  near.send(Channel.Reliable, Uint8Array.of(1), 0);
  assert.equal(near.drain().length, 1);
  near.poll(249);
  assert.equal(near.drain().length, 0);
  near.poll(250);
  assert.equal(near.drain().length, 1);
  assert.equal(near.stats.retransmits, 1);
});

test("a sender that outruns its peer loses the link", () => {
  const near = new Link();
  const message = new Uint8Array(60000);
  let accepted = 0;
  assert.throws(() => {
    for (;;) { near.send(Channel.Reliable, message, 0); accepted++; }
  }, /outran the peer/);
  // the window takes 32 fragments of the first message; sixteen more fill the megabyte
  assert.equal(accepted, 17);
  assert.ok(RELIABLE_BACKLOG_BYTES < 18 * message.length);
  assert.ok(near.isDead);
  assert.equal(near.backlog(), 0);
  assert.equal(near.nextOutgoing(), undefined);
});

test("a dead link refuses to send and keeps nothing", () => {
  const near = new Link();
  for (let i = 0; i < 100; i++) near.send(Channel.Reliable, Uint8Array.of(1), 0);
  for (let at = 1000; at < IDLE_TIMEOUT; at += 1000) assert.ok(near.poll(at));
  assert.equal(near.poll(IDLE_TIMEOUT), false);
  assert.equal(near.backlog(), 0);
  assert.equal(near.nextOutgoing(), undefined);
  for (const channel of [Channel.Reliable, Channel.Unreliable]) {
    assert.throws(() => near.send(channel, Uint8Array.of(1), IDLE_TIMEOUT), /link dead/);
  }
  near.flushBatch();
  assert.equal(near.nextOutgoing(), undefined);
  assert.equal(near.poll(2 * IDLE_TIMEOUT), false);
});

test("a timer that comes late resends a packet before the link is given up", () => {
  const drain = (link) => { const out = []; for (let d; (d = link.nextOutgoing()); ) out.push(d); return out; };
  for (const answered of [true, false]) {
    const near = new Link(), far = new Link();
    near.send(Channel.Reliable, Uint8Array.of(7), 0);
    assert.equal(drain(near).length, 1, "the packet goes out, and is lost");
    const late = IDLE_TIMEOUT + 5000;
    assert.ok(near.poll(late), "no resend had its chance yet");
    const again = drain(near);
    assert.equal(again.length, 1, "the packet is resent");
    if (answered) {
      for (const datagram of again) far.receive(datagram, late);
      far.poll(late + 100);
      for (const datagram of drain(far)) near.receive(datagram, late + 100);
      assert.ok(near.poll(late + 2 * IDLE_TIMEOUT), "the resend was acknowledged");
    } else {
      assert.ok(near.poll(late + IDLE_TIMEOUT - 1));
      assert.equal(near.poll(late + IDLE_TIMEOUT), false, "the resend went unanswered for the idle timeout");
    }
  }
});
