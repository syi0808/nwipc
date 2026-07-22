import { NativeMessagePort, type NativePortBinding } from "@nwipc/client-core";

export interface NwipcNativeBinding {
  connect(): NativePortBinding;
}

declare global {
  var __nwipc: NwipcNativeBinding | undefined;
}

export class NwipcUnsupportedError extends Error {
  readonly code = "NWIPC_UNSUPPORTED";

  constructor() {
    super("NWIPC native binding is unavailable");
    this.name = "NwipcUnsupportedError";
  }
}

export function connect(): NativeMessagePort {
  const binding = globalThis.__nwipc;
  if (binding === undefined || typeof binding.connect !== "function") throw new NwipcUnsupportedError();
  return new NativeMessagePort(binding.connect());
}

export { NativeMessagePort, NwipcPortError } from "@nwipc/client-core";
