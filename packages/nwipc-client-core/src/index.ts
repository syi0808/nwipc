export type PortState = "connecting" | "open" | "closing" | "closed" | "failed";
export type SendDisposition = "sent" | "backpressured";

export interface NativePortBinding {
  readonly bufferedAmount: number;
  send(payload: Uint8Array): SendDisposition;
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

export type PortEventType = PortEvent["type"];
export type PortEventOf<Type extends PortEventType> = Extract<PortEvent, { type: Type }>;
export type PortListener<Type extends PortEventType = PortEventType> =
  (event: PortEventOf<Type>) => void;

export class NwipcPortError extends Error {
  constructor(readonly code: string) {
    super(code);
    this.name = "NwipcPortError";
  }
}

type WritableWaiter = {
  readonly resolve: () => void;
  readonly reject: (error: NwipcPortError) => void;
};

export class NativeMessagePort {
  #binding: NativePortBinding | undefined;
  #listeners = new Map<PortEventType, Set<PortListener>>();
  #state: PortState = "connecting";
  #writableWaiters: WritableWaiter[] = [];
  #eventQueue: PortEvent[] = [];
  #dispatching = false;
  #backpressured = false;

  onmessage: PortListener<"message"> | null = null;
  onwritable: PortListener<"writable"> | null = null;
  onclose: PortListener<"close"> | null = null;
  onerror: PortListener<"error"> | null = null;

  constructor(binding: NativePortBinding) {
    this.#binding = binding;
    binding.setHandler({
      message: (data) => this.#onMessage(data),
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

  addEventListener(listener: PortListener): () => void;
  addEventListener<Type extends PortEventType>(type: Type, listener: PortListener<Type>): () => void;
  addEventListener<Type extends PortEventType>(
    typeOrListener: Type | PortListener,
    possibleListener?: PortListener<Type>,
  ): () => void {
    if (typeof typeOrListener === "function") {
      const listener = typeOrListener;
      for (const type of ["message", "writable", "close", "error"] as const) {
        this.#listenersFor(type).add(listener);
      }
      return () => {
        for (const listeners of this.#listeners.values()) listeners.delete(listener);
      };
    }
    const listener = possibleListener as unknown as PortListener;
    this.#listenersFor(typeOrListener).add(listener);
    return () => this.#listeners.get(typeOrListener)?.delete(listener);
  }

  postMessage(payload: Uint8Array): SendDisposition {
    if (!(payload instanceof Uint8Array)) throw new NwipcPortError("NWIPC_INVALID_PAYLOAD");
    const binding = this.#requireOpen();
    const disposition = binding.send(payload);
    if (disposition !== "sent" && disposition !== "backpressured") {
      throw new NwipcPortError("NWIPC_INVALID_NATIVE_RESULT");
    }
    this.#backpressured = disposition === "backpressured";
    return disposition;
  }

  writable(): Promise<void> {
    if (this.#state !== "open") {
      return Promise.reject(new NwipcPortError("NWIPC_PORT_NOT_OPEN"));
    }
    if (!this.#backpressured && this.bufferedAmount === 0) return Promise.resolve();
    return new Promise((resolve, reject) => this.#writableWaiters.push({ resolve, reject }));
  }

  close(): void {
    if (this.#state === "closing" || this.#state === "closed" || this.#state === "failed") return;
    this.#state = "closing";
    try {
      this.#binding?.close();
    } catch {
      this.#finish("failed", { type: "error", code: "NWIPC_NATIVE_CLOSE_FAILED" });
    }
  }

  #requireOpen(): NativePortBinding {
    if (this.#state !== "open" || this.#binding === undefined) {
      throw new NwipcPortError("NWIPC_PORT_NOT_OPEN");
    }
    return this.#binding;
  }

  #onMessage(data: Uint8Array): void {
    if (this.#state === "closed" || this.#state === "failed") return;
    if (!(data instanceof Uint8Array)) {
      this.#finish("failed", { type: "error", code: "NWIPC_INVALID_NATIVE_PAYLOAD" });
      return;
    }
    this.#enqueue({ type: "message", data: data.slice() });
  }

  #onWritable(): void {
    if (this.#state !== "open" || (!this.#backpressured && this.#writableWaiters.length === 0)) return;
    this.#backpressured = false;
    const waiters = this.#writableWaiters.splice(0);
    for (const waiter of waiters) waiter.resolve();
    this.#enqueue({ type: "writable" });
  }

  #finish(state: "closed" | "failed", event: PortEventOf<"close"> | PortEventOf<"error">): void {
    if (this.#state === "closed" || this.#state === "failed") return;
    this.#state = state;
    this.#backpressured = false;
    this.#binding?.setHandler(undefined);
    this.#binding = undefined;
    const waiters = this.#writableWaiters.splice(0);
    const error = new NwipcPortError(event.type === "error" ? event.code : "NWIPC_PORT_CLOSED");
    for (const waiter of waiters) waiter.reject(error);
    this.#enqueue(event);
  }

  #enqueue(event: PortEvent): void {
    this.#eventQueue.push(event);
    if (this.#dispatching) return;
    this.#dispatching = true;
    try {
      while (this.#eventQueue.length > 0) {
        const next = this.#eventQueue.shift();
        if (next === undefined) break;
        this.#dispatch(next);
        if (next.type === "close" || next.type === "error") {
          this.#eventQueue.length = 0;
          this.#listeners.clear();
          this.onmessage = this.onwritable = this.onclose = this.onerror = null;
        }
      }
    } finally {
      this.#dispatching = false;
    }
  }

  #dispatch(event: PortEvent): void {
    const propertyListener = this.#propertyListener(event.type);
    propertyListener?.(event as never);
    for (const listener of [...(this.#listeners.get(event.type) ?? [])]) {
      listener(event as never);
    }
  }

  #propertyListener(type: PortEventType): PortListener | null {
    switch (type) {
      case "message": return this.onmessage as PortListener | null;
      case "writable": return this.onwritable as PortListener | null;
      case "close": return this.onclose as PortListener | null;
      case "error": return this.onerror as PortListener | null;
    }
  }

  #listenersFor(type: PortEventType): Set<PortListener> {
    let listeners = this.#listeners.get(type);
    if (listeners === undefined) {
      listeners = new Set();
      this.#listeners.set(type, listeners);
    }
    return listeners;
  }
}
