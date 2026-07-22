import assert from "node:assert/strict";
import test from "node:test";
import { connect, NwipcUnsupportedError, type NwipcNativeBinding } from "./index.js";

test("missing native binding is explicit", () => {
  globalThis.__nwipc = undefined;
  assert.throws(connect, NwipcUnsupportedError);
});

test("malformed native binding is explicit", () => {
  globalThis.__nwipc = {} as NwipcNativeBinding;
  assert.throws(connect, NwipcUnsupportedError);
});
