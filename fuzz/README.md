# Fuzz targets

The three fail-closed wire decoders are exercised directly:

```sh
cargo install cargo-fuzz --locked
cargo fuzz run record -- -max_total_time=60
cargo fuzz run bootstrap -- -max_total_time=60
cargo fuzz run layout -- -max_total_time=60
```

Committed seeds cover magic prefixes, truncation, and the record golden fixture shape. CI performs
bounded smoke runs; longer local runs persist newly interesting inputs under `fuzz/corpus/`.

When a crash is fixed, keep its minimized input in the matching corpus directory as a regression
seed. Fuzzing is additive to `cargo test`: target assertions check canonical round trips only after
the untrusted decoder has accepted an input.
