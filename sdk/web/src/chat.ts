import type { Client } from "./index.js";
import type { EventFrame } from "./event.js";

/** A chat message as delivered to `onMessage`: `channel` is undefined for whispers. */
export interface ChatMessage {
  from: number;
  channel?: number;
  text: string;
  cached: boolean;
}

/** Chat on top of a relay room, no extra service: channels are interest groups (join = subscribe),
 *  messages are reliable events to the group with `cache: "add"` so late joiners get the history the room
 *  still holds, whispers are events to one actor. Everything stays inside the room (and its moderation,
 *  rate limits and GDPR tooling). */
export class Chat {
  private readonly joined = new Set<number>();
  private handlers: Array<(message: ChatMessage) => void> = [];
  private readonly textDecoder = new TextDecoder();
  private readonly textEncoder = new TextEncoder();

  /** `code` is the relay event code reserved for chat (default 240); `groupBase` keeps chat groups away from gameplay groups. */
  constructor(
    private readonly client: Client,
    private readonly opts: {
      code?: number;
      groupBase?: number;
      history?: boolean;
    } = {},
  ) {
    client.on("event", (frame: EventFrame) => this.onEvent(frame));
  }
  get code(): number {
    return this.opts.code ?? 240;
  }
  private group(channel: number): number {
    return (this.opts.groupBase ?? 60000) + channel;
  }

  join(channel: number): void {
    if (!this.joined.has(channel)) {
      this.joined.add(channel);
      this.client.setGroups([this.group(channel)], []);
      for (const frame of this.client.cached) {
        if (
          frame.code === this.code &&
          frame.payload[0] === 0 &&
          ((frame.payload[1] << 8) | frame.payload[2]) === channel
        ) {
          this.onEvent(frame);
        }
      }
    }
  }
  leave(channel: number): void {
    if (this.joined.delete(channel)) {
      this.client.setGroups([], [this.group(channel)]);
    }
  }
  /** Send to a channel; `history` (default from opts, true) keeps it in the room cache for late joiners. */
  send(
    channel: number,
    text: string,
    history = this.opts.history ?? true,
  ): void {
    this.client.sendEvent(
      this.code,
      this.encode(0, channel, text),
      "all",
      true,
      { group: this.group(channel), cache: history ? "add" : undefined },
    );
  }
  /** Private message to one player (never cached). */
  whisper(actor: number, text: string): void {
    this.client.sendEvent(this.code, this.encode(1, 0, text), [actor], true);
  }
  onMessage(fn: (message: ChatMessage) => void): () => void {
    this.handlers.push(fn);
    return () => {
      this.handlers = this.handlers.filter((handler) => handler !== fn);
    };
  }

  private encode(kind: number, channel: number, text: string): Uint8Array {
    const encoded = this.textEncoder.encode(text.slice(0, 2000));
    const payload = new Uint8Array(3 + encoded.length);
    payload[0] = kind;
    payload[1] = channel >> 8;
    payload[2] = channel & 0xff;
    payload.set(encoded, 3);
    return payload;
  }
  private onEvent(frame: EventFrame): void {
    const cached = frame.cached === true;
    if (frame.code !== this.code || frame.payload.length < 3) {
      return;
    }
    const kind = frame.payload[0];
    const channel = (frame.payload[1] << 8) | frame.payload[2];
    const text = this.textDecoder.decode(frame.payload.subarray(3));
    const message: ChatMessage =
      kind === 1
        ? { from: frame.from, text, cached }
        : { from: frame.from, channel, text, cached };
    if (kind === 0 && !this.joined.has(channel)) {
      return;
    }
    for (const handler of this.handlers) {
      handler(message);
    }
  }
}
