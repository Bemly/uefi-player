//! Conformance-harness front end: `rusty_h265 <in.bit|.hevc> <out.yuv>`.
//!
//! Decodes an Annex-B elementary stream and writes the cropped pictures in
//! output order as planar 4:2:0 (`u8` for 8-bit, `u16` LE otherwise), the
//! layout `tools/hevc/conform.py` hashes. Prints one `key=value` stats line
//! on stdout so the harness can read frame and error counts.

// Primary allocator for this binary: our rusty_alloc, the pure-Rust mimalloc
// remake, which is what `rff-cli` ships. Timing this harness under the system
// allocator instead would not be comparable to the product — and several of the
// decoder's wins are removed allocations, which is exactly the thing an
// allocator swap changes the price of.
#[cfg(feature = "bench-alloc")]
#[global_allocator]
static GLOBAL_ALLOC: rusty_alloc_api::RustyAlloc = rusty_alloc_api::RustyAlloc;

use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // The two positional arguments are whatever is left after the flags, at
    // whichever position they land.
    //
    // They used to be `args[1]` and `args[2]` outright. That works until a
    // caller puts a flag first -- and one does: `conform.py --decoder "exe
    // --isa sse41"` appends the input and output AFTER the decoder string, so
    // the flag occupies the positional slots, the decoder tries to open
    // `--isa` as a bitstream, and the suite reports 0/147. Which reads exactly
    // like a decoder that has broken, on a change that touched no decoding.
    let mut positional: Vec<&String> = Vec::new();
    let mut skip_next = false;
    for a in args.iter().skip(1) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if a == "--isa" {
            skip_next = true; // `--isa <level>` takes a value
            continue;
        }
        if a.starts_with("--") {
            continue;
        }
        positional.push(a);
    }
    if positional.len() < 2 {
        eprintln!("usage: rusty_h265 <in.bit> <out.yuv> [--headers-only] [--pipe] [--isa avx2|sse41|baseline]");
        std::process::exit(2);
    }
    let (in_path, out_path) = (positional[0].clone(), positional[1].clone());
    let data = match std::fs::read(&in_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("read {in_path}: {e}");
            std::process::exit(2);
        }
    };
    let headers_only = args.iter().any(|a| a == "--headers-only");
    // `--pipe` writes the raw planar YUV to STDOUT, so a consumer can watch
    // frames arrive as they are decoded. The stats line then goes to stderr --
    // interleaved with the frame bytes it would corrupt the stream.
    let pipe = args.iter().any(|a| a == "--pipe");
    // `--isa sse41` caps the kernels for THIS process. The env var cannot do
    // the job a paired A/B needs: both arms share an environment, so setting it
    // there measures one configuration against itself.
    if let Some(i) = args.iter().position(|a| a == "--isa") {
        match args.get(i + 1).map(String::as_str) {
            Some("sse41") => rusty_h265::accel::force_isa(rusty_h265::accel::Isa::Sse41),
            Some("baseline") | Some("sse2") => rusty_h265::accel::force_isa(rusty_h265::accel::Isa::Baseline),
            Some("avx2") | None => {}
            Some(other) => {
                eprintln!("--isa: expected avx2, sse41 or baseline, got {other:?}");
                std::process::exit(2);
            }
        }
    }
    let verify_sei = args.iter().any(|a| a == "--verify-sei");
    // `-` as the output path writes nothing: the decode-only arm, so a
    // measurement is not dominated by the YUV write (codec-measurement §4).
    // `-` discards, so nothing needs serialising; `--pipe` and a real path do.
    let serialize = pipe || out_path != "-";
    let mut out: Box<dyn Write> = if pipe {
        Box::new(std::io::BufWriter::with_capacity(1 << 20, std::io::stdout()))
    } else if out_path == "-" {
        Box::new(std::io::sink())
    } else {
        Box::new(std::io::BufWriter::new(std::fs::File::create(&out_path).expect("create output")))
    };
    // Stage profiling is opt-in twice over: the `prof` feature must be built
    // in, and the variable must be set. Neither the shipping binary nor an
    // ordinary `--features prof` run pays anything until this line fires.
    #[cfg(feature = "prof")]
    let profiling = std::env::var_os("RH265_PROF").is_some();
    #[cfg(feature = "prof")]
    if profiling {
        rusty_h265::prof::enable();
    }
    let t0 = std::time::Instant::now();
    let mut dec = rusty_h265::Decoder::new();
    dec.headers_only = headers_only;
    dec.verify_sei = verify_sei;
    let mut first_err: Option<String> = None;
    for nal in rusty_h265::nal::split_annex_b(&data) {
        if let Err(e) = dec.push_nal(nal, None) {
            dec.stats.errors += 1;
            first_err.get_or_insert_with(|| e.to_string());
        }
        drain(&mut dec, &mut out, serialize);
    }
    dec.flush();
    let (w, h, bd) = drain(&mut dec, &mut out, serialize);
    out.flush().expect("flush output");
    let ms = t0.elapsed().as_millis();
    let s = dec.stats;
    let first_sei = dec.sei_results.first().map_or("none", |r| if r.1 { "ok" } else { "bad" });
    let line = format!(
        "frames={} errors={} decode_ms={} width={} height={} bit_depth={} pictures={} slices={} skipped_rasl={} generated_refs={} sei_checked={} sei_mismatch={} first_sei={} alloc={} isa={}",
        FRAMES.with(|f| f.get()),
        s.errors,
        ms,
        w,
        h,
        bd,
        s.pictures,
        s.slices,
        s.skipped_rasl,
        s.generated_refs,
        s.sei_checked,
        s.sei_mismatch,
        first_sei,
        // The measurement harness refuses to time a binary that is not the
        // one that ships: CLAUDE.md requires every performance number to come
        // from a rusty_alloc build, and this whole campaign was measured under
        // the system allocator before anyone checked. A comment in the harness
        // did not prevent that; a field it can assert on does.
        if cfg!(feature = "bench-alloc") { "rusty" } else { "system" },
        rusty_h265::accel::describe().rsplit(": ").next().unwrap_or("?"),
    );
    // Under `--pipe` stdout carries the frame bytes, so the stats go to stderr.
    if pipe {
        eprintln!("{line}");
    } else {
        println!("{line}");
    }
    #[cfg(feature = "prof")]
    if profiling {
        eprint!("{}", rusty_h265::prof::report(t0.elapsed().as_nanos() as u64));
    }
    if std::env::var_os("RH265_SAOBYTES").is_some() {
        use std::sync::atomic::Ordering;
        let (cp, pl, sp) = (
            rusty_h265::filters::SAO_COPIED.load(Ordering::Relaxed),
            rusty_h265::filters::SAO_PLANE.load(Ordering::Relaxed),
            rusty_h265::filters::SAO_SPANS.load(Ordering::Relaxed),
        );
        eprintln!("SAO copy: {cp} of {pl} samples ({:.1}%) in {sp} spans, {:.0} samples/span", 100.0 * cp as f64 / pl as f64, cp as f64 / sp.max(1) as f64);
    }
    if std::env::var_os("RH265_POOL").is_some() {
        use std::sync::atomic::Ordering;
        let (h, m) = (rusty_h265::decoder::POOL_HIT.load(Ordering::Relaxed), rusty_h265::decoder::POOL_MISS.load(Ordering::Relaxed));
        let (ok, sh, fu) = (
            rusty_h265::decoder::RECL_OK.load(Ordering::Relaxed),
            rusty_h265::decoder::RECL_SHARED.load(Ordering::Relaxed),
            rusty_h265::decoder::RECL_FULL.load(Ordering::Relaxed),
        );
        eprintln!("picture pool: {h} reused, {m} fresh ({:.0}% hit); reclaim: {ok} ok, {sh} still shared, {fu} pool full", 100.0 * h as f64 / (h + m).max(1) as f64);
    }
    if let Some(e) = first_err {
        eprintln!("first error: {e}");
    }
    // REACHABILITY (codec-vectorize-kernel): a kernel with a test and a
    // benchmark but a zero here is not deployed, whatever the call graph says.
    if std::env::var_os("RH265_CENSUS").is_some() {
        eprintln!("{}", rusty_h265::accel::describe());
        // The per-bin, per-block and per-edge counters are gated on
        // `census::ALWAYS`, a `cfg!(feature = "census")` CONST, because their
        // runtime check -- a `OnceLock` read -- is itself the cost being
        // measured on those paths. Without the feature they compile away and
        // report ZERO, which reads exactly like "this path never runs": the
        // false-refutation shape that a byte census exists to prevent. Say so
        // loudly rather than let a zero be believed.
        if !rusty_h265::accel::census::ALWAYS {
            eprintln!(
                "census WARNING: built without `--features census`. Counters on per-bin,                  per-block and per-edge paths are compile-time gated and will read 0 here.                  Rebuild with `--features census` before believing any zero below."
            );
        }
        for (name, v) in rusty_h265::accel::census::snapshot() {
            eprintln!("census {name} = {v}");
        }
    }

    // A decode that produced nothing must not look like success.
    //
    // This exited 0 after `frames=0 errors=60` on a Range-Extensions stream it
    // legitimately cannot decode -- and a benchmark harness timed that, saw
    // 6 ms against ffmpeg's 734 ms, and reported us **46x faster**. The number
    // was not wrong about the clock; it was wrong about what had happened, and
    // nothing in the exit status said so. Scripts read exit codes.
    if FRAMES.with(|f| f.get()) == 0 || s.errors > 0 {
        std::process::exit(1);
    }
}

thread_local! {
    static FRAMES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Drain every picture the decoder has ready.
///
/// `serialize` is false for the discard output (`-`). Draining is real decoder
/// work -- it runs the DPB's bumping process and releases pictures -- but
/// SERIALISING each one into a `Vec` that is then written to `io::sink()` is
/// not: it was 1.38 MB of memcpy per frame, 830 MB over a 600-frame clip, on
/// the path every published timing measures. `ffmpeg -f null -` does not do it
/// either, so leaving it in was measuring us doing strictly more work than the
/// arm we compare against (codec-measurement §4).
fn drain(dec: &mut rusty_h265::Decoder, out: &mut impl Write, serialize: bool) -> (usize, usize, u8) {
    let mut geom = (0, 0, 0);
    let mut buf = Vec::new();
    while let Ok(frame) = dec.next_frame() {
        if serialize {
            buf.clear();
            frame.write_yuv(&mut buf);
            out.write_all(&buf).expect("write output");
        }
        geom = (frame.width, frame.height, frame.bit_depth());
        FRAMES.with(|f| f.set(f.get() + 1));
    }
    geom
}
