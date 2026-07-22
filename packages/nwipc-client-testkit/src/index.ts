import type { NativePortBinding, NativePortHandler, SendDisposition } from "@nwipc/client-core";

export class MockNativeBinding implements NativePortBinding {
  #handler: NativePortHandler | undefined;
  #bufferedAmount = 0;
  readonly sent: Uint8Array[] = [];

  get handlerAttached(): boolean { return this.#handler !== undefined; }

  get bufferedAmount(): number { return this.#bufferedAmount; }

  send(payload: Uint8Array): SendDisposition {
    this.sent.push(payload.slice());
    return this.#bufferedAmount === 0 ? "sent" : "backpressured";
  }

  close(): void { this.#handler?.close(); }

  setHandler(handler: NativePortHandler | undefined): void { this.#handler = handler; }

  receive(payload: Uint8Array): void { this.#handler?.message(payload); }

  setBackpressured(bytes: number): void { this.#bufferedAmount = bytes; }

  becomeWritable(): void {
    this.#bufferedAmount = 0;
    this.#handler?.writable();
  }

  fail(code: string): void { this.#handler?.error(code); }

  closeRemote(): void { this.#handler?.close(); }
}

export const rendererContractScenarios = Object.freeze([
  "binary-copy",
  "fifo-reentrancy",
  "backpressure-writable-edge",
  "terminal-close",
  "terminal-error",
  "stale-document",
] as const);
