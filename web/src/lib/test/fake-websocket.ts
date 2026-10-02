/**
 * Scripted WebSocket fake for store tests: every `new WebSocket(url)` lands in
 * `FakeWebSocket.instances`, tests then drive `open()`/`message()`/`close()`
 * and inspect `sent` frames. Implements just enough of the DOM contract the
 * daemon store touches (readyState constants + handler assignment).
 */

export class FakeWebSocket {
  static readonly CONNECTING = 0;
  static readonly OPEN = 1;
  static readonly CLOSING = 2;
  static readonly CLOSED = 3;

  static instances: FakeWebSocket[] = [];

  static reset() {
    FakeWebSocket.instances = [];
  }

  static latest(): FakeWebSocket {
    const ws = FakeWebSocket.instances[FakeWebSocket.instances.length - 1];
    if (!ws) throw new Error("no WebSocket instance created");
    return ws;
  }

  readonly url: string;
  readonly protocols: string[] | undefined;
  readyState = FakeWebSocket.CONNECTING;
  sent: string[] = [];

  onopen: (() => void) | null = null;
  onmessage: ((event: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;

  constructor(url: string, protocols?: string | string[]) {
    this.url = url;
    this.protocols =
      protocols === undefined
        ? undefined
        : Array.isArray(protocols)
          ? protocols
          : [protocols];
    FakeWebSocket.instances.push(this);
  }

  send(data: string) {
    this.sent.push(data);
  }

  close() {
    this.readyState = FakeWebSocket.CLOSED;
    // Mirrors DOM behavior: close() does not fire onclose on the closing side
    // unless the server acknowledges; tests trigger server-side closes via
    // serverClose().
  }

  // -- test drivers ---------------------------------------------------------

  open() {
    this.readyState = FakeWebSocket.OPEN;
    this.onopen?.();
  }

  message(frame: unknown) {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }

  /** Server-initiated close. */
  serverClose() {
    this.readyState = FakeWebSocket.CLOSED;
    this.onclose?.();
  }

  serverError() {
    this.onerror?.();
  }

  sentJson(index = 0): Record<string, unknown> {
    return JSON.parse(this.sent[index]) as Record<string, unknown>;
  }
}

/** Install the fake as the global WebSocket constructor. */
export function installFakeWebSocket() {
  const ctor = FakeWebSocket as unknown as typeof WebSocket;
  globalThis.WebSocket = ctor;
}
