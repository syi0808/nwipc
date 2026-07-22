import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { NativeMessagePort } from "@nwipc/client-core";
import { MockNativeBinding, rendererContractScenarios } from "./index.js";

test("mock drives deterministic binary receive and copies send data", () => {
  const binding = new MockNativeBinding();
  const port = new NativeMessagePort(binding);
  let received: Uint8Array | undefined;
  port.onmessage = (event) => { received = event.data; };
  binding.receive(Uint8Array.of(1, 2, 3));
  assert.deepEqual(received, Uint8Array.of(1, 2, 3));

  const payload = Uint8Array.of(4);
  port.postMessage(payload);
  payload[0] = 9;
  assert.deepEqual(binding.sent, [Uint8Array.of(4)]);
});

test("mock exposes writable, close, and error terminal drivers", async () => {
  const binding = new MockNativeBinding();
  const port = new NativeMessagePort(binding);
  binding.setBackpressured(10);
  assert.equal(port.postMessage(Uint8Array.of(1)), "backpressured");
  const writable = port.writable();
  binding.becomeWritable();
  await writable;
  binding.fail("NWIPC_TEST_FAILURE");
  assert.equal(port.state, "failed");
  assert.equal(binding.handlerAttached, false);
  binding.closeRemote();
  assert.equal(port.state, "failed");
});

test("contract scenario names are stable and complete", () => {
  const fixture = readFileSync(
    new URL("../../../tests/renderer-contract/scenarios.txt", import.meta.url),
    "utf8",
  ).trim().split("\n");
  assert.deepEqual(rendererContractScenarios, fixture);
  assert.deepEqual(fixture, [
    "binary-copy",
    "fifo-reentrancy",
    "backpressure-writable-edge",
    "terminal-close",
    "terminal-error",
    "stale-document",
  ]);
});
