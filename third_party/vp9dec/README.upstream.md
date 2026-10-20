# vp9dec

A fully from-scratch VP9 video decoder (Rust, zero dependency crates).

## Purpose

vp9dec is a clean-room decoder for VP9 (a royalty-free video codec). The decoder itself -- everything under
`src/` -- depends on no external crates: Rust standard library only, no runtime dependencies.
(Test tooling may use `[dev-dependencies]`; in practice the only one is a self-referential
dev-dep on this crate for test-only encoder helpers, with no effect on a normal `cargo build`.)

The primary reference is the [VP9 Bitstream & Decoding Process Specification v0.7](
https://storage.googleapis.com/downloads.webmproject.org/docs/vp9/vp9-bitstream-specification-v0.7-20170222-draft.pdf)
(Google, February 22, 2017 edition). No existing OSS implementation (libvpx etc.) source code
is consulted (clean-room implementation).

## Current architecture

### Public API

- `Decoder`: a stateful decoder for one VP9 stream (VP9 carries reference frames, probability
  tables, and other state across frames -- see the doc comment on `Decoder` in `src/lib.rs` for
  the full list). `Decoder::new()`, then repeated calls to
  `Decoder::decode_frame(&mut self, chunk: &[u8]) -> Result<Vec<DecodedFrame>, DecodeError>`,
  one call per container chunk (e.g. one IVF frame), in bitstream/decode order. A chunk may pack
  more than one VP9 frame via the "superframe" mechanism (e.g. a hidden altref frame followed by
  a visible frame); `decode_frame` splits it internally and returns one `DecodedFrame` per
  constituent VP9 frame, in order.
- `DecodedFrame { info: Option<FrameDecodeInfo>, frame: Option<Frame> }`: `info` is `None` only
  for a `show_existing_frame` chunk (no uncompressed header is parsed on that path); `frame` is
  `None` for a hidden frame (`show_frame == 0`).
- `Frame { width, height, bit_depth, subsampling_x, subsampling_y, y, u, v }`: one decoded
  picture, cropped to display size. Each plane is a `PlaneData` (`enum { U8(Vec<u8>),
  U16(Vec<u16>) }`, row-major): 8-bit streams decode to `U8`, 10/12-bit (profile 2/3) to `U16`.
- `FrameDecodeInfo`: read-only per-frame decode statistics (`intra_only`, `frame_is_intra`,
  `reset_frame_context`, `segmentation_enabled`, `seg_features_active`), for observation only --
  no effect on decode behavior.
- `DecodeError`: `Header`/`CompressedHeader`/`Tile` (wrapping the parse-layer error),
  `TruncatedFrame`, `MissingReferenceFrame`.
- `ivf`: the only other public module -- `IvfReader`/`IvfHeader`/`IvfFrame`/`IvfError` for
  reading an IVF container, and `write_ivf(...)` for the inverse.

### Module map

The decode pipeline in data-flow order. Every module below is `#[doc(hidden)] pub` -- internal,
exposed only so the pure-std integration tests can reach it -- and its doc-comment cites the
exact spec sections it implements.

- **`bit_reader` / `bool_coder`** -- the plain uncompressed-header bit reader and VP9's
  arithmetic (bool) decoder (the latter drives the compressed header and all tile data).
- **`header` / `compressed_header`** -- uncompressed- and compressed-header parsing; the 4
  frame-context probability slots (`FrameContextStore`) and cross-frame `PersistentState`.
- **`tile` + `tile/{mode_info, ref_ctx, mv_pred, residual, parallel}`** -- the tile-decode hub:
  partition/block decode, mode & MV syntax, neighbor contexts, MV prediction, and `residual`
  (per-plane prediction + token decode + reconstruction + the per-frame dequant table).
  `tile/parallel` decodes a multi-tile-column frame (with exactly one tile row)
  on one worker thread per tile column, each into column-strip buffers merged back disjointly;
  other tilings decode sequentially.
- **`predict` / `transform` / `quant` / `scan`** -- intra prediction and inter motion
  compensation; the inverse DCT/ADST/WHT transforms; dequantization; coefficient scan orders.
- **`loop_filter`** -- the deblocking filter, applied once per frame after tile decode.
- **`dpb` / `superframe`** -- the 8-slot reference-frame buffer (also serves
  `show_existing_frame`) and superframe splitting.
- **`common` / `subpel` / `mv_ref_tables` / `prob_tables`** -- shared constant tables (default
  probabilities, decode trees, subpel filters, MV-ref tables) with no decode logic of their own.
- **`test_support`** (feature-gated) -- encoder-side mirrors of the bit/bool coders, used to
  hand-build synthetic bitstreams for round-trip tests; never in a normal build.

### Current decode support

**All four VP9 profiles**: 8/10/12-bit depth and 4:2:0 / 4:2:2 / 4:4:0 / 4:4:4 chroma
subsampling. Key frames, intra-only frames, and inter frames; full segmentation (segment-id
decode including temporal prediction, and all four `SEG_LVL_*` features); superframes;
`show_existing_frame`. 10/12-bit output is exposed via `PlaneData::U16` on the decoded `Frame`.

## Status / limitations

The full official-vector sweep is complete across all profiles: **315/315 MD5-checkable vectors
decode bit-exact**, both with SIMD enabled and forced scalar. To reproduce, fetch the corpus
with `scripts/fetch-vectors.{sh,ps1}`, then run:

```sh
RUST_MIN_STACK=16777216 cargo test --release --test sweep_test official_vector_sweep -- --nocapture
```

### What's proven (verified against an external oracle, not just this crate's own tests)

| Area | Evidence |
| --- | --- |
| Key frame + inter frame decode, motion compensation, probability adaptation, DPB, loop filter, tiles (1x1 through 4x4), superframes, `show_existing_frame`, frame-parallel-mode streams, mid-stream resize, intra-only frames, SVC | The full official sweep: 315/315 vectors, every displayed frame bit-exact against the official `.ivf.md5` |
| All four profiles: 8/10/12-bit depth, 4:2:0 / 4:2:2 / 4:4:0 / 4:4:4 subsampling | The profile 1/2/3 official vectors (`vp91-2-04-yuv{422,440,444}`, `vp92-2-20-{10,12}bit-yuv420`, `vp93-2-20-{10,12}bit-yuv{422,440,444}`) all decode bit-exact in the sweep (10/12-bit MD5 over the 16-bit LE output, per libvpx convention) |
| Reference-frame scaling (spec §8.5.2.3, a reference whose size differs from the current frame) | Verified end-to-end by the sweep: the SVC vectors (`vp90-2-22-svc_1280x720_*`, 2:1 inter-layer scaling), the resize families (`vp90-2-05/14/18/21-*resize*`, scaling across mid-stream size changes), and `vp90-2-13-largescaling` all decode bit-exact |
| Segmentation: seg-id decode, `SEG_LVL_ALT_Q` | Included in the `vp90-2-15-segkey*` bit-exact matches in the sweep |
| Segmentation: `SEG_LVL_ALT_L`, `SEG_LVL_REF_FRAME`, `SEG_LVL_SKIP` (no official vector exists for these -- see below) | Synthetic round-trip vectors (`tests/synthetic_seg_test.rs`) cross-decoded byte-identically by two independent third-party VP9 decoders (ffmpeg's `libvpx-vp9` and its native `vp9`), 8/8 `[xdecode]` checks (4 scenarios x 2 decoders) |
| Loop filter, including the `SEG_LVL_ALT_L` level-override path and the frame-level `loop_filter_level == 0` gate | Exercised by the whole sweep; the `SEG_LVL_ALT_L` and level-0-gate synthetic vectors additionally pin exact hand-derived pixel values (not just "output changed") that match both ffmpeg decoders |
| Profile-3 HBD multi-tile output (a combination too wide for the official HBD vectors) | A conformant synthetic 512x64 10-bit 4:4:4 key frame (`tests/synthetic_hbd_tile_test.rs`) produces exact split-plane values, matches forced-sequential tile decode, and cross-decodes byte-identically with ffmpeg's `libvpx-vp9` and native `vp9` decoders |
| Malformed-input rejection at libvpx parity | The official libvpx `invalid-*` corpus (`kVP9InvalidFileTests`): `tests/invalid_vector_test.rs` decodes each vector packet by packet and requires an `Err` at exactly the packet libvpx's `.res` sidecar first rejects -- 21/21. (`tests/robustness_test.rs` separately fuzzes for no-panic-on-arbitrary-corruption.) |

Internal regression coverage additionally compares exact SIMD and forced-scalar reports for 27
generated conformant scenarios in isolated processes (`tests/simd_scalar_differential_test.rs`).
`tests/structured_fuzz_test.rs` preserves uncompressed headers, prior chunks, superframe indexes,
and tile partitioning while mutating compressed-header or tile entropy suffixes, complementing
the arbitrary-corruption fuzz.

### Known limits

| Limit | Detail |
| --- | --- |
| Intra prediction and the WHT (lossless) inverse transform stay scalar | AVX2 covers all inter prediction (including reference-edge clamping), both loop-filter directions, and every non-lossless inverse transform at all bit depths. Measurements kept the remaining paths scalar: intra prediction was at most 1.7% on representative inter content and 3.1% on the short intra-only vector; the WHT-containing stage was 1.6 ms on a two-frame lossless vector. Revisit for a measured workload where either is material. |
| No aarch64 NEON kernels | Non-x86 targets use the bit-exact scalar fallback. A NEON mirror is intentionally conditional on a concrete aarch64 deployment and workload; there is no such target in scope today. |
| 19 corpus clips ship no upstream `.md5` | The 7 `vp90-2-bbb_*` and 12 `vp90-2-tos_*`/`vp90-2-sintel_*` movie clips ship only a `.webm` upstream (libvpx uses them for its own perf tests, not md5 conformance), so the sweep cannot MD5-check them; the fetch scripts still download/remux them, and they are excluded from the sweep's 315. They are instead cross-checked against ffmpeg's `libvpx-vp9`: the 12 tos/sintel clips (full-length movies, up to 1920x800) in full (all 268,832 displayed frames byte-identical), and the 7 bbb clips as a spot-check (first 1000 frames each, `tests/bbb_cross_check.rs`). |

## Tests & verification

```sh
cargo test                 # unit + integration tests
cargo clippy --all-targets
cargo fmt --check

# Optional bounded structure-aware fuzz campaign (release recommended).
VP9DEC_FUZZ_LONG_ITERS=10000 cargo test --release --test structured_fuzz_test -- --nocapture
```

`cargo test` runs the library's unit tests plus the integration tests under `tests/`. Anything
that needs the conformance corpus (`conformance_test`, `sweep_test`) or ffmpeg
(`synthetic_seg_test`'s cross-decode) skips cleanly when those aren't present, so the default run
stays green without any downloads.

### Getting the test vectors

Test vectors and their `.ivf.md5` files aren't committed (excluded via `.gitignore`). Fetch them
before running the conformance checks for real -- both scripts are idempotent and manifest-driven
(`scripts/vectors.txt`, the full official corpus, ~3.5 GB), downloading each vector and remuxing
the `.webm`-only ones to `.ivf` (no re-encode):

```sh
bash scripts/fetch-vectors.sh      # or: pwsh scripts/fetch-vectors.ps1
```

With the corpus present, the full sweep (the `315/315` above) runs via the release-only command
in Status / limitations; the default `cargo test` skips it and stays fast.

### External cross-decode (ffmpeg)

ffmpeg is not a dependency, but its `libvpx-vp9` and native `vp9` decoders act as independent
oracles for the synthetic segmentation vectors (`tests/synthetic_seg_test.rs`, covering
`SEG_LVL_ALT_L`/`REF_FRAME`/`SKIP` -- features no official vector exercises). The cross-decode
test finds ffmpeg via `$VP9DEC_FFMPEG` or `PATH` and skips cleanly if it's absent:

```sh
VP9DEC_FFMPEG=/path/to/ffmpeg cargo test --test synthetic_seg_test synthetic_streams_cross_decode_against_ffmpeg -- --nocapture
```

### PNG dump (for visual inspection)

`cargo run --example decode_to_png [-- <vector> <ivf-frame>]` decodes an `.ivf` and writes a
BT.601 RGB PNG to `target/dump/` (with its own from-scratch, dependency-free PNG encoder).

## History

[docs/history.md](docs/history.md) is a concise milestone-by-milestone record of how the
decoder was built (M1 through M7). Still-relevant design rationale, landmines, and
known gaps are in [docs/implementation-notes.md](docs/implementation-notes.md); the detailed,
change-by-change history is in the git log.

## License

MIT
