# oxideav-theora

[![CI](https://github.com/OxideAV/oxideav-theora/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-theora/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-theora.svg)](https://crates.io/crates/oxideav-theora) [![docs.rs](https://docs.rs/oxideav-theora/badge.svg)](https://docs.rs/oxideav-theora) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust Theora video codec — clean-room implementation, in progress.
The crate works on Theora packets directly (it does not parse the Ogg
page framing itself — that lives in the container crates), but ships
the complete codec side of Ogg carriage: the registration declares the
identification header's payload magic for registry-first stream
identification, `classify_packet` routes header vs. data packets per
§6.1, and the §A.2.3 granule-position mapping (forward, inverse, and a
mux-side tracker) is public API pinned against every data page of the
staged fixture corpus. The
decoder is sample-exact for its supported feature set — validated up to
HD against reference dumps, at all three pixel formats and across all
eight §7.4 macro-block coding modes against third-party-encoded
streams. The encoder emits complete self-describing
streams — §6 headers plus intra and inter (P) frames through a unified
rate-distortion mode decision over all eight §7.5.2 coding modes, with
predictor-seeded motion estimation to half-pixel accuracy,
rate-distortion-optimized quantization, measured-rate `MVMODE`
election, per-block skip, adaptive quantization, content-tuned Huffman
codebooks, non-macro-block-aligned picture regions, duplicate-frame
packets, and one-pass or two-pass target-bitrate rate control. The
round-453 quality campaign is **measured**: −21.0 % mean BD-rate /
+2.35 dB mean BD-PSNR over the six-sequence `rd_ladder` battery versus
the round-451 encoder (see the measured rate-distortion section), and
every re-pinned stream family is **externally validated** — muxed into
Ogg and black-box-decoded by an independent reference decoder
**pixel-exactly** against this crate's own reconstruction, with the
corpus pinned by SHA-256 under `tests/`.

## What works

The decode pipeline is wired end-to-end at the packet level. For the
supported feature set the decoder reconstructs frames **sample-exactly**
against reference output.

* **Headers** — identification (`decode_identification_header`), comment
  (`parse_comment_header`), and setup (`parse_setup_header` /
  `decode_setup_header`, including loop-filter limits, quantisation
  parameters, and the DCT-token Huffman tables).
* **Ogg carriage (codec side)** — the registration declares the §6.2
  identification header's leading bytes (`IDENTIFICATION_HEADER_MAGIC`)
  as the codec's payload magic, so a resolver-driven demuxer identifies
  a Theora logical stream from its first packet registry-first;
  `classify_packet` / `TheoraPacketKind` route de-framed packets per
  §6.1 (the three header types, reserved types `0x83`–`0xFF` to be
  ignored, video data including zero-byte duplicates); and the §A.2.3
  granule-position mapping is public API — `split_granule_position` /
  `join_granule_position`, `granule_position_for_frame` and its
  frame-index / seek-anchor / decodable-count inverses, pre-3.2.1
  (`VREV < 1`) normalization, `TheoraIdentHeader` methods that apply
  the stream's `VREV` and frame rate automatically
  (`granule_position_seconds`), and a mux-side `GranulePositionTracker`
  that turns keyframe flags into per-packet granule positions. The
  mapping is pinned against the granule value of **every data page of
  the fourteen staged `input.ogv` fixtures** (spanning `KFGSHIFT` 6, 7
  and 8, and zero-byte duplicate frames) and rehearsed end-to-end at
  all three pixel formats: encoder packet stream → blind
  classification → payload-magic resolution → granule tracking →
  registry-built decode.
* **Frame header & bitstream** — frame header decode, coded-block flags,
  macroblock modes, motion-vector decode (zero-MV and explicit per-block
  MV with the half-pixel / quarter-pixel split), block-level `qi`
  selection, EOB and coefficient tokens, and full DCT coefficient
  decode.
* **Reconstruction** — DC prediction and inversion, the inverse DCT
  (DC-only shortcut plus the full two-pass path), intra and
  motion-compensated inter reconstruction (with the §7.5.1 per-axis
  fractional-resolution motion vectors: half-pixel on luma /
  non-sub-sampled axes, quarter-pixel on each sub-sampled chroma axis,
  validated pixel-exact at HD), the complete frame-level reconstruction
  driver, and reference-frame promotion.
* **Loop filter** — the edge primitives, `lflim()` response, and the
  complete raster-order loop-filter driver (applied in place; collapses
  to identity at a zero filter limit). The full §6.4.1 limit range is
  exercised on real streams: the staged fixtures run limits 0 and 15,
  and two round-437 self-encoded streams — every limit at the 7-bit
  ceiling (`NBITS = 7`, `lflim()` at `L = 127` on every edge) and the
  all-zero table (`NBITS = 0`, sixty-four §5.2.5 zero-bit reads on the
  wire, filter skipped at every qi) — are externally black-box-decoded
  pixel-exactly against this crate's reconstruction.
* **Display crop** — `crop_frame_to_picture_region` (and the
  `FrameDecoder::crop_for_display` wrapper) crops the macroblock-aligned
  reconstruction down to the picture region, with the spec's chroma-axis
  rounding for all three pixel formats. A non-MB-aligned fixture
  (visible 26×18 inside coded 32×32) is validated end-to-end against a
  reference dump captured *after* the §2.2 crop, exercising the §4.4.4
  chroma round-up (13×9, not 13×8) on real reference pixels.

`FrameDecoder::decode_frame` chains the per-packet path
(header → block decode → reconstruction → in-place loop filter →
reference promotion) and is the high-level entry point. Empty (zero-byte)
packets are handled as duplicate-frame markers — except as the very first
packet, where there is no reference to duplicate: that is rejected with
`Error::FirstFrameEmptyPacket` rather than reconstructing a frame from the
zero-initialized reference store.

* **Intra encoder** — `FrameEncoder` turns a macro-block-aligned
  `SourceFrame` into a §7 video-data packet. The forward pipeline
  inverts the decoder's: the §7.9.3.3 forward DCT (`forward_dct_1d` /
  `forward_dct_2d`), forward quantization (`quantize_block`, the inverse
  of §7.9.2), forward §7.8 DC prediction (`forward_dc_prediction`), and
  the §7.7 token entropy writer over a new MSb-first `BitWriter`. The
  token writer uses the **full §7.7 token alphabet**: single-coefficient
  tokens 9–22, both pure zero-run tokens (3-bit token 7 for short gaps,
  6-bit token 8 for long ones), the combined run+value tokens 23–31
  (folding a zero gap and its terminating coefficient into one Huffman
  code), and **cross-block §7.7.1 EOB runs** (tokens 0–6): the writer
  simulates the §7.7.3 decoder's visit order once and coalesces every
  maximal run of consecutive block-end visits into a single EOB-run
  token — the rest of the run's blocks consume no bits at all, with
  runs freely crossing the `ti = 1` selector refresh and the
  luma/chroma boundary (runs past 4095 split greedily). It also
  **optimizes the §7.7.3 Huffman selectors independently per range**:
  the `ti = 0` pair only ever addresses group 0 (DC) and the re-read
  `ti = 1` pair groups 1–4 (AC), so the writer tallies the tokens
  actually written and picks each pair's cheapest table separately — a
  pure bit-rate win at zero distortion. Together these cut the intra
  measurement harness ~39% versus the round-379 writer at identical
  reconstruction. An intra, all-coded, single-`qi` frame emits no §7.3 /
  §7.4 / §7.5 / §7.6 bits, matching the decoder's intra short-circuits.
  An encoded INTRA frame decodes back through this crate's own
  `FrameDecoder` faithfully to within the quantizer step (a 32×32
  gradient round-trips with max luma error 5 / mean 0.27 at the weakest
  quantiser, chroma exact; a flat frame is lossless).

* **Adaptive block-level quantization** —
  `FrameEncoder::encode_intra_frame_with_qiis` is the first encoder
  path to emit a §7.1 multi-`qi` frame header (the `MOREQIS` / `QIS`
  chain, up to `NQIS = 3`) and the §7.6 block-level qi stream
  (`NQIS − 1` §7.2.1 long-run promotion passes, the exact inverse of
  the decoder's `decode_block_level_qi`): `qis[0]` drives every DC
  quantizer (the §7.6 preamble's rule, so DC prediction is untouched)
  and the loop-filter limit, while each block's AC quantizer is
  `qis[qiis[bi]]`. `encode_intra_frame_adaptive` layers a per-block
  rate-distortion chooser on top — each block is quantized once per
  candidate, reconstructed exactly as the decoder will, and scored
  `D + λ·R` — and `TheoraEncoder::with_adaptive_quant` drives every
  keyframe through it end-to-end via the framework `Encoder` trait
  (inter frames keep the single frame-level `qi`). Tests re-parse the
  emitted packets through the production §7.11 step-1 driver and pin
  the wire `QIS` / `QIIS` arrays exactly; the `NQIS = 1` case is
  byte-identical to the single-`qi` packet.

* **Content-tuned Huffman codebooks (two-pass encoding)** —
  `HuffmanTable::from_token_counts` builds a minimum-redundancy §6.4.4
  codebook from per-token emission counts (all 32 tokens leafed,
  canonical codes satisfying the Kraft equality — a full tree —
  deterministic ties, 16-bit length cap).
  `FrameEncoder::intra_token_statistics` tallies the tokens an intra
  encode would write (combined tokens and EOB runs included) into a
  `TokenStatistics` accumulator without producing a packet, and
  `SetupHeaderTables::with_tuned_huffman_tables` replaces, per group,
  the selector slot scoring worst on the sampled luma statistics with
  the luma-tuned codebook and the worst remaining slot with the
  chroma-tuned one — so content resembling the sample keeps the
  fallback it would actually pick. The tuned set serializes through
  `encode_setup_header`, keeping the stream self-describing;
  `TheoraEncoder::with_tuned_setup_keyframe_interval` wraps the whole
  two-pass flow in one constructor. Measured: a textured intra frame
  shrinks a further 19–23% versus the VP3-default tables at
  bit-identical reconstruction.

* **Round 387 — inter-side encoder quality.** Four additions drive the
  P-frame rate curves at equal reconstruction:

  * **Inter token statistics + mixed I/P GOP tuning** —
    `FrameEncoder::inter_token_statistics` tallies the §7.7 tokens an
    RD inter encode would write (EOB runs and combined tokens
    included) without producing a packet, and
    `SetupHeaderTables::with_gop_tuned_huffman_tables` tunes **four**
    codebooks per Huffman group (intra/inter × luma/chroma), each
    replacing the selector slot scoring worst on its own statistics
    column. `TheoraEncoder::with_gop_tuned_setup_keyframe_interval`
    runs the first pass as the exact I/P GOP the stream will be
    (inter frames planned by RD against a mirror decoder's
    references). Measured on a 6-frame textured motion sequence at
    interval 3: 2248 B (VP3 defaults) → 2141 B (intra-only tuning) →
    2072 B GOP-tuned, all three reconstructing bit-identically.
  * **Measured token rate everywhere** — `TokenBitCosts` prices every
    RD candidate's exact token plan against the stream's own codebooks
    (group-minimum code length + exact extra bits), replacing the flat
    bits-per-coefficient proxies in both the inter mode decision and
    the intra adaptive-quant chooser. The §7.4 mode writer likewise
    picks the cheapest **mode-coding scheme** per frame (direct
    scheme 7, the six fixed Table 7.19 alphabets, or a
    frequency-sorted scheme-0 custom alphabet — a golden-dominated
    31-MB mode section spells in 8 B vs 12 B direct).
  * **INTRA in the P-frame candidate set** — the RD decision weighs
    all **eight** §7.5.2 coding modes: an INTRA macro block codes from
    scratch (flat-128 predictor, qti = 0 matrices per §7.9.4 step
    2(d)ii) when neither reference predicts it. On a
    checkerboard→gradient quadrant switch: 26 B / luma SSD 224 vs the
    intra-less motion path's 149 B / SSD 4052.
  * **Inter adaptive quantization** —
    `FrameEncoder::encode_inter_frame_rd_adaptive` emits the §7.1
    `MOREQIS` chain + §7.6 block-level qi stream on P-frames, each
    coded block's AC quantizer chosen by `D + λ·R` on delivered SSD +
    measured rate (`TheoraEncoder::with_adaptive_quant` now covers
    both frame types); an adaptive `[0, 63]` spread lands strictly
    inside the single-`qi` rate/distortion corners (pinned in-test at
    equal reference state).
  * **Measured-rate keyframe policy (golden-frame refresh)** —
    `TheoraEncoder::with_keyframe_rate_policy(r)` converts a P-frame
    to a true keyframe when its references have decayed (size ratio
    past `r ×` the last keyframe, or a majority-INTRA mode plan) *and*
    the intra spelling wins a Lagrangian comparison on SSD measured
    through a throwaway mirror-decoder clone — re-seeding both
    references (only intra frames refresh the golden frame). A
    content-family switch at interval 30 converts the de-facto
    keyframes while staying within a pinned whole-stream Lagrangian
    bound of the interval-only spelling (a greedy per-frame policy
    buys its refreshed references at a small, bounded premium).

* **Inter (P-frame) encoder** — `FrameEncoder::encode_inter_frame`
  (zero-MV baseline) and `encode_inter_frame_motion` (with motion
  estimation) emit a §7 *inter* video-data packet that round-trips
  through this crate's own decoder. The forward pipeline drives the
  complete inter bit-stream chain: the §7.2.1 / §7.2.2 run-length
  bit-string encoders (`encode_long_run_bit_string` /
  `encode_short_run_bit_string`, exact inverses of the decoders), the
  §7.3 coded-block-flag encoder (`encode_coded_block_flags`, deriving
  the `SBPCODED` / `SBFCODED` / per-block streams from a `bcoded`
  array), the §7.4 macro-block-mode encoder (`MSCHEME = 7` direct
  modes), the §7.5 motion-vector encoder (the frame's `MVMODE` elected by
  measured rate — Table 7.23 Huffman components unless the
  fixed-length spelling is strictly cheaper), and the shared §7.7
  token writer. Each block is
  predicted from the **reconstructed** previous reference (the bytes
  the decoder holds), so an unchanged second frame reconstructs
  bit-exactly as an all-uncoded `INTER_NOMV` pure copy; a changed
  frame stays within the quantizer bound. The motion path runs a
  **predictor-seeded four-step whole-pixel descent** per macro block
  (the descent starts from the best of the zero vector, the running
  §7.5.2 `LAST1`/`LAST2` predictors, and the previous macro block's
  winner; steps 8, 4, 2, 1 around the running winner reach every
  displacement in ±15 pixels of the start with 33 probes, zero-biased,
  §7.5.1 ±31-component clamp, each probe loaded with `√λ ×` its
  Table 7.23 vector bits;
  a (6, −5) translation spells a 16 B motion packet where the zero-MV
  spelling needs 125 B) **then refines the winner to
  half-pixel accuracy** (§7.5.1): the integer search only spells even
  motion-vector components, so the eight half-pixel neighbours are
  probed and any strict SAD improvement kept, making the decoder's
  §7.9.1.3 two-tap half-pixel predictor reachable from the encoder (a
  linear ramp sampled half a pixel off the reference grid drops the
  whole-plane luma SAD from 1184 to 704, −40.5 %, at equal
  reconstruction). It codes the winning vector with
  `INTER_MV`, and forces its blocks coded (an uncoded inter block always
  copies the zero-MV colocated reference, so a non-zero MV requires
  coded blocks). A forward §7.5.2 LAST1 / LAST2 pass then recodes a
  macro block whose vector matches the running last vectors as the
  predicted `INTER_MV_LAST` / `INTER_MV_LAST2` mode (no explicit MV
  bits, identical reconstruction). A horizontally translated frame
  round-trips and the motion search measurably reduces the luma SAD
  versus the zero-MV baseline; a uniformly translated frame drives most
  macro blocks through the predicted-MV modes.

  Two further entry points emit the remaining inter modes:
  `encode_inter_frame_golden` searches **both** the previous and golden
  references per macro block and codes `INTER_GOLDEN_NOMV` /
  `INTER_GOLDEN_MV` when the golden reference predicts the block's four
  luma sub-blocks with the smaller SAD (reconstructing against the
  golden motion-compensated predictor), and `encode_inter_frame_four_mv`
  searches one vector **per luma block** and codes `INTER_MV_FOUR` when
  the four vectors disagree — each luma block carrying its own vector
  and each chroma block the §7.5.2 averaged vector for the pixel format.
  Both modes are correctly handled by the §7.5.2 LAST1 / LAST2 predictor
  history (golden excluded; four-MV updating LAST1 to its last coded
  luma vector). Each round-trips through this crate's own decoder: a
  golden copy of a re-shown reference reconstructs bit-exactly, and a
  per-luma-block-displaced frame reconstructs within the quantizer
  bound — so every §7.5.2 inter mode is now reachable from the encoder.

  A **unified rate-distortion mode decision**
  (`encode_inter_frame_rd`) replaces the fixed per-entry-point SAD
  heuristic with one joint per-macro-block Lagrangian choice. Each macro
  block evaluates every reachable uniform inter mode (`INTER_NOMV`,
  `INTER_MV`, `INTER_GOLDEN_NOMV`, `INTER_GOLDEN_MV`) by its true cost
  `D + λ·R` and codes the cheapest. The distortion `D` is the sum of
  squared errors between the source and the block the decoder will
  actually reconstruct — the encoder runs its own forward
  quantize → dequantize → inverse-DCT → clamp (`block_rd_cost`), so a
  candidate is scored on *delivered* fidelity rather than a
  pre-quantization SAD proxy; the rate `R` folds in the §7.4 mode code
  and §7.5 `MVMODE = 1` explicit-MV bits, weighted by a multiplier λ
  derived from the frame's **actual quantizer step** (λ ∝ step² on the
  §6.4.3 luma first-AC quantization value; multi-`qi` frames take the
  geometric mean of their candidate steps). External measurement —
  encode → Ogg mux → black-box reference decode → PSNR — calibrated
  the constant and caught the previous raw-`qi` ramp sloping the wrong
  way (a higher `qi` is a *smaller* step in Theora): the I+P
  rate/PSNR curve used to fold over above qi 52 (44.8 → 39.0 dB at
  qi 63 with rate still rising) and is now monotone on both axes
  (qi 63 at 51.4 dB for +4.5 % bytes).
  This subsumes the raw-SAD previous-vs-golden choice into one decision
  on reconstructed distortion; `INTER_MV` winners still flow through the
  §7.5.2 `INTER_MV_LAST` / `INTER_MV_LAST2` recode. The RD rate term is
  **predictor-aware**: the decision tracks the running `LAST1` / `LAST2`
  motion-vector predictor and charges an `INTER_MV` candidate zero
  explicit-MV bits when its vector matches the predictor (it will recode
  to a no-MV-bit LAST mode), so a vector the predictor already supplies
  for free is not over-penalised against `INTER_NOMV`. **`INTER_MV_FOUR` is
  part of the same candidate set**: each macro block also evaluates a
  per-luma-block search (four independent vectors, chroma averaged) by its
  true `D + λ·R` cost and codes four-MV when it beats every uniform mode —
  its four explicit vectors are paid for out of the rate term, so it is
  chosen only when the per-block fidelity gain justifies them (an
  all-equal four-MV is strictly dominated by uniform `INTER_MV` and never
  wins). On a scene the golden
  reference predicts perfectly the RD path codes a distortion-free golden
  copy (SSD 0, 25 B) where the previous-reference-only motion path must
  code a large residual against the unrelated previous reference
  (SSD 50264, 252 B) — a measured strict win on both distortion and
  rate. The mode decision has **no bitstream-syntax effect**: every
  chosen mode emits the same §7 bytes the existing writers produce.
  `TheoraEncoder` drives its P-frames through this RD path by default;
  `with_inter_mode` selects an alternative `InterModeStrategy` —
  `PreviousMotion` (previous-reference SAD), `GoldenMotion` (previous-vs-
  golden raw-SAD), or `FourMv` (per-luma-block four-MV search) — each
  reachable end-to-end through the framework `Encoder` trait.

  A **per-block rate-distortion skip decision** completes the coded /
  uncoded choice: in an `INTER_NOMV` macro block the decoder's uncoded
  path is a bit-exact zero-MV previous copy — exactly the predictor
  the encoder built — so a block whose quantized residual survives
  quantization may still legally *skip*. Every inter path scores both
  spellings by the frame's Lagrangian (skip: predictor SSD, zero bits;
  code: delivered SSD + λ·measured token bits) and keeps the cheaper
  one; `block_rd_cost` applies the same min inside the mode decision.
  A noise-only P-frame (±3, residuals surviving a mid-strength qi-40
  quantizer) drops from ~140 B to 5 B and reconstructs as a bit-exact
  reference copy, while a strongly changed block in the same conditions
  still codes and tracks the source (at the *weakest* quantizers the
  step-derived λ correctly prefers coding the noise — bits are cheap
  in that regime). When the skip decision empties a
  `TheoraEncoder` P-frame entirely (a **duplicate frame**), the
  encoder emits the spec's own syntax for that: a §7.11 step-2
  zero-byte packet, decoding bit-exactly as a previous-frame copy.

* **Framework `Decoder` integration** — `TheoraDecoder` implements
  `oxideav_core::Decoder`, and `register` installs it into a
  `RuntimeContext` (reachable through the shared `CodecRegistry`). Packets
  are dispatched by their §6.1 type bit: header packets (`0x80` / `0x81`
  / `0x82`) advance header collection, video-data packets are decoded,
  §2.2-cropped to the picture region, and flipped to top-down raster as
  an `oxideav_core::VideoFrame`. Headers may arrive inline as leading
  stream packets, via `TheoraDecoder::with_headers`, or as a
  length-prefixed `CodecParameters::extradata` chain parsed by the
  `make_decoder` factory.

* **§6 header-packet serialization** — `encode_identification_header`
  (§6.2), `encode_comment_header` (§6.3), and `encode_setup_header`
  (§6.4) are the exact inverses of the three header decoders. The
  identification serializer is *byte-exact* (re-encoding a decoded ident
  packet reproduces the original payload); the setup serializer writes
  the §6.4.5 body (LFLIMS with minimal `NBITS`, the §6.4.2 quantization
  parameters as fresh `NEWQR=1` ranges, and all 80 §6.4.4 Huffman tables
  via a pre-order tree walk) onto one shared `BitWriter` and round-trips
  the full setup tables back to an equal `SetupHeaderTables`. The full
  §6.4.5 bundle can also be **synthesized from the published Appendix B
  data** — `SetupHeaderTables::vp3_defaults()` assembles the §B.2 / §B.3
  scale tables, the §B.4 base matrices + single-range quant assignment,
  and the §B.4 80 DCT-token Huffman codebooks (`HuffmanTable::from_code_list`
  rebuilds each codebook into the byte-identical §6.4.4 tree), so the
  encoder can emit a conformant setup header with no caller-supplied
  tables.

* **Framework `Encoder` integration** — `TheoraEncoder` implements
  `oxideav_core::Encoder`, and `register` now installs it (alongside the
  decoder) via the `make_encoder` factory (which needs only the §6.2
  identification header in `extradata` — a §6.4 setup header there is
  optional, synthesized from the VP3 defaults when omitted). The encoder
  serializes the three §6 headers up front and emits them as the first
  three packets
  (flagged `header`), then turns each top-down `VideoFrame` — at the
  coded dimensions or at the §2.2 picture dimensions (see the
  picture-region bullet below) — into one §7 data packet. The keyframe
  interval (`-g`,
  `TheoraEncoder::with_keyframe_interval`, default 1) decides intra vs
  inter: interval-boundary frames are intra keyframes, the frames
  between are inter (P) frames predicted from the reconstructed previous
  reference. Optional **scene-cut detection**
  (`with_scene_cut_threshold`) inserts a fresh keyframe mid-GOP when the
  mean absolute luma difference between the incoming source and the
  previous reconstruction exceeds the threshold **and** more than
  doubles the GOP's running average difference — the relative gate
  separates a cut (a one-frame spike) from steadily fast-moving
  content, which an absolute threshold alone converted to a keyframe
  storm (externally measured: the gated spelling of a moving-texture
  stream is 33 % smaller *and* 1.4 dB better); a scene change makes
  inter prediction worthless, so an intra frame is both smaller and
  resets the references; the keyframe-interval counter restarts from the
  inserted keyframe. The encoder mirrors its own output through an internal
  `FrameDecoder` so the reference it predicts from is byte-identical to
  the downstream decoder's. `output_params` carries the picture
  dimensions (equal to the coded ones for a full-frame picture region),
  the mapped pixel format, and a length-prefixed extradata
  header chain, so a complete encode → decode loop round-trips through
  both `oxideav_core` traits and through the shared `CodecRegistry`
  (`first_encoder` → `first_decoder`): a flat frame is lossless, a
  gradient stays within the quantizer bound, and an I,P,P interval-3
  sequence reconstructs every frame within the quantizer bound.

* **Picture-region (non-MB-aligned) encoding** — the encode-side
  counterpart of the display crop. `TheoraIdentHeader::for_picture`
  builds a §6.2 header around arbitrary picture dimensions (smallest
  containing coded frame; region left- and top-aligned — `PICX = 0`,
  `PICY = 16·FMBH − PICH` in the spec's lower-left coordinates — the
  placement observed on the reference-captured fixtures), with
  `picture_plane_dims` exposing the §4.4.4 chroma round-up per pixel
  format, and a container-carriable `KFGSHIFT` default of 6 (a zero
  shift leaves inter frames with no representable §A.2.3 granule
  position — the field stays public for longer GOPs). `SourceFrame::from_picture` pads picture-shaped planes to
  the coded frame by edge replication (§2.2 leaves the outside-region
  samples to the encoder; replication keeps the padding blocks'
  residual energy near zero), and `TheoraEncoder::send_frame` accepts
  picture-shaped `VideoFrame`s directly — exactly the shape
  `TheoraDecoder` emits. A 26×18 picture (coded 32×32, the decode
  fixture's geometry) intra- and inter(RD)-encodes and crops back to
  picture planes faithful to the source across all three chroma
  formats, including the odd-edge 13×9 chroma window at 4:2:0.

* **Target-bitrate rate control** — `TheoraEncoder::with_target_bitrate`
  (and the `with_target_bitrate_bounded` variant for explicit `qi`
  bounds) enables a leaky-bucket rate-control loop that adapts the
  frame-level quantization index before each frame to steer the running
  output size toward a byte budget. Each frame is allotted
  `target_bits / fps` bits (the frame rate read from the §6.2 `FRN / FRD`
  header); the actual coded size feeds a signed fullness accumulator, and
  a proportional, clamped step moves `qi` to oppose the imbalance — a
  stream running ahead of budget *lowers* `qi` (stronger quantization,
  smaller frames; recall a lower `qi` is stronger in Theora), one running
  behind *raises* it (better fidelity). The chosen `qi` lands in the
  frame header `QIS[0]`, so the adaptation has **no bitstream-syntax
  effect**: a downstream decoder reads each frame exactly as it would a
  fixed-`qi` stream. The loop is **keyframe-aware**: because an intra frame
  is typically several times larger than the inter frames around it,
  charging it against a single per-frame budget would spike the bucket and
  slam the quantizer at every GOP boundary; instead a keyframe drains a
  weighted budget (`keyframe_weight × bits_per_frame`) and the surplus is
  repaid in equal shares across the GOP's inter frames, so the long-run
  average target is preserved exactly while a keyframe no longer perturbs
  the frames after it (the controller learns the GOP length from the
  encoder's keyframe interval). The fullness accumulator is **anti-windup
  clamped** to the ±8 per-frame budgets the proportional step can act
  on, so a stretch of content far under budget cannot bank unbounded
  credit (externally measured at a reachable 400 kb/s target: a busy
  stretch after 3 s of static content ran >2× the target for 3+
  seconds unclamped, and is back inside regulation within one second
  clamped). The loop is fully opt-in (disabled by
  default, a no-op) and **composes with adaptive quantization**: on a
  stream built with both `with_adaptive_quant` and a target bitrate,
  the rate-control loop owns each frame's `QIS[0]` (the frame-level
  quantizer driving the DC quantizers, loop-filter limit, and RD λ)
  while the caller's candidates ride along as the per-block AC
  alternatives — the bucket steers each frame's base rate and the
  per-block `D + λ·R` chooser still redistributes bits within the
  frame. **Externally measured** (5-second 320×240 runs
  decoded black-box from the muxed `.ogv`): a 400 kb/s target lands
  +0.5 % and an 800 kb/s target −0.9 %, while a target below the
  content's `qi_min` rate floor saturates cleanly at the floor. Enabling the loop
  also **declares the target in the §6.2 `NOMBR`** nominal-bitrate
  header field (saturating at the 24-bit "`2^24 − 1` or greater"
  ceiling), rewriting the queued ident packet and the advertised
  extradata chain so both header carriage paths agree. A **two-pass**
  mode (`two_pass_stats` + `with_two_pass_rate_control`) splits the
  whole-stream budget in proportion to each frame's *measured*
  first-pass share and seeds the loop at the §B.3 `ACSCALE`-scaled
  prediction of the target-rate quantizer — measured at 250 kb/s
  targets: mean |rate error| 15.4 % → 8.4 % and +0.9 dB mean luma
  PSNR versus the one-pass loop (see the measured rate-distortion
  section).

End-to-end fixtures decoded sample-exactly cover intra-only streams,
intra-then-inter sequences, explicit motion vectors, custom quantisation
tables, weakest- and strongest-quantiser streams, a non-MB-aligned
picture region cropped to its visible 26×18 window (compared against a
post-crop reference dump), a single-frame bitstream-version-3.2.1
(alpha3+) stream, a sustained multi-frame keyframe-interval run, an
all-keyframe four-packet `-g 1` run (every frame INTRA, each re-seeding
both references), and a
grayscale-source `monochrome-via-zero-chroma` I+P stream whose chroma
planes stay a flat `0x80` through both the coded-residual and pure-copy
inter branches (asserted in addition to the sample-exact pixel match).

Three third-party-encoded fixture families close the decode side's
last staged-corpus gaps **pixel-exactly** (per-frame SHA-256s plus the
whole-stream `expected.yuv` digest from each fixture's `notes.md`):

* `pixel-format-422-64x64` / `pixel-format-444-64x64` — the corpus's
  first third-party `PF = 2` and `PF = 3` streams (every earlier
  non-4:2:0 validation decoded this crate's *own* encoder output).
  They pin the §2.3–§2.4 super-block/block counts (`NBS` 128 / 192),
  the §4.4 block-to-plane mapping, and the horizontally-only (32×64)
  versus unsubsampled (64×64) chroma geometry, through both the direct
  `FrameDecoder` path and the framework `Decoder` trait.
* `all-mb-modes-64x64` — 24 frames (one interval-250 keyframe + 23
  inter) whose §7.4 census spans **all eight coding modes**, asserted
  exactly (173 / 104 / 45 / 50 / 5 / 1 / 1 / 5): the corpus's only
  third-party-encoded `INTER_GOLDEN_NOMV`, `INTER_GOLDEN_MV`, and
  `INTER_MV_FOUR` macro blocks, decoded against a golden frame that is
  23 frames stale by the stream's end — pinning golden-reference
  selection, per-luma-block four-MV decode with §7.5.2 chroma
  averaging, and the LAST / LAST2 vector history across the corpus's
  only long inter run with real motion.

The `dimensions-1080p-very-short` fixture (coded 1920×1088, visible
1920×1080, two frames I then P) is decoded end-to-end and validated
against every invariant the staged instrumented trace records
bit-exactly: the HD §6.2 geometry (`NMBS = 8160`, `NSBS = 3060`,
`NBS = 48960`), both §7.1 frame headers, the §6.4.1 loop-filter limit,
the §2.2 visible-crop dimensions, and the §7.4 frame-1 macro-block
modes — not just the 5-bucket histogram but **every one of the 8160
macro blocks** pinned at its exact super-block coded-order position
against the trace (mode index + derived MB-coded flag), so a raster /
Hilbert-order / mode-decode-position regression can no longer pass a
bucket-total check. The decoded mode set spans the full inter range
(`INTER_NO_MV`, `INTER_MV`, `INTER_MV_LAST`, `INTER_MV_LAST2`). It is
also pinned **pixel-exactly**: the SHA-256 of
the two concatenated cropped display frames equals the `c48344b1…`
digest recorded in the fixture's `notes.md`. Frame 0 is the all-intra
keyframe; frame 1 exercises the §7.9.4 inter path including the §7.5.1
quarter-pixel chroma motion compensation — the band of nonzero-MV
chroma blocks that earlier diverged is now sample-exact.

## Not yet supported

* **Ogg page framing** — parsing/writing `OggS` pages stays out of
  scope here (it lives in the container crates); this crate supplies
  the codec side of the carriage contract — payload magic, §6.1
  packet classification, and the §A.2.3 granule mapping — and works
  on pre-de-framed packets.
* **Encoder mode coverage is complete** — every §7.5.2 inter mode
  (`INTER_NOMV`, `INTER_MV`, `INTER_MV_LAST`, `INTER_MV_LAST2`,
  `INTER_GOLDEN_NOMV`, `INTER_GOLDEN_MV`, `INTER_MV_FOUR`) is reachable
  from the encoder, and **all** of them — including `INTER_MV_FOUR` (a
  per-luma-block search, chroma averaged) — are chosen by the one joint
  rate-distortion decision (`encode_inter_frame_rd`, the `TheoraEncoder`
  P-frame default — see the inter-encoder bullet above). A
  **target-bitrate rate-control loop** is now wired in too (see the
  rate-control bullet above). The encoder also **synthesizes its own
  §6.4 setup header** from scratch
  (`SetupHeaderTables::encoder_defaults()`: the §B.2 loop-filter
  limits, §B.3 AC/DC scale tables, §B.4 base matrices — the two intra
  ones flattened by the round-457 measured election, see below — with
  their single-range quant assignment, and the §B.4 80 DCT-token
  Huffman codebooks; `vp3_defaults()` keeps the published Appendix B
  data verbatim), so `TheoraEncoder::with_default_setup` needs only
  the identification header and a quantizer to emit a complete
  self-describing stream; a caller may still supply pre-decoded tables
  via `TheoraEncoder::new` / `extradata` when matching an existing
  setup.
* **Pre-3.2.0 (legacy) bitstreams** — the §A.2.3 pre-3.2.1 granule
  normalization is implemented, but no *decodable* `VREV < 1` fixture
  exists: the staged catalogue
  (`docs/video/theora/third-party-corpora.md` §3.1) records a live
  `0x030100` stream whose pre-3.2.0 identification header uses a
  different layout, and no `expected.yuv` can currently be derived
  for it, so it remains a parser-fixture candidate only (a
  documented corpus gap, not a crate gap). The external catalogue's
  fetch-and-test families (§1–§3: large pictures with `PICX`/`PICY`
  offsets up to 128/144, chained logical streams, variable frame
  rate, non-square pixels) are likewise catalogued by URL + SHA-256
  rather than staged, and container-level concerns among them
  (chaining, page framing) belong to the container crates.

## Measured rate-distortion (round 457)

`tests/bd_rate.rs` pins the crate's Bjøntegaard battery: four
deterministic 176×144 scenes (`square0`, `blobs`, `pan`, `cut` from
`tests/common/rd.rs`, 24 frames, keyframe interval 16) across the
qi 8/20/32/44/56 ladder, each stream decoded back through this crate's
own decoder and scored on bytes, luma PSNR, chroma PSNR and luma
SSIM. Every scene must hold two pins — no luma / chroma BD-rate
regression against the round-453 reference curves below, and the
campaign floor recorded in the test — and the same harness drives
`examples/rd_ladder.rs` (now with an SSIM column and chroma / SSIM
BD-rates against a `--save` reference). No reference *encoder* binary
exists in this environment (`ffmpeg` ships only the Theora decoder), so
the battery measures this encoder against its own prior rounds; the
black-box reference *decoder* validates every re-pinned stream
byte-exactly (see `tests/encoded-corpus-notes.md`).

Final round-457 operating points (bytes / luma PSNR dB, 24 frames,
keyframe interval 16, encoder-default setup tables):

| sequence | qi 8 | qi 20 | qi 32 | qi 44 | qi 56 |
|---|---|---|---|---|---|
| square0 | 9266 / 33.29 | 11188 / 39.94 | 13210 / 44.68 | 15820 / 47.99 | 21776 / 51.35 |
| blobs | 2423 / 34.49 | 4442 / 37.06 | 7542 / 38.79 | 11117 / 39.73 | 22015 / 43.19 |
| pan | 8377 / 30.45 | 12869 / 33.16 | 20446 / 35.21 | 49426 / 36.55 | 123498 / 39.80 |
| cut | 5768 / 33.89 | 7725 / 38.43 | 10361 / 40.80 | 14261 / 42.08 | 23264 / 44.39 |
| fx-all-mb-modes | 2899 / 28.32 | 5688 / 32.10 | 10551 / 35.73 | 17309 / 40.50 | 23487 / 42.98 |
| fx-kf-interval-30 | 283 / 26.34 | 441 / 30.72 | 629 / 38.73 | 791 / 42.08 | 1064 / 45.95 |

Cumulative round-457 gains versus the round-453 encoder (Bjøntegaard
piecewise-linear deltas over the qi ladder; luma rate, chroma rate,
luma PSNR):

| sequence | BD-rate Y | BD-rate C | BD-PSNR Y |
|---|---|---|---|
| square0 | −24.2 % | −16.2 % | +6.24 dB |
| cut | −20.8 % | −8.5 % | +1.90 dB |
| pan | −17.6 % | −12.9 % | +0.64 dB |
| blobs | −0.5 % | +1.2 % | +0.12 dB |
| fx-all-mb-modes | −0.8 % | −5.1 % | +0.06 dB |
| fx-kf-interval-30 | −28.0 % | −12.5 % | +4.72 dB |
| **mean** | **−15.3 %** | **−9.0 %** | **+2.28 dB** |

Per-change contributions this round (luma BD-rate vs the round-453
encoder, mean over the six ladder sequences, measured incrementally):

* RDOQ end-of-block search + corrected transform-gain distortion model
  (1/16, was ≈ 7/64): −2.0 % luma / −1.4 % chroma, +0.22 dB BD-PSNR
  (`pan` −7.5 %, `square0` −3.0 %; the noise-textured `blobs` +1.1 %).
* Motion search: `LAST1` / `LAST2` as explicit rate-distortion
  candidates −2.6 %, priced `INTER_MV_FOUR` block searches −0.4 %,
  iterated half-pixel refinement −0.8 % (cumulative −5.7 % luma /
  −5.7 % chroma, +0.70 dB BD-PSNR vs round 453).
* Lookahead (`with_lookahead(n)`, off by default and byte-identical
  when off): keyframes land on cuts seen by a two-sided detector and
  an interval keyframe due just before a cut is deferred onto it
  (`cut` content with the cut at frame 18, interval 16: {0, 16, 18} →
  {0, 18}, −17 % bytes at +0.5 dB). Under rate control the window's
  budget is shared by an online per-type log-linear rate fit — the
  two-pass mechanism without the probe pass: 96-frame runs at
  120/150/180 kb/s reach 37.08 dB at 2.9 % mean |rate error| against
  one-pass 36.26 dB / 2.2 % and two-pass 38.72 dB / 1.1 %.
  `with_vbv_buffer(bits)` simulates a decoder buffer across the window
  and re-codes an oversized frame; a 40000-bit buffer at 150 kb/s
  never underflows on the battery (`vbv_min_level_bits()`).
* Quantization matrices: the encoder's default setup header
  (`SetupHeaderTables::encoder_defaults()`) flattens the two intra
  base matrices three quarters of the way toward their first AC entry
  — elected by a `--flat n/d --flat-bm a,b` sweep: −10.2 % luma /
  −3.7 % chroma BD-rate (+1.65 dB) against the §B VP3 tables with every
  sequence improving; flattening the inter matrix cost +1.2 % and is
  not done. Cumulative vs round 453: **−15.3 % luma / −9.0 % chroma,
  +2.28 dB BD-PSNR** (`square0` −24.2 %, `cut` −20.8 %, `pan` −17.6 %,
  `blobs` −0.5 %).
* Speed (byte-identical): stack token plans, predictor fast paths,
  hoisted source extraction and early-abandoned search SADs — 1.5×
  faster encode+decode on every ladder scene (176×144 `pan`
  ≈ 94 → 140 fps, `square0` 164 → 245 fps), output unchanged.
* Activity masking (`with_activity_masking(1|2)`, opt-in, needs a
  multi-`qi` list): −0.35 % / −0.59 % luma SSIM-rate for +0.15 % /
  +0.75 % PSNR-rate against `with_adaptive_quant_auto` alone — a small
  perceptual trade. (`with_adaptive_quant_auto` itself measures +2.9 %
  luma BD-rate and +6.9 % SSIM-rate against the single-`qi` default on
  this battery, so neither is on by default.)

Both elections that closed round 453 were re-run on the finished
round-457 encoder and stand: a uniform rescale of the §6.4.1
loop-filter limits (×½ −0.3 % luma but +0.6 % chroma and −0.10 dB;
×1.5 +2.4 %; ×2 +5.8 %) and the RD λ divisor (36 → +0.8 %, 42 → +0.2 %,
56 → +0.3 %, 64 → +0.2 % against 48).

## Round-453 reference curves

`examples/rd_ladder.rs` (now over `tests/common/rd.rs`) is the
crate's reproducible measurement harness: four deterministic 176×144
synthetic sequences (`square0` gradient + moving square, `blobs`
sub-pixel drifting blobs over fixed-pattern noise, `pan` a
half-pixel-per-frame textured pan, `cut` a mid-stream scene change)
plus two fixture-derived sources (`all-mb-modes-64x64`,
`keyframe-interval-30`), encoded at interval 16 across the five-point
quantizer ladder and decoded through this crate's own decoder.
Reproduce with `cargo run --release --example rd_ladder -- --fixtures
docs/video/theora/fixtures [--ref saved.tsv] [--twopass] [--lookahead n]
[--vbv bits] [--frames n] [--lfscale n/d] [--flat n/d] [--vp3]`.

Round-453 operating points, the reference the battery's floors are
measured against (bytes / luma PSNR dB, 24 frames, VP3 tables):

| sequence | qi 8 | qi 20 | qi 32 | qi 44 | qi 56 |
|---|---|---|---|---|---|
| square0 | 10358 / 29.23 | 13168 / 36.63 | 15044 / 41.11 | 18046 / 44.45 | 21822 / 48.46 |
| blobs | 2562 / 34.77 | 4675 / 37.33 | 7801 / 39.03 | 11938 / 39.94 | 18799 / 40.94 |
| pan | 9298 / 30.63 | 17068 / 33.65 | 29852 / 35.56 | 64926 / 36.77 | 139046 / 39.07 |
| cut | 6378 / 31.41 | 8992 / 37.24 | 11653 / 40.02 | 15873 / 41.67 | 22266 / 43.21 |
| fx-all-mb-modes | 3280 / 29.08 | 6554 / 33.00 | 11373 / 36.35 | 17511 / 39.94 | 23845 / 43.34 |
| fx-kf-interval-30 | 237 / 23.19 | 425 / 26.97 | 597 / 30.53 | 816 / 35.66 | 904 / 41.45 |

Round 453's own gains versus the round-451 encoder (history;
Bjøntegaard-style piecewise-linear deltas over the qi ladder, luma):

| sequence | BD-rate | BD-PSNR |
|---|---|---|
| square0 | −32.4 % | +7.92 dB |
| pan | −40.2 % | +1.58 dB |
| cut | −24.3 % | +2.36 dB |
| fx-all-mb-modes | −17.5 % | +1.44 dB |
| blobs | −7.6 % | +0.22 dB |
| fx-kf-interval-30 | −4.2 % | +0.56 dB |
| **mean** | **−21.0 %** | **+2.35 dB** |

Per-change contributions, measured incrementally in round order: the
§7.5.1 quarter-pixel chroma-predictor correction −10.8 % mean BD-rate
(and chroma PSNR curves un-inverted: `pan` 23.5 → 50.2 dB at qi 56);
Table 7.23 `MVMODE = 0` vector coding + measured MV rate −1.6/−1.7 %
on the motion-heavy sequences; rate-distortion-optimized quantization
−9.0 % mean (+0.72 dB); predictor-seeded motion search −2.4 % mean;
selected-selector token re-pricing −0.5 % mean (every sequence
improved). No reference *encoder* binary is installed in this
environment (`ffmpeg` ships only the Theora decoder), so the ladder
measures this encoder against its own prior rounds; the black-box
reference *decoder* validates every stream byte-exactly.

Two-pass rate control at 250 kb/s targets (24-frame sequences, vs the
one-pass loop): mean |rate error| 15.4 % → 8.4 %; luma PSNR square0
44.40 → 47.09 dB, cut 42.10 → 44.05 dB, blobs 40.84 → 41.55 dB, pan
35.27 → 35.04 dB (mean +0.9 dB). Rate-floor / rate-ceiling targets
saturate cleanly at the qi bounds under both loops.

Two measured elections close the round: a uniform rescale sweep of
the §6.4.1 loop-filter limit table (×0, ×½, ×1, ×1.5, ×2 across
qi 8–44) leaves the §B.2 VP3 defaults at or near the optimum (×2
buys ≤ +0.6 dB on `square0` at qi 44 but loses at qi 8 and on
`fx-all-mb-modes`, ×0 loses everywhere) — the defaults stand; and the
keyframe-interval sweep (4/8/16/30 at qi 20/44) shows interval 16–30
dominating coherent content (interval 4 costs 25–55 % more bytes at
equal-or-lower PSNR), with the scene-cut and measured-rate keyframe
policies covering content changes — the `TheoraEncoder` guidance is
interval 16–30 plus those policies, not short intervals.

## Robustness and fuzzing (round 437, extended round 457)

The decode surface is fuzzed by a `fuzz/` sub-crate with six
libFuzzer targets (round 457 added `setup_header_pair`, a structured
writer/parser target: fuzz-perturbed *valid* §6.4 tables — 7-bit
loop-filter limits including the `NBITS = 0` table, 16-bit scale
ladders, up to eight base matrices, fresh multi-range quant layouts —
must serialize, parse, decode back equal, re-serialize
byte-identically and evaluate §6.4.3 everywhere; the encoder→decoder
contract target now also drives lookahead, VBV and activity masking;
bounded sessions of all six this round found nothing): arbitrary bytes at the three §6 header decoders
(with the crate's own serializers as self-inverse oracles — an
accepted ident header re-encodes byte-exactly, accepted setup tables
survive an encode → decode fixpoint), hostile §7 packet chains
against a live `FrameDecoder` at all pixel formats and
non-MB-aligned geometries, the `make_decoder` extradata walk plus
inline-header routing through the `oxideav_core::Decoder` trait, the
§A.2.3 granule-mapping algebra, and an encoder→decoder contract
target on fuzz-derived streams. The first sessions (~27M runs)
surfaced one real decode defect — §6.4.2 step 7(a)ivG transcribes
the non-initial `QRBMIS` reads without step 7(a)ivC's `NBMS` range
check, so a syntactically valid setup header could reference an
undefined base matrix and panic §6.4.3 — now rejected with a typed
error and regression-pinned at both the parameter and wire level.
Deterministic sweeps complement the fuzzers: every possible
truncation of the reference-encoded fixture packets (intra on a
fresh decoder, inter on a decoder holding the fixture's real
reference) and 6400 corruption-storm mutants across self-,
reference- and third-party-encoded spellings must return `Ok` or a
typed `Err`, never panic. Round 444 extended the truncation sweep to **every
data packet of the three third-party-encoded fixtures** — each
prefix decoded against the exact reference state the full stream
produced up to that packet — so the §5.2 end-of-packet discipline
now crosses the 4:2:2 / 4:4:4 block geometry and the golden /
four-MV mode syntax at every possible packet-end boundary.

## External validation (round 413)

The encoder's output is validated by an **independent reference
decoder**, black-box, end-to-end: twelve scenario families — plain
I/P GOPs at 320×240 and 1920×1080, 4:2:2 and 4:4:4, a non-MB-aligned
130×98 picture region, multi-`qi` adaptive quantization, target-bitrate
rate control (with the `NOMBR` declaration), GOP-tuned custom Huffman
setup headers, scene cuts, the four-MV and golden inter strategies, and
zero-byte duplicate-frame packets — were muxed into `.ogv` through the
published `oxideav-ogg` crate (in a throwaway helper project; this
crate itself takes no container dependency), passed `oggz-validate`,
and decoded to raw video that matched this crate's own reconstruction
**byte-for-byte in all twelve cases**. The same scenario family is
pinned by SHA-256 (wire packets + decoded output) in
`tests/encoded_corpus.rs`, with the route, verdicts, and measured
operating points (externally-measured rate/PSNR curves and
rate-control accuracy within ±1 % of reachable targets) documented in
`tests/encoded-corpus-notes.md`. Round 437 added three
**decode-corner** scenarios through the same route — loop-filter
limits at the 7-bit ceiling, the all-zero `NBITS = 0` limit table,
and a three-quant-range transmitted layout with adaptive-quant qis
spanning the ranges (§6.4.3 interpolation across interior boundaries
of a non-VP3 layout) — all three black-box-decoded byte-identically
to this crate's reconstruction. Round 444 added the **composed
rate-control + adaptive-quant** scenario (a stream shape that did
not previously exist — see the rate-control bullet) through the
same route, also byte-identical (16/16 scenarios cumulative). The round-413 external route surfaced and fixed
three real defects: `for_picture`'s `KFGSHIFT = 0` (inter
frames had no representable Ogg granule position), a wrong-slope RD λ
(the rate/PSNR curve folded over above qi 52), and a scene-cut
detector that converted steady motion into keyframe storms. Round 453
re-ran the route at **every corpus re-pin** (four times — chroma-split
fix, MV coding, RDOQ + seeded search, selector re-pricing): all
**15/15 scenarios byte-identical** each time, and two new stream
shapes (a two-pass rate-controlled stream and an auto-adaptive-quant
stream) were validated the same way (see
`tests/encoded-corpus-notes.md`).

## Usage

```rust
use oxideav_theora::{decode_identification_header, decode_setup_header, FrameDecoder};

# let (ident_packet, setup_packet, data_packet): (&[u8], &[u8], &[u8]) = (&[], &[], &[]);
// `ident_packet`, `setup_packet`, and the data packets are de-framed
// from the container by the caller.
let ident = decode_identification_header(ident_packet)?;
let setup = decode_setup_header(setup_packet)?;
let mut dec = FrameDecoder::new(ident, setup)?;
let frame = dec.decode_frame(data_packet)?;
let display = dec.crop_for_display(&frame)?;
# Ok::<(), oxideav_theora::Error>(())
```

Alternatively, drive the decoder through the `oxideav_core::Decoder`
trait. `TheoraDecoder` accepts the three §6 header packets (inline, via
`with_headers`, or as a length-prefixed `extradata` chain) and then
emits a top-down `VideoFrame` per video-data packet:

```rust
use oxideav_core::{CodecId, Decoder, Packet, TimeBase};
use oxideav_theora::{TheoraDecoder, THEORA_CODEC_ID};

# fn run(ident: &[u8], setup: &[u8], data: &[u8]) -> oxideav_core::Result<()> {
let mut dec = TheoraDecoder::with_headers(
    CodecId::new(THEORA_CODEC_ID),
    &[ident, setup],
)
.map_err(|e| oxideav_core::Error::invalid(e.to_string()))?;
dec.send_packet(&Packet::new(0, TimeBase::from_rate(1), data.to_vec()))?;
let _frame = dec.receive_frame()?; // Frame::Video, top-down planes
# Ok(())
# }
```

To encode an intra frame, build a `FrameEncoder` from the same header
objects and a quantization index, then hand it a `SourceFrame` (three
planes, lower-left row-major, at the coded macro-block-aligned
dimensions). The produced packet decodes back through `FrameDecoder`:

```rust
use oxideav_theora::{FrameEncoder, FrameDecoder, SourceFrame};
# fn run(ident: oxideav_theora::TheoraIdentHeader,
#        setup: oxideav_theora::SetupHeaderTables,
#        frame: SourceFrame) -> Result<(), oxideav_theora::Error> {
let enc = FrameEncoder::new(ident.clone(), setup.clone(), /* qi */ 32)?;
let packet = enc.encode_intra_frame(&frame)?;
let mut dec = FrameDecoder::new(ident, setup)?;
let _decoded = dec.decode_frame(&packet)?; // faithful to within the quantizer
# Ok(())
# }
```

To emit a complete decodable stream, drive `TheoraEncoder` through the
`oxideav_core::Encoder` trait. It serializes the three §6 headers up
front (returned as the first `receive_packet` outputs, flagged
`header`), then encodes each top-down `VideoFrame` — supplied at the
coded macro-block-aligned dimensions — into one intra data packet:

```rust
use oxideav_core::{CodecId, Encoder, Frame};
use oxideav_theora::{TheoraEncoder, THEORA_CODEC_ID};
# fn run(ident: oxideav_theora::TheoraIdentHeader,
#        setup: oxideav_theora::SetupHeaderTables,
#        frame: Frame) -> oxideav_core::Result<()> {
let mut enc = TheoraEncoder::new(
    CodecId::new(THEORA_CODEC_ID), ident, setup, /* qi */ 32,
)
.map_err(|e| oxideav_core::Error::invalid(e.to_string()))?;
enc.send_frame(&frame)?;
while let Ok(pkt) = enc.receive_packet() {
    // pkt.flags.header marks the three §6 header packets; the rest are
    // §7 intra video-data packets (each a keyframe).
    let _ = pkt;
}
# Ok(())
# }
```

## Clean-room sources

Only the published Theora I Specification (`docs/video/theora/`), the
staged §6.4.1 procedure body transcribed from the specification's own
source for the section the PDF omits, and the fixture corpus under
`docs/video/theora/fixtures/` are consulted. Reference binaries are used
only as opaque black-box validators.

## License

MIT. See [LICENSE](LICENSE).
