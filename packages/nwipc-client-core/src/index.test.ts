import assert from "node:assert/strict";
import test from "node:test";
import type { NativePortBinding, NativePortHandler, SendDisposition } from "./index.js";
import { NativeMessagePort, NwipcPortError } from "./index.js";

class Binding implements NativePortBinding {
  bufferedAmount = 0;
  handler: NativePortHandler | undefined;
  closeCount = 0;

  send(payload: Uint8Array): SendDisposition {
    this.bufferedAmount += payload.byteLength;
    return this.bufferedAmount > 4 ? "backpressured" : "sent";
  }
  close(): void { this.closeCount += 1; this.handler?.close(); }
  setHandler(handler: NativePortHandler | undefined): void { this.handler = handler; }
}

test("close is terminal, synchronous, and idempotent", () => {
  const binding = new Binding();
  const port = new NativeMessagePort(binding);
  const events: string[] = [];
  port.addEventListener((event) => events.push(event.type));
  port.close();
  port.close();
  assert.equal(binding.closeCount, 1);
  assert.equal(port.state, "closed");
  assert.deepEqual(events, ["close"]);
  assert.throws(() => port.postMessage(new Uint8Array()), /NWIPC_PORT_NOT_OPEN/);
});

test("backpressure resolves writable waiters on one edge", async () => {
  const binding = new Binding();
  const port = new NativeMessagePort(binding);
  assert.equal(port.postMessage(Uint8Array.of(1, 2, 3, 4, 5)), "backpressured");
  let writableEvents = 0;
  port.onwritable = () => writableEvents += 1;
  const writable = port.writable();
  binding.bufferedAmount = 0;
  binding.handler?.writable();
  binding.handler?.writable();
  await writable;
  assert.equal(writableEvents, 1);
});

test("reentrant native delivery remains FIFO", () => {
  const binding = new Binding();
  const port = new NativeMessagePort(binding);
  const messages: number[] = [];
  port.addEventListener("message", (event) => {
    messages.push(event.data[0] ?? 0);
    if (messages.length === 1) binding.handler?.message(Uint8Array.of(2));
  });
  binding.handler?.message(Uint8Array.of(1));
  assert.deepEqual(messages, [1, 2]);
});

test("receive data is JS-owned and terminal error rejects waiters", async () => {
  const binding = new Binding();
  const port = new NativeMessagePort(binding);
  let received: Uint8Array | undefined;
  port.onmessage = (event) => { received = event.data; };
  const source = Uint8Array.of(7);
  binding.handler?.message(source);
  source[0] = 9;
  assert.deepEqual(received, Uint8Array.of(7));

  port.postMessage(Uint8Array.of(1, 2, 3, 4, 5));
  const writable = port.writable();
  binding.handler?.error("NWIPC_STALE_GENERATION");
  await assert.rejects(writable, (error) =>
    error instanceof NwipcPortError && error.code === "NWIPC_STALE_GENERATION");
  assert.equal(port.state, "failed");
});
