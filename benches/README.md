# Benchmark baseline

Run the portable in-process SPSC baseline in release mode:

```sh
cargo run --release -p xtask -- benchmark
```

The command writes `target/hardening/benchmark.md` with OS, architecture, build mode, provider,
payload size, iterations, mean round-trip latency, payload throughput, and saturation in bytes and
messages. Cases are 64 B, 1 KiB, 16 KiB, and the 1 MiB maximum inline payload.

Set `NWIPC_BENCH_SCALE` to a positive integer for longer sampling. Compare results only on the same
runner, power profile, provider, and build mode; this first baseline records observations but does
not impose a noisy cross-machine CI threshold. WebKit/JSC copy count, idle CPU, and real Darwin
polling recovery remain provider-specific measurements in the signed E2E lab.
