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
  if (globalThis.__nwipc === undefined) throw new NwipcUnsupportedError();
  return new NativeMessagePort(globalThis.__nwipc.connect());
}

export { NativeMessagePort } from "@nwipc/client-core";
