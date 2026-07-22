import assert from "node:assert/strict";
import test from "node:test";
import type { NativePortBinding, NativePortHandler } from "./index.js";
import { NativeMessagePort } from "./index.js";

class Binding implements NativePortBinding {
  bufferedAmount = 0;
  handler: NativePortHandler | undefined;

  send(): "sent" { return "sent"; }
  close(): void { this.handler?.close(); }
  setHandler(handler: NativePortHandler | undefined): void { this.handler = handler; }
}

test("close is terminal and idempotent", () => {
  const binding = new Binding();
  const port = new NativeMessagePort(binding);
  const events: string[] = [];
  port.addEventListener((event) => events.push(event.type));
  port.close();
  port.close();
  assert.equal(port.state, "closed");
  assert.deepEqual(events, ["close"]);
  assert.throws(() => port.postMessage(new Uint8Array()), /NWIPC_PORT_NOT_OPEN/);
});
