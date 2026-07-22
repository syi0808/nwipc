import assert from "node:assert/strict";
import test from "node:test";
import { NativeMessagePort } from "@nwipc/client-core";
import { MockNativeBinding } from "./index.js";

test("mock drives deterministic receive", () => {
  const binding = new MockNativeBinding();
  const port = new NativeMessagePort(binding);
  let received: Uint8Array | undefined;
  port.addEventListener((event) => {
    if (event.type === "message") received = event.data;
  });
  binding.receive(Uint8Array.of(1, 2, 3));
  assert.deepEqual(received, Uint8Array.of(1, 2, 3));
});
