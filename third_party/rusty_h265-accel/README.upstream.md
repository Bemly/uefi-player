# rusty_h265-accel

SIMD kernels for [`rusty_h265`](https://crates.io/crates/rusty_h265), the pure-Rust
HEVC decoder.

This is **the one crate in the workspace allowed `unsafe`**. The decoder itself is
`#![forbid(unsafe_code)]`; everything that needs raw pointers or `target_feature`
lives here, behind a runtime-dispatched seam, so the unsafe surface is one small
crate you can read in an afternoon rather than a property of the whole codec.

## The rule every kernel follows

Each kernel has a **scalar twin** that stays in the tree forever -- as the oracle
it is differential-tested against, and as the fallback on any CPU without the
instruction set. A kernel is not allowed to exist without one:

```text
    kernel_avx2()  ---- *_matches_scalar test ---->  kernel_scalar()
         |                                                |
         +------------- runtime dispatch ----------------+
```

Integer kernels are gated on **bit-identity** with the twin, not a tolerance --
HEVC decoding is exact, so anything less is a bug.

## Features

| feature | default | what it does |
|---|---|---|
| `simd` | on | the kernels. `--no-default-features` compiles zero `unsafe`. |
| `census` | off | always-on kernel call counters (else `RH265_CENSUS=1` at runtime). |

The census exists because a kernel with a test and a benchmark but no production
caller is the most expensive defect in this class -- it passes every gate while
serving nothing. The counters make reachability a measurement instead of an
assumption.

## License

Apache-2.0.
