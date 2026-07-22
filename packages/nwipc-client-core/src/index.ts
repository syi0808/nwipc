export type PortState = "connecting" | "open" | "closing" | "closed" | "failed";

export interface NativePortBinding {
  readonly bufferedAmount: number;
  send(payload: Uint8Array): "sent" | "backpressured";
  close(): void;
  setHandler(handler: NativePortHandler | undefined): void;
}
export interface NativePortHandler {
  message(payload: Uint8Array): void;
  writable(): void;
  close(): void;
  error(code: string): void;
}

export type PortEvent =
  | { readonly type: "message"; readonly data: Uint8Array }
  | { readonly type: "writable" }
  | { readonly type: "close" }
  | { readonly type: "error"; readonly code: string };

export type PortListener = (event: PortEvent) => void;

export class NativeMessagePort {
  #binding: NativePortBinding | undefined;
  #listeners = new Set<PortListener>();
  #state: PortState = "connecting";
  #writableResolvers: Array<() => void> = [];

  constructor(binding: NativePortBinding) {
    this.#binding = binding;
    binding.setHandler({
      message: (data) => this.#dispatch({ type: "message", data: data.slice() }),
      writable: () => this.#onWritable(),
      close: () => this.#finish("closed", { type: "close" }),
      error: (code) => this.#finish("failed", { type: "error", code }),
    });
    this.#state = "open";
  }

  get state(): PortState {
    return this.#state;
  }

  get bufferedAmount(): number {
    return this.#binding?.bufferedAmount ?? 0;
  }

  addEventListener(listener: PortListener): () => void {
    this.#listeners.add(listener);
    return () => this.#listeners.delete(listener);
  }

  postMessage(payload: Uint8Array): "sent" | "backpressured" {
    if (this.#state !== "open" || this.#binding === undefined) {
      throw new Error("NWIPC_PORT_NOT_OPEN");
    }
    return this.#binding.send(payload);
  }

  writable(): Promise<void> {
    if (this.bufferedAmount === 0) return Promise.resolve();
    return new Promise((resolve) => this.#writableResolvers.push(resolve));
  }

  close(): void {
    if (this.#state === "closed" || this.#state === "failed") return;
    this.#state = "closing";
    this.#binding?.close();
  }

  #onWritable(): void {
    const resolvers = this.#writableResolvers.splice(0);
    for (const resolve of resolvers) resolve();
    this.#dispatch({ type: "writable" });
  }

  #finish(state: "closed" | "failed", event: PortEvent): void {
    if (this.#state === "closed" || this.#state === "failed") return;
    this.#state = state;
    this.#binding?.setHandler(undefined);
    this.#binding = undefined;
    this.#dispatch(event);
    this.#listeners.clear();
  }

  #dispatch(event: PortEvent): void {
    for (const listener of [...this.#listeners]) listener(event);
  }
}
