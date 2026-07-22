import assert from "node:assert/strict";
import test from "node:test";
import { connect, NwipcUnsupportedError } from "./index.js";

test("missing native binding is explicit", () => {
  globalThis.__nwipc = undefined;
  assert.throws(connect, NwipcUnsupportedError);
});
