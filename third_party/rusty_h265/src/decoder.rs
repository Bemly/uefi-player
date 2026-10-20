//! The decoder state machine: NAL dispatch, parameter-set activation, picture
//! boundaries, POC (§8.3.1), reference picture set marking (§8.3.2), missing
//! reference generation (§8.3.3), reference list construction (§8.3.4) and
//! the DPB output/bumping process (§C.5.2).

// VENDOR(uefi-player): core/alloc paths work in both std and no_std builds.
use alloc::collections::VecDeque;
use alloc::sync::Arc;

use crate::bits::BitReader;
use crate::ctu::{Ablate, CtxStore, ScalingFactors, SliceDecoder, SliceInputs};
use crate::error::{Error, Result};
use crate::frame::{Frame, Picture};
use crate::nal::{split_annex_b, unescape, NalHeader, NalType, Rbsp};
use crate::pic::PicState;
use crate::ps::{parse_pps, parse_sps, parse_vps, Pps, Sps, TileLayout, Vps, PROFILE_MAIN, PROFILE_MAIN10, PROFILE_MAIN_STILL};
use crate::sei::{self, PictureHash};
use crate::slice::{parse_slice_header, SliceHeader};
// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::{format, vec, vec::Vec};

/// Reference marking of a DPB picture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefMark {
    Unused,
    ShortTerm,
    LongTerm,
}

/// One decoded picture buffer slot.
#[derive(Debug, Clone)]
pub struct DpbEntry {
    pub pic: Arc<Picture>,
    pub poc: i32,
    pub mark: RefMark,
    pub needed_for_output: bool,
    pub pic_latency: u32,
    pub pts: Option<i64>,
    /// `pic_output_flag` after the RASL rule (§8.1.3): false for generated pictures.
    pub output_flag: bool,
}

/// A reference picture list entry.
#[derive(Debug, Clone)]
pub struct RefPic {
    pub pic: Arc<Picture>,
    pub poc: i32,
    pub is_long_term: bool,
}

/// The reference picture set of the current picture, resolved against the DPB.
#[derive(Debug, Clone, Default)]
pub struct RefLists {
    pub l0: Vec<RefPic>,
    pub l1: Vec<RefPic>,
}

/// The picture being decoded.
pub struct CurrentPicture {
    pub pic: Picture,
    pub poc: i32,
    pub sps: Arc<Sps>,
    pub pps: Arc<Pps>,
    pub tiles: Arc<TileLayout>,
    pub nal_type: NalType,
    pub temporal_id: u8,
    pub output_flag: bool,
    pub pts: Option<i64>,
    /// Last independent slice header (for dependent slice segments).
    pub last_indep_header: Option<SliceHeader>,
    /// Per-slice reference lists in decoding order (index = slice number).
    pub ref_lists: Vec<RefLists>,
    /// Number of slice segments decoded so far.
    pub slice_segments: u32,
    /// `StCurrBefore ∪ StCurrAfter ∪ LtCurr` POCs, for sanity.
    pub num_refs: usize,
    /// Set when a slice of this picture failed to decode.
    pub errored: bool,
    /// Per-4x4 syntax/reconstruction state.
    pub state: PicState,
    /// WPP / dependent-slice context storage.
    pub store: CtxStore,
    /// Slice headers in decoding order.
    pub headers: Vec<SliceHeader>,
    /// `SliceAddrRs` of the current slice (dependent segments inherit it).
    pub slice_addr_rs: i32,
    /// Decoded picture hash SEI attached to this picture.
    pub expected_hash: Option<PictureHash>,
    /// Scaling factors when scaling lists are enabled.
    pub scaling: Option<ScalingFactors>,
}

/// Decoder statistics (also printed by the harness binary).
#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub pictures: u64,
    pub slices: u64,
    pub errors: u64,
    pub skipped_rasl: u64,
    pub generated_refs: u64,
    /// Pictures whose decoded-picture-hash SEI was checked / did not match.
    pub sei_checked: u64,
    pub sei_mismatch: u64,
}

/// A pure-Rust HEVC decoder. Push NAL units (or Annex-B chunks) in, pull
/// [`Frame`]s out in output order.
/// Diagnostic: how often the picture pool actually supplies a buffer.
pub static POOL_HIT: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static POOL_MISS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
/// Why a buffer did or did not come back: reclaimed, still shared with the
/// output queue, or dropped because the pool was already full.
pub static RECL_OK: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static RECL_SHARED: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
pub static RECL_FULL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

/// Scratch the in-loop filters reuse across every picture.
#[derive(Default)]
pub struct FilterScratch {
    /// Deblocked samples SAO reads while it writes filtered ones.
    pub sao: Vec<u16>,
    /// Boundary strengths, per 4x4, one map per direction.
    pub bs_v: Vec<u8>,
    pub bs_h: Vec<u8>,
}

pub struct Decoder {
    vps: Vec<Option<Arc<Vps>>>,
    sps: Vec<Option<Arc<Sps>>>,
    pps: Vec<Option<Arc<Pps>>>,
    active_sps_id: Option<u8>,
    dpb: Vec<DpbEntry>,
    output: VecDeque<Frame>,
    cur: Option<CurrentPicture>,
    /// POC of the previous TemporalId-0 picture that is not RASL/RADL/SLNR.
    prev_tid0_poc: i32,
    /// True before the first picture and after an end-of-sequence NAL.
    first_in_sequence: bool,
    /// `NoRaslOutputFlag` of the most recent IRAP picture.
    no_rasl_output_flag: bool,
    /// Set while the slices of a skipped RASL picture stream by.
    skipping_picture: bool,
    pending_pts: Option<i64>,
    /// A picture hash from a prefix SEI, for the next picture.
    pending_hash: Option<PictureHash>,
    /// The sequence-invariant tables (`MinTbAddrZs`, the tile map), kept across
    /// pictures. Rebuilt only when the SPS or the tile layout actually changes.
    seq_tables: Option<crate::pic::SeqTables>,
    /// The in-loop filters' working buffers, kept for the life of the decoder.
    ///
    /// These lived on `CurrentPicture`, so every picture allocated them from
    /// nothing: the SAO scratch alone grows to a whole luma plane, 1.84 MB at
    /// 720p, and had to be sized and zeroed again for each of the 600 pictures
    /// in a clip. That also silently cancelled the narrowed SAO copy above it --
    /// there is no point copying 32% of a plane into a buffer that was just
    /// zeroed in full.
    filter_scratch: FilterScratch,
    /// The previous picture's per-4x4 maps, for the next one to reuse.
    ///
    /// Unlike a `Picture` this is never shared -- it belongs to the picture
    /// being decoded and dies with it -- so one slot recycles every time,
    /// where the picture pool can only catch the buffers the output queue has
    /// already released.
    state_pool: Option<PicState>,
    /// Pictures the DPB has let go of but somebody else may still hold.
    /// Swept just before the next picture is allocated -- see `reclaim`.
    pic_parked: Vec<Arc<Picture>>,
    /// Sample-plane buffers reclaimed from the DPB, for the next picture.
    ///
    /// A picture is 2.76 MB at 720p and one is built per picture: 1.66 GB of
    /// allocate-and-zero over a 600-picture clip, measured at 3.2% of decode.
    /// Reference pictures are `Arc`-shared with the output queue, so a buffer
    /// comes back only when the last handle to it goes -- which `Arc::into_inner`
    /// reports exactly. Bounded, because a decoder that leaks buffers into a
    /// pool has just moved the allocation into a place nobody is watching.
    pic_pool: Vec<Picture>,
    /// Verify every picture against its decoded-picture-hash SEI.
    pub verify_sei: bool,
    /// Stage ablation (ceiling probes); off unless the environment asks.
    pub ablate: Ablate,
    /// Per finished picture: (POC, hash matched) when `verify_sei` and a hash was present.
    pub sei_results: Vec<(i32, bool)>,
    pub stats: Stats,
    /// Bring-up: decode nothing past the slice header (Phase 1).
    pub headers_only: bool,
    /// `RH265_TRACE=1`: one line per picture boundary on stderr.
    trace: bool,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            vps: vec![None; 16],
            sps: vec![None; 16],
            pps: vec![None; 64],
            active_sps_id: None,
            dpb: Vec::new(),
            output: VecDeque::new(),
            cur: None,
            prev_tid0_poc: 0,
            first_in_sequence: true,
            no_rasl_output_flag: true,
            skipping_picture: false,
            pending_pts: None,
            pending_hash: None,
            seq_tables: None,
            filter_scratch: FilterScratch::default(),
            state_pool: None,
            pic_parked: Vec::new(),
            pic_pool: Vec::new(),
            verify_sei: false,
            ablate: Ablate::from_env(),
            sei_results: Vec::new(),
            stats: Stats::default(),
            headers_only: false,
            trace: crate::knob("RH265_TRACE"),
        }
    }

    /// Feeds an Annex-B chunk (any number of NAL units, e.g. one access unit).
    pub fn push_annexb(&mut self, data: &[u8], pts: Option<i64>) -> Result<()> {
        let mut first_err = None;
        let mut pts = pts;
        for nal in split_annex_b(data) {
            if let Err(e) = self.push_nal(nal, pts.take()) {
                self.stats.errors += 1;
                first_err.get_or_insert(e);
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Feeds one NAL unit (header + escaped payload, no start code).
    pub fn push_nal(&mut self, nal: &[u8], pts: Option<i64>) -> Result<()> {
        let hdr = NalHeader::parse(nal).ok_or_else(|| Error::invalid("bad NAL header"))?;
        if pts.is_some() {
            self.pending_pts = pts;
        }
        if hdr.layer_id != 0 {
            // Enhancement layers (SHVC/MV-HEVC) are ignored: the base layer decodes alone.
            return Ok(());
        }
        let payload = &nal[2..];
        match hdr.nal_type {
            NalType::Vps => {
                let rbsp = unescape(payload);
                let v = parse_vps(&rbsp.data)?;
                let id = v.id as usize;
                self.vps[id] = Some(Arc::new(v));
            }
            NalType::Sps => {
                let rbsp = unescape(payload);
                let s = parse_sps(&rbsp.data)?;
                let id = s.id as usize;
                self.sps[id] = Some(Arc::new(s));
            }
            NalType::Pps => {
                let rbsp = unescape(payload);
                let p = parse_pps(&rbsp.data)?;
                let id = p.id as usize;
                self.pps[id] = Some(Arc::new(p));
            }
            NalType::Eos | NalType::Eob => {
                // §C.5.2 gives end-of-sequence no output semantics: the DPB
                // keeps its pictures until the next IRAP decides their fate
                // (a CRA after EOS infers NoOutputOfPriorPicsFlag = 1 and
                // discards them — NoOutPrior_A). Only the NoRaslOutputFlag
                // of the next IRAP is affected.
                if self.trace {
                    eprintln!("EOS");
                }
                self.finish_picture();
                self.first_in_sequence = true;
            }
            NalType::PrefixSei | NalType::SuffixSei => {
                if self.verify_sei {
                    let rbsp = unescape(payload);
                    let cf = self.cur.as_ref().map_or(1, |c| c.sps.chroma_format_idc);
                    if let Some(h) = sei::parse_picture_hash(&rbsp.data, cf) {
                        match (&mut self.cur, hdr.nal_type) {
                            (Some(c), NalType::SuffixSei) => c.expected_hash = Some(h),
                            _ => self.pending_hash = Some(h),
                        }
                    }
                }
            }
            NalType::Aud | NalType::Fd => {}
            t if t.is_vcl() => {
                if t.id() >= 22 {
                    // reserved VCL types: ignore
                    return Ok(());
                }
                let rbsp = unescape(payload);
                self.push_slice(&hdr, rbsp)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Ends the stream: finishes the picture in flight and outputs everything.
    pub fn flush(&mut self) {
        self.finish_picture();
        self.flush_dpb_output();
        self.first_in_sequence = true;
    }

    /// Next frame in output order, or [`Error::Again`].
    pub fn next_frame(&mut self) -> Result<Frame> {
        self.output.pop_front().ok_or(Error::Again)
    }

    fn lookup(&self, pps_id: u8) -> Option<(&Sps, &Pps)> {
        let pps = self.pps.get(pps_id as usize)?.as_ref()?;
        let sps = self.sps.get(pps.sps_id as usize)?.as_ref()?;
        Some((sps, pps))
    }

    fn push_slice(&mut self, hdr: &NalHeader, rbsp: Rbsp) -> Result<()> {
        let mut r = BitReader::new(&rbsp.data);
        // Peek first_slice_segment_in_pic_flag: a new picture ends the old one.
        let first = r.peek_bits(1) == 1;
        if first {
            self.finish_picture();
            self.skipping_picture = false;
        } else if self.skipping_picture {
            return Ok(());
        }
        let lookup = |id: u8| self.lookup(id);
        let prev = self.cur.as_ref().and_then(|c| c.last_indep_header.as_ref());
        let sh = parse_slice_header(&mut r, hdr, &lookup, prev)?;
        if first {
            self.start_picture(hdr, &sh)?;
            if self.skipping_picture {
                return Ok(());
            }
        } else if self.cur.is_none() {
            return Err(Error::invalid("slice without a first slice segment"));
        }
        let cur = self.cur.as_mut().ok_or_else(|| Error::invalid("no current picture"))?;
        if sh.pps_id != cur.pps.id {
            return Err(Error::invalid("slice PPS differs within a picture"));
        }
        // Reference lists for this slice (§8.3.4).
        let lists = if sh.slice_type.is_intra() { RefLists::default() } else { build_ref_lists(&self.dpb, &sh, cur)? };
        cur.ref_lists.push(lists);
        if !sh.dependent_slice_segment {
            cur.last_indep_header = Some(sh.clone());
            cur.slice_addr_rs = sh.segment_address as i32;
        }
        cur.slice_segments += 1;
        self.stats.slices += 1;
        if self.trace {
            eprintln!(
                "  slice addr={} dep={} type={:?} qp={} sao={}{} dbk_off={} beta={} tc={} lf_slices={} entry={} tiles={} wpp={} dqp={} tqb={}",
                sh.segment_address,
                sh.dependent_slice_segment as u8,
                sh.slice_type,
                sh.slice_qp,
                sh.sao_luma as u8,
                sh.sao_chroma as u8,
                sh.deblocking_filter_disabled as u8,
                sh.beta_offset_div2,
                sh.tc_offset_div2,
                sh.loop_filter_across_slices_enabled as u8,
                sh.entry_point_offsets.len(),
                cur.pps.tiles_enabled as u8,
                cur.pps.entropy_coding_sync_enabled as u8,
                cur.pps.cu_qp_delta_enabled as u8,
                cur.pps.transquant_bypass_enabled as u8
            );
        }
        if self.headers_only {
            return Ok(());
        }
        let slice_idx = cur.headers.len() as u16;
        cur.headers.push(sh.clone());
        let refs = cur.ref_lists.last().cloned().unwrap_or_default();
        let poc = cur.poc;
        let r = SliceDecoder::new(SliceInputs {
            sps: &cur.sps,
            pps: &cur.pps,
            sh: &sh,
            tiles: &cur.tiles,
            pic: &mut cur.pic,
            st: &mut cur.state,
            store: &mut cur.store,
            rbsp: &rbsp,
            scaling: cur.scaling.as_ref(),
            refs: &refs,
            poc,
            ablate: self.ablate,
            slice_addr_rs: cur.slice_addr_rs,
            slice_idx,
        })
        .and_then(|d| d.decode());
        if let Err(e) = r {
            cur.errored = true;
            return Err(e);
        }
        Ok(())
    }

    fn start_picture(&mut self, hdr: &NalHeader, sh: &SliceHeader) -> Result<()> {
        let pps = self.pps[sh.pps_id as usize].clone().ok_or_else(|| Error::invalid("PPS"))?;
        let sps = self.sps[pps.sps_id as usize].clone().ok_or_else(|| Error::invalid("SPS"))?;
        let nt = hdr.nal_type;

        // NoRaslOutputFlag (§8.1.3).
        if nt.is_irap() {
            self.no_rasl_output_flag = nt.is_idr() || nt.is_bla() || self.first_in_sequence;
        }
        if nt.is_rasl() && self.no_rasl_output_flag {
            // Associated IRAP started the sequence: RASL pictures are not decodable.
            self.skipping_picture = true;
            self.stats.skipped_rasl += 1;
            return Ok(());
        }
        if !nt.is_irap() && self.first_in_sequence {
            // Stream does not start with an IRAP: skip until one arrives.
            self.skipping_picture = true;
            return Ok(());
        }

        // Activation + scope check.
        check_scope(&sps, &pps)?;
        let sps_changed = self.active_sps_id != Some(sps.id) || self.dpb.iter().any(|e| e.pic.width() != sps.width as usize || e.pic.height() != sps.height as usize);
        if nt.is_irap() && self.no_rasl_output_flag {
            self.active_sps_id = Some(sps.id);
        } else if self.active_sps_id != Some(sps.id) {
            return Err(Error::invalid("SPS changed outside an IRAP"));
        }
        let tiles = Arc::new(TileLayout::new(&sps, &pps)?);

        // POC (§8.3.1).
        let max_poc_lsb = 1i32 << sps.log2_max_poc_lsb;
        let poc_msb = if nt.is_irap() && self.no_rasl_output_flag {
            0
        } else {
            let prev_lsb = self.prev_tid0_poc & (max_poc_lsb - 1);
            let prev_msb = self.prev_tid0_poc - prev_lsb;
            let lsb = sh.poc_lsb as i32;
            if lsb < prev_lsb && prev_lsb - lsb >= max_poc_lsb / 2 {
                prev_msb + max_poc_lsb
            } else if lsb > prev_lsb && lsb - prev_lsb > max_poc_lsb / 2 {
                prev_msb - max_poc_lsb
            } else {
                prev_msb
            }
        };
        let poc = poc_msb + sh.poc_lsb as i32;
        if hdr.temporal_id == 0 && !nt.is_rasl() && !nt.is_radl() && !nt.is_sub_layer_non_ref() {
            self.prev_tid0_poc = poc;
        }

        // RPS marking (§8.3.2).
        let (st_before, st_after, st_foll, lt_curr, lt_foll) = derive_rps(&sps, sh, poc);
        if nt.is_idr() {
            for e in self.dpb.iter_mut() {
                e.mark = RefMark::Unused;
            }
        } else {
            let mut keep = vec![RefMark::Unused; self.dpb.len()];
            for (p, msb_present) in lt_curr.iter().chain(lt_foll.iter()) {
                if let Some(i) = find_ref(&self.dpb, *p, !*msb_present, max_poc_lsb) {
                    keep[i] = RefMark::LongTerm;
                }
            }
            for p in st_before.iter().chain(st_after.iter()).chain(st_foll.iter()) {
                if let Some(i) = self.dpb.iter().position(|e| e.mark == RefMark::ShortTerm && e.poc == *p) {
                    if keep[i] == RefMark::Unused {
                        keep[i] = RefMark::ShortTerm;
                    }
                }
            }
            for (e, k) in self.dpb.iter_mut().zip(keep) {
                e.mark = k;
            }
            // Missing references (§8.3.3): generate unavailable pictures.
            let mut missing: Vec<(i32, bool)> = Vec::new();
            for p in st_before.iter().chain(st_after.iter()) {
                if !self.dpb.iter().any(|e| e.mark == RefMark::ShortTerm && e.poc == *p) {
                    missing.push((*p, false));
                }
            }
            for (p, msb_present) in &lt_curr {
                if find_ref(&self.dpb, *p, !*msb_present, max_poc_lsb).is_none() {
                    missing.push((*p, true));
                }
            }
            for (p, lt) in missing {
                let pic = generate_picture(&sps, p);
                self.dpb.push(DpbEntry {
                    pic: Arc::new(pic),
                    poc: p,
                    mark: if lt { RefMark::LongTerm } else { RefMark::ShortTerm },
                    needed_for_output: false,
                    pic_latency: 0,
                    pts: None,
                    output_flag: false,
                });
                self.stats.generated_refs += 1;
            }
        }

        // C.5.2.2: output and removal before the current picture.
        let highest_tid = sps.max_sub_layers_minus1 as usize;
        let ord = sps.ordering[highest_tid];
        if nt.is_irap() && self.no_rasl_output_flag && self.stats.pictures > 0 {
            // §C.5.2.2: a CRA infers 1; when the SPS geometry changed the
            // decoder *may* set 1 but "should not" — and the conformance
            // md5s are the should-not reading, so we always output them.
            let _ = sps_changed;
            let no_output_of_prior_pics = nt.is_cra() || sh.no_output_of_prior_pics;
            if no_output_of_prior_pics {
                for e in self.dpb.drain(..).collect::<Vec<_>>() {
                    self.reclaim(e);
                }
            } else {
                self.flush_dpb_output();
            }
        } else {
            self.purge_dpb();
            loop {
                let n_out = self.dpb.iter().filter(|e| e.needed_for_output).count();
                let latency_hit = ord.max_latency_increase_plus1 != 0
                    && self
                        .dpb
                        .iter()
                        .any(|e| e.needed_for_output && e.pic_latency >= ord.max_num_reorder_pics + ord.max_latency_increase_plus1 - 1);
                let full = self.dpb.len() > ord.max_dec_pic_buffering_minus1 as usize;
                if n_out > ord.max_num_reorder_pics as usize || latency_hit || (full && n_out > 0) {
                    self.bump();
                } else {
                    break;
                }
            }
            // A full DPB with nothing left to output: drop unreferenced pictures.
            if self.dpb.len() > ord.max_dec_pic_buffering_minus1 as usize {
                self.purge_dpb();
            }
        }

        let output_flag = if nt.is_rasl() && self.no_rasl_output_flag { false } else { sh.pic_output_flag };
        if self.trace {
            eprintln!(
                "pic {:?} tid={} poc={} lsb={} out={} norasl={} noprior={} dpb={} refs={}",
                nt,
                hdr.temporal_id,
                poc,
                sh.poc_lsb,
                output_flag,
                self.no_rasl_output_flag,
                sh.no_output_of_prior_pics,
                self.dpb.len(),
                st_before.len() + st_after.len() + lt_curr.len()
            );
        }
        // Per-picture setup: the frame buffers and every per-4x4 map. Timed
        // because the first profile left ~22% of decode unaccounted for and
        // this is the largest thing outside the CTU loop.
        crate::prof_scope!(crate::prof::Stage::Dpb);
        self.sweep_parked();
        let mut pic = {
            crate::prof_scope!(crate::prof::Stage::PicAlloc);
            let (w, h) = (sps.width as usize, sps.height as usize);
            // A pattern guard cannot take `p` by mutable reference, so the
            // reuse attempt happens before the match.
            let recycled = self.pic_pool.pop().filter(|_| true).and_then(|mut p| {
                if p.reuse(w, h, sps.chroma_format_idc, sps.bit_depth_luma, sps.bit_depth_chroma) {
                    Some(p)
                } else {
                    // The geometry changed mid-stream and this buffer is the
                    // wrong shape: drop it rather than keep it, so a resolution
                    // switch does not leave stale sizes in the pool forever.
                    None
                }
            });
            if crate::knob("RH265_POOL") {
                use core::sync::atomic::Ordering;
                if recycled.is_some() { POOL_HIT.fetch_add(1, Ordering::Relaxed) } else { POOL_MISS.fetch_add(1, Ordering::Relaxed) };
            }
            match recycled {
                Some(p) => p,
                None => Picture::new(w, h, sps.chroma_format_idc, sps.bit_depth_luma, sps.bit_depth_chroma),
            }
        };
        let (ow, oh) = sps.output_size();
        pic.crop = (
            (sps.sub_width_c as u32 * sps.conf_win[0]) as usize,
            (sps.sub_height_c as u32 * sps.conf_win[2]) as usize,
            ow as usize,
            oh as usize,
        );
        pic.poc = poc;
        // Sequence-invariant tables: reuse unless the SPS or tiles changed.
        if !self.seq_tables.as_ref().is_some_and(|t| t.matches(&sps, &tiles)) {
            self.seq_tables = Some(crate::pic::SeqTables::build(&sps, &tiles));
        } else if crate::knob("RH265_SEQCHECK") {
            // Diagnostic: rebuild anyway and report any field the key missed.
            let fresh = crate::pic::SeqTables::build(&sps, &tiles);
            let t = self.seq_tables.as_ref().unwrap();
            if *fresh.zs != *t.zs {
                eprintln!("SEQCHECK: zs differs on a cache HIT");
            }
            if *fresh.tile_id != *t.tile_id {
                eprintln!("SEQCHECK: tile_id differs on a cache HIT");
            }
        }
        let tables = self.seq_tables.as_ref().expect("just built");
        // As in the picture pool: a pattern guard cannot borrow mutably, so
        // the reuse attempt happens first.
        let state = match self.state_pool.take() {
            Some(mut st) => {
                if st.reuse(&sps, tables) {
                    st
                } else {
                    PicState::with_tables(&sps, tables)
                }
            }
            None => PicState::with_tables(&sps, tables),
        };
        let scaling = if sps.scaling_list_enabled {
            pps.scaling_list.as_ref().or(sps.scaling_list.as_ref()).map(ScalingFactors::new)
        } else {
            None
        };
        self.cur = Some(CurrentPicture {
            pic,
            poc,
            sps,
            pps,
            tiles,
            nal_type: nt,
            temporal_id: hdr.temporal_id,
            output_flag,
            pts: self.pending_pts.take(),
            last_indep_header: None,
            ref_lists: Vec::new(),
            slice_segments: 0,
            num_refs: st_before.len() + st_after.len() + lt_curr.len(),
            errored: false,
            state,
            store: CtxStore::default(),
            headers: Vec::new(),
            slice_addr_rs: sh.segment_address as i32,
            expected_hash: self.pending_hash.take(),
            scaling,
        });
        self.first_in_sequence = false;
        Ok(())
    }

    /// C.5.2.3: the current picture is done — store, mark, bump.
    /// Take a dropped entry's buffer back for reuse, if this was the last
    /// handle to it. `Arc::into_inner` returns `None` while the output queue or
    /// another reference still holds the picture, which is exactly the test.
    /// Queue a dropped entry's picture for reuse.
    ///
    /// Taking the buffer here and now recovers **41%** of them: at the moment
    /// the DPB drops a reference the output queue usually still holds one, so
    /// `Arc::into_inner` returns `None` and the buffer is lost -- measured 351
    /// of 600 lost that way, and none to a full pool. The handle is therefore
    /// PARKED instead, and retried once the caller has taken and dropped the
    /// frame. Nothing here waits on the caller: a handle that is still shared
    /// on the next sweep simply stays parked.
    fn reclaim(&mut self, e: DpbEntry) {
        const PARK: usize = 16;
        if self.pic_parked.len() < PARK {
            self.pic_parked.push(e.pic);
        }
    }

    /// Drop every DPB entry that is neither referenced nor awaiting output,
    /// reclaiming its picture buffer.
    ///
    /// In place. This was `dpb.drain(..).partition(..)`, which allocated TWO
    /// fresh `Vec`s and memmoved every entry into one of them -- at both call
    /// sites, every picture -- to remove on average less than one entry.
    /// `memcpy_census.py` attributed six runtime-length `memmove` calls to it.
    ///
    /// `retain` cannot be used: its closure would have to call `reclaim`, which
    /// needs `&mut self` while `retain` holds the vector. So this compacts by
    /// hand -- kept entries keep their ORDER, which output order depends on --
    /// and then pops the discarded tail, whose order does not matter.
    fn purge_dpb(&mut self) {
        let mut w = 0;
        for i in 0..self.dpb.len() {
            if self.dpb[i].needed_for_output || self.dpb[i].mark != RefMark::Unused {
                self.dpb.swap(i, w);
                w += 1;
            }
        }
        while self.dpb.len() > w {
            let gone = self.dpb.pop().expect("len > w");
            self.reclaim(gone);
        }
    }

    /// Take back every parked buffer nobody else is holding any more.
    fn sweep_parked(&mut self) {
        const KEEP: usize = 6;
        let diag = crate::knob("RH265_POOL");
        let mut i = 0;
        while i < self.pic_parked.len() {
            if self.pic_pool.len() >= KEEP {
                if diag {
                    RECL_FULL.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                }
                break;
            }
            // `try_unwrap`, not `into_inner`: both consume the handle, but
            // `try_unwrap` hands it back in the `Err` arm, which is what lets a
            // still-shared picture stay parked for the next sweep.
            let a = self.pic_parked.swap_remove(i);
            match alloc::sync::Arc::try_unwrap(a) {
                Ok(p) => {
                    if diag {
                        RECL_OK.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    }
                    self.pic_pool.push(p);
                }
                Err(a) => {
                    if diag {
                        RECL_SHARED.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
                    }
                    self.pic_parked.push(a);
                    i += 1;
                }
            }
        }
    }

    /// Zero every coding tree block that did not decode.
    ///
    /// `Picture::reuse` hands back a buffer still holding the previous picture,
    /// so a block no slice reconstructed would otherwise show unrelated samples.
    /// Runs before the in-loop filters, which read across block boundaries, so
    /// what they see is byte-for-byte what the old whole-plane clear gave them.
    /// On a complete picture -- every conformant stream -- this is a scan of
    /// `ctb_w * ctb_h` bools and not one store.
    fn clear_uncovered(cur: &mut CurrentPicture) {
        let st = &cur.state;
        if st.ctb_done.iter().all(|&d| d) {
            return;
        }
        let ctb = 1usize << st.log2_ctb;
        let (ctb_w, ctb_h) = (st.ctb_w, st.ctb_h);
        let todo: Vec<usize> = (0..ctb_w * ctb_h).filter(|&i| !st.ctb_done[i]).collect();
        // 4:2:0 halves both axes, 4:2:2 only the horizontal, 4:4:4 neither.
        let (sw, sh) = match cur.pic.chroma_format_idc {
            1 => (2, 2),
            2 => (2, 1),
            _ => (1, 1),
        };
        for rs in todo {
            let (rx, ry) = (rs % ctb_w, rs / ctb_w);
            for (c, plane) in cur.pic.planes.iter_mut().enumerate() {
                if plane.width == 0 {
                    continue;
                }
                let (bw, bh) = if c == 0 { (ctb, ctb) } else { (ctb / sw, ctb / sh) };
                let (x0, y0) = (rx * bw, ry * bh);
                if x0 >= plane.width || y0 >= plane.height {
                    continue;
                }
                let w = (x0 + bw).min(plane.width) - x0;
                for y in y0..(y0 + bh).min(plane.height) {
                    let a = y * plane.stride + x0;
                    plane.data[a..a + w].fill(0);
                }
            }
        }
    }

    fn finish_picture(&mut self) {
        let Some(mut cur) = self.cur.take() else { return };
        self.stats.pictures += 1;
        Self::clear_uncovered(&mut cur);
        if !self.headers_only {
            crate::filters::apply_in_loop_filters(&mut cur, &mut self.filter_scratch);
            // Compress the motion field to 16×16 for later TMVP (§8.5.3.2.9).
            let st = &cur.state;
            let w16 = st.width.div_ceil(16);
            let h16 = st.height.div_ceil(16);
            // Straight into the picture's own vector: a pooled picture still
            // has this capacity from last time, so the per-picture
            // `Vec::with_capacity` was an allocation the buffer already had.
            cur.pic.motion16.clear();
            cur.pic.motion16.reserve(w16 * h16);
            for y in 0..h16 {
                for x in 0..w16 {
                    cur.pic.motion16.push(st.motion[st.idx4(x * 16, y * 16)]);
                }
            }
            cur.pic.motion16_w = w16;
        }
        if self.verify_sei {
            if let Some(h) = &cur.expected_hash {
                self.stats.sei_checked += 1;
                let bad = sei::mismatched_planes(h, &cur.pic);
                self.sei_results.push((cur.poc, bad.is_empty()));
                if !bad.is_empty() {
                    self.stats.sei_mismatch += 1;
                    if self.trace {
                        eprintln!("  SEI hash mismatch poc={} planes={:?}", cur.poc, bad);
                    }
                }
            }
        }
        let ord = cur.sps.ordering[cur.sps.max_sub_layers_minus1 as usize];
        if cur.output_flag {
            for e in self.dpb.iter_mut() {
                if e.needed_for_output {
                    e.pic_latency += 1;
                }
            }
        }
        self.dpb.push(DpbEntry {
            pic: Arc::new(cur.pic),
            poc: cur.poc,
            mark: RefMark::ShortTerm,
            needed_for_output: cur.output_flag,
            pic_latency: 0,
            pts: cur.pts,
            output_flag: cur.output_flag,
        });
        loop {
            let n_out = self.dpb.iter().filter(|e| e.needed_for_output).count();
            let latency_hit = ord.max_latency_increase_plus1 != 0
                && self
                    .dpb
                    .iter()
                    .any(|e| e.needed_for_output && e.pic_latency >= ord.max_num_reorder_pics + ord.max_latency_increase_plus1 - 1);
            if n_out > ord.max_num_reorder_pics as usize || latency_hit {
                self.bump();
            } else {
                break;
            }
        }
        // The maps are not shared with anything; keep them for the next picture.
        self.state_pool = Some(cur.state);
    }

    /// C.5.2.4 bumping: output the smallest-POC picture waiting for output.
    fn bump(&mut self) {
        let Some(i) = self.dpb.iter().enumerate().filter(|(_, e)| e.needed_for_output).min_by_key(|(_, e)| e.poc).map(|(i, _)| i) else {
            return;
        };
        let e = &mut self.dpb[i];
        e.needed_for_output = false;
        if self.trace {
            eprintln!("  output poc={}", e.poc);
        }
        let (_, _, w, h) = e.pic.crop;
        self.output.push_back(Frame {
            picture: e.pic.clone(),
            poc: e.poc,
            pts: e.pts,
            width: w,
            height: h,
        });
        if e.mark == RefMark::Unused {
            let gone = self.dpb.remove(i);
            self.reclaim(gone);
        }
    }

    /// Outputs every waiting picture in POC order and empties the DPB.
    fn flush_dpb_output(&mut self) {
        while self.dpb.iter().any(|e| e.needed_for_output) {
            self.bump();
        }
        for e in self.dpb.drain(..).collect::<Vec<_>>() {
            self.reclaim(e);
        }
    }

    /// The DPB contents (for tests and the harness).
    pub fn dpb(&self) -> &[DpbEntry] {
        &self.dpb
    }
}

/// v1 scope: Main / Main 10 / Main Still Picture, 4:2:0 (or monochrome
/// syntax), ≤ 10 bit, no extensions.
fn check_scope(sps: &Sps, pps: &Pps) -> Result<()> {
    let prof = sps.ptl.profile();
    if !matches!(prof, 0 | PROFILE_MAIN | PROFILE_MAIN10 | PROFILE_MAIN_STILL) {
        return Err(Error::unsupported(format!("general_profile_idc {prof} (only Main / Main 10 / Main Still Picture)")));
    }
    if sps.chroma_format_idc != 1 {
        return Err(Error::unsupported(format!("chroma_format_idc {} (only 4:2:0)", sps.chroma_format_idc)));
    }
    if sps.bit_depth_luma > 10 || sps.bit_depth_chroma > 10 {
        return Err(Error::unsupported(format!("bit depth {}/{} (max 10)", sps.bit_depth_luma, sps.bit_depth_chroma)));
    }
    if sps.range_extension || sps.multilayer_extension || sps.ext_3d || sps.scc_extension {
        return Err(Error::unsupported("SPS extensions (RExt/SHVC/3D/SCC)"));
    }
    if pps.range_extension || pps.multilayer_extension || pps.ext_3d || pps.scc_extension {
        return Err(Error::unsupported("PPS extensions (RExt/SHVC/3D/SCC)"));
    }
    if pps.sps_id != sps.id {
        return Err(Error::invalid("PPS/SPS id mismatch"));
    }
    // Level 6.2 (Table A.8): MaxLumaPs = 35 651 584, each dimension ≤ sqrt(8·MaxLumaPs).
    if sps.width > 16888 || sps.height > 16888 || sps.width as u64 * sps.height as u64 > 35_651_584 {
        return Err(Error::unsupported("picture larger than level 6.2"));
    }
    Ok(())
}

/// §8.3.2 (8-5): the five POC lists. Long-term entries carry their
/// `delta_poc_msb_present_flag` (false = match on LSB only).
#[allow(clippy::type_complexity)]
fn derive_rps(sps: &Sps, sh: &SliceHeader, poc: i32) -> (Vec<i32>, Vec<i32>, Vec<i32>, Vec<(i32, bool)>, Vec<(i32, bool)>) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let mut foll = Vec::new();
    for &(d, used) in &sh.st_rps.neg {
        if used {
            before.push(poc + d);
        } else {
            foll.push(poc + d);
        }
    }
    for &(d, used) in &sh.st_rps.pos {
        if used {
            after.push(poc + d);
        } else {
            foll.push(poc + d);
        }
    }
    let max_poc_lsb = 1i32 << sps.log2_max_poc_lsb;
    let mut lt_curr = Vec::new();
    let mut lt_foll = Vec::new();
    for lt in &sh.long_term {
        let mut p = lt.poc_lsb as i32;
        if lt.delta_poc_msb_present {
            p += poc - lt.delta_poc_msb_cycle as i32 * max_poc_lsb - (poc & (max_poc_lsb - 1));
        }
        if lt.used_by_curr_pic {
            lt_curr.push((p, lt.delta_poc_msb_present));
        } else {
            lt_foll.push((p, lt.delta_poc_msb_present));
        }
    }
    (before, after, foll, lt_curr, lt_foll)
}

/// Finds a reference picture by POC (full or LSB-only) among pictures that
/// are marked as used for reference.
fn find_ref(dpb: &[DpbEntry], poc: i32, lsb_only: bool, max_poc_lsb: i32) -> Option<usize> {
    let m = max_poc_lsb - 1;
    // Prefer an exact match; long-term marking may already apply.
    dpb.iter().position(|e| e.mark != RefMark::Unused && if lsb_only { e.poc & m == poc & m } else { e.poc == poc })
}

/// §8.3.3.2: an unavailable reference picture — mid-grey, not for output.
fn generate_picture(sps: &Sps, poc: i32) -> Picture {
    let mut pic = Picture::new(sps.width as usize, sps.height as usize, sps.chroma_format_idc, sps.bit_depth_luma, sps.bit_depth_chroma);
    let vl = 1u16 << (sps.bit_depth_luma - 1);
    let vc = 1u16 << (sps.bit_depth_chroma - 1);
    pic.planes[0].data.fill(vl);
    pic.planes[1].data.fill(vc);
    pic.planes[2].data.fill(vc);
    pic.poc = poc;
    pic
}

/// §8.3.4: RefPicList0/1 for a P/B slice.
fn build_ref_lists(dpb: &[DpbEntry], sh: &SliceHeader, cur: &CurrentPicture) -> Result<RefLists> {
    let sps = &cur.sps;
    let (before, after, _foll, lt_curr, _lt_foll) = derive_rps(sps, sh, cur.poc);
    let max_poc_lsb = 1i32 << sps.log2_max_poc_lsb;
    let st = |p: i32| -> Result<RefPic> {
        dpb.iter()
            .find(|e| e.mark == RefMark::ShortTerm && e.poc == p)
            .map(|e| RefPic {
                pic: e.pic.clone(),
                poc: e.poc,
                is_long_term: false,
            })
            .ok_or_else(|| Error::invalid(format!("missing short-term reference POC {p}")))
    };
    let lt = |(p, msb): (i32, bool)| -> Result<RefPic> {
        find_ref(dpb, p, !msb, max_poc_lsb)
            .map(|i| RefPic {
                pic: dpb[i].pic.clone(),
                poc: dpb[i].poc,
                is_long_term: true,
            })
            .ok_or_else(|| Error::invalid(format!("missing long-term reference POC {p}")))
    };
    let total = sh.num_pic_total_curr as usize;
    if total == 0 {
        return Err(Error::invalid("no reference pictures for a P/B slice"));
    }
    let mut temp0: Vec<RefPic> = Vec::new();
    let n0 = (sh.num_ref_idx_l0_active as usize).max(total);
    while temp0.len() < n0 {
        for &p in &before {
            if temp0.len() < n0 {
                temp0.push(st(p)?);
            }
        }
        for &p in &after {
            if temp0.len() < n0 {
                temp0.push(st(p)?);
            }
        }
        for &p in &lt_curr {
            if temp0.len() < n0 {
                temp0.push(lt(p)?);
            }
        }
    }
    let pick = |temp: &[RefPic], modif: &Option<Vec<u32>>, n: usize| -> Result<Vec<RefPic>> {
        (0..n)
            .map(|i| {
                let idx = match modif {
                    Some(l) => l[i] as usize,
                    None => i,
                };
                temp.get(idx).cloned().ok_or_else(|| Error::invalid("list_entry out of range"))
            })
            .collect()
    };
    let l0 = pick(&temp0, &sh.ref_pic_list_modification_l0, sh.num_ref_idx_l0_active as usize)?;
    let mut l1 = Vec::new();
    if sh.slice_type.is_b() {
        let n1 = (sh.num_ref_idx_l1_active as usize).max(total);
        let mut temp1: Vec<RefPic> = Vec::new();
        while temp1.len() < n1 {
            for &p in &after {
                if temp1.len() < n1 {
                    temp1.push(st(p)?);
                }
            }
            for &p in &before {
                if temp1.len() < n1 {
                    temp1.push(st(p)?);
                }
            }
            for &p in &lt_curr {
                if temp1.len() < n1 {
                    temp1.push(lt(p)?);
                }
            }
        }
        l1 = pick(&temp1, &sh.ref_pic_list_modification_l1, sh.num_ref_idx_l1_active as usize)?;
    }
    Ok(RefLists { l0, l1 })
}
