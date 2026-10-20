//! Slice segment header (§7.3.6.1), `ref_pic_lists_modification()` (§7.3.6.2)
//! and `pred_weight_table()` (§7.3.6.3).

use crate::bits::BitReader;
use crate::error::{Error, Result};
use crate::nal::NalHeader;
use crate::ps::{parse_st_ref_pic_set, Pps, ShortTermRps, Sps};
// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::{format, vec, vec::Vec};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceType {
    B = 0,
    P = 1,
    I = 2,
}

impl SliceType {
    pub fn is_intra(self) -> bool {
        self == SliceType::I
    }
    pub fn is_b(self) -> bool {
        self == SliceType::B
    }
}

/// One entry of the long-term picture list signalled in the slice header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LongTermEntry {
    pub poc_lsb: u32,
    pub used_by_curr_pic: bool,
    pub delta_poc_msb_present: bool,
    /// `DeltaPocMsbCycleLt` (already accumulated per (7-52)).
    pub delta_poc_msb_cycle: u32,
}

/// Explicit weighted-prediction weights for one list entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WeightEntry {
    pub luma_weight: i32,
    pub luma_offset: i32,
    pub chroma_weight: [i32; 2],
    pub chroma_offset: [i32; 2],
}

#[derive(Debug, Clone, Default)]
pub struct PredWeightTable {
    pub luma_log2_weight_denom: u32,
    pub chroma_log2_weight_denom: u32,
    pub l0: Vec<WeightEntry>,
    pub l1: Vec<WeightEntry>,
}

/// A parsed slice segment header. Dependent slice segments copy the
/// independent header's fields (everything from `slice_type` down) from the
/// preceding independent segment.
#[derive(Debug, Clone)]
pub struct SliceHeader {
    pub first_slice_segment_in_pic: bool,
    pub no_output_of_prior_pics: bool,
    pub pps_id: u8,
    pub dependent_slice_segment: bool,
    pub segment_address: u32,
    pub slice_type: SliceType,
    pub pic_output_flag: bool,
    pub colour_plane_id: u8,
    pub poc_lsb: u32,
    pub short_term_ref_pic_set_sps_flag: bool,
    /// The active short-term RPS (from the SPS by index, or parsed here).
    pub st_rps: ShortTermRps,
    /// Bits consumed by an in-header `st_ref_pic_set` (for `NumBitsForShortTermRefPicSetInSlice`).
    pub st_rps_bits: u32,
    pub long_term: Vec<LongTermEntry>,
    pub num_long_term_sps: u32,
    pub temporal_mvp_enabled: bool,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub num_ref_idx_l0_active: u8,
    pub num_ref_idx_l1_active: u8,
    pub ref_pic_list_modification_l0: Option<Vec<u32>>,
    pub ref_pic_list_modification_l1: Option<Vec<u32>>,
    pub mvd_l1_zero: bool,
    pub cabac_init_flag: bool,
    pub collocated_from_l0: bool,
    pub collocated_ref_idx: u8,
    pub pred_weight: Option<PredWeightTable>,
    pub max_num_merge_cand: u8,
    pub slice_qp_delta: i32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub deblocking_filter_disabled: bool,
    pub beta_offset_div2: i32,
    pub tc_offset_div2: i32,
    pub loop_filter_across_slices_enabled: bool,
    /// `entry_point_offset_minus1[i] + 1`, in escaped NAL bytes.
    pub entry_point_offsets: Vec<u32>,
    /// RBSP byte offset where `slice_segment_data()` starts.
    pub data_offset: usize,
    /// `SliceQpY`.
    pub slice_qp: i32,
    /// `NumPicTotalCurr` (§7.4.7.2).
    pub num_pic_total_curr: u32,
}

impl SliceHeader {
    /// Number of entry-point substreams (`num_entry_point_offsets + 1`).
    pub fn num_substreams(&self) -> usize {
        self.entry_point_offsets.len() + 1
    }
}

fn ceil_log2(v: u32) -> u32 {
    if v <= 1 {
        0
    } else {
        32 - (v - 1).leading_zeros()
    }
}

fn parse_pred_weight_table(r: &mut BitReader, sps: &Sps, sh: &SliceHeader) -> Result<PredWeightTable> {
    let mut t = PredWeightTable {
        luma_log2_weight_denom: r.read_ue_max(7)?,
        ..Default::default()
    };
    if sps.chroma_array_type != 0 {
        let d = r.read_se()?;
        let c = t.luma_log2_weight_denom as i32 + d;
        if !(0..=7).contains(&c) {
            return Err(Error::invalid("ChromaLog2WeightDenom"));
        }
        t.chroma_log2_weight_denom = c as u32;
    }
    let high_precision = false; // sps_range_extension.high_precision_offsets_enabled_flag (RExt)
    let offset_shift_l = if high_precision { 0 } else { sps.bit_depth_luma as i32 - 8 };
    let offset_shift_c = if high_precision { 0 } else { sps.bit_depth_chroma as i32 - 8 };
    let wp_offset_half_c = 1 << (if high_precision { sps.bit_depth_chroma - 1 } else { 7 });
    for list in 0..(if sh.slice_type.is_b() { 2 } else { 1 }) {
        let n = if list == 0 { sh.num_ref_idx_l0_active } else { sh.num_ref_idx_l1_active } as usize;
        let mut luma_flags = vec![false; n];
        let mut chroma_flags = vec![false; n];
        for f in luma_flags.iter_mut() {
            *f = r.read_flag()?;
        }
        if sps.chroma_array_type != 0 {
            for f in chroma_flags.iter_mut() {
                *f = r.read_flag()?;
            }
        }
        let mut entries = Vec::with_capacity(n);
        for i in 0..n {
            let mut e = WeightEntry {
                luma_weight: 1 << t.luma_log2_weight_denom,
                luma_offset: 0,
                chroma_weight: [1 << t.chroma_log2_weight_denom; 2],
                chroma_offset: [0; 2],
            };
            if luma_flags[i] {
                let dw = r.read_se()?;
                if !(-128..=127).contains(&dw) {
                    return Err(Error::invalid("delta_luma_weight"));
                }
                e.luma_weight += dw;
                let off = r.read_se()?;
                if !(-128..=127).contains(&off) {
                    return Err(Error::invalid("luma_offset"));
                }
                e.luma_offset = off << offset_shift_l;
            }
            if chroma_flags[i] {
                for j in 0..2 {
                    let dw = r.read_se()?;
                    if !(-128..=127).contains(&dw) {
                        return Err(Error::invalid("delta_chroma_weight"));
                    }
                    e.chroma_weight[j] += dw;
                    let d_off = r.read_se()?;
                    if !(-512..=511).contains(&d_off) {
                        return Err(Error::invalid("delta_chroma_offset"));
                    }
                    // (7-56)
                    let v = (wp_offset_half_c + d_off - ((wp_offset_half_c * e.chroma_weight[j]) >> t.chroma_log2_weight_denom)).clamp(-wp_offset_half_c, wp_offset_half_c - 1);
                    e.chroma_offset[j] = v << offset_shift_c;
                }
            }
            entries.push(e);
        }
        if list == 0 {
            t.l0 = entries;
        } else {
            t.l1 = entries;
        }
    }
    Ok(t)
}

/// Parses a slice segment header. `prev` is the last independent slice
/// segment header of the same picture (needed for dependent segments).
/// The reader is left at the start of `slice_segment_data()`.
pub fn parse_slice_header<'a>(r: &mut BitReader, nal: &NalHeader, sps_by_pps: &dyn Fn(u8) -> Option<(&'a Sps, &'a Pps)>, prev: Option<&SliceHeader>) -> Result<SliceHeader> {
    let first_slice_segment_in_pic = r.read_flag()?;
    let mut no_output_of_prior_pics = false;
    if nal.nal_type.is_irap() {
        no_output_of_prior_pics = r.read_flag()?;
    }
    let pps_id = r.read_ue_max(63)? as u8;
    let (sps, pps) = sps_by_pps(pps_id).ok_or_else(|| Error::invalid(format!("slice refers to unknown PPS {pps_id}")))?;

    let mut dependent_slice_segment = false;
    let mut segment_address = 0;
    if !first_slice_segment_in_pic {
        if pps.dependent_slice_segments_enabled {
            dependent_slice_segment = r.read_flag()?;
        }
        segment_address = r.read_bits(ceil_log2(sps.pic_size_in_ctbs))?;
        if segment_address >= sps.pic_size_in_ctbs {
            return Err(Error::invalid("slice_segment_address out of range"));
        }
    }

    let mut sh = if dependent_slice_segment {
        let p = prev.ok_or_else(|| Error::invalid("dependent slice segment without an independent one"))?;
        let mut sh = p.clone();
        sh.first_slice_segment_in_pic = first_slice_segment_in_pic;
        sh.no_output_of_prior_pics = no_output_of_prior_pics;
        sh.dependent_slice_segment = true;
        sh.segment_address = segment_address;
        sh.entry_point_offsets.clear();
        sh
    } else {
        SliceHeader {
            first_slice_segment_in_pic,
            no_output_of_prior_pics,
            pps_id,
            dependent_slice_segment: false,
            segment_address,
            slice_type: SliceType::I,
            pic_output_flag: true,
            colour_plane_id: 0,
            poc_lsb: 0,
            short_term_ref_pic_set_sps_flag: false,
            st_rps: ShortTermRps::default(),
            st_rps_bits: 0,
            long_term: Vec::new(),
            num_long_term_sps: 0,
            temporal_mvp_enabled: false,
            sao_luma: false,
            sao_chroma: false,
            num_ref_idx_l0_active: 0,
            num_ref_idx_l1_active: 0,
            ref_pic_list_modification_l0: None,
            ref_pic_list_modification_l1: None,
            mvd_l1_zero: false,
            cabac_init_flag: false,
            collocated_from_l0: true,
            collocated_ref_idx: 0,
            pred_weight: None,
            max_num_merge_cand: 5,
            slice_qp_delta: 0,
            cb_qp_offset: 0,
            cr_qp_offset: 0,
            deblocking_filter_disabled: pps.deblocking_filter_disabled,
            beta_offset_div2: pps.beta_offset_div2,
            tc_offset_div2: pps.tc_offset_div2,
            loop_filter_across_slices_enabled: pps.loop_filter_across_slices_enabled,
            entry_point_offsets: Vec::new(),
            data_offset: 0,
            slice_qp: pps.init_qp,
            num_pic_total_curr: 0,
        }
    };

    if !dependent_slice_segment {
        for _ in 0..pps.num_extra_slice_header_bits {
            r.read_flag()?; // slice_reserved_flag
        }
        sh.slice_type = match r.read_ue_max(2)? {
            0 => SliceType::B,
            1 => SliceType::P,
            _ => SliceType::I,
        };
        if pps.output_flag_present {
            sh.pic_output_flag = r.read_flag()?;
        }
        if sps.separate_colour_plane_flag {
            sh.colour_plane_id = r.read_bits(2)? as u8;
        }
        if !nal.nal_type.is_idr() {
            sh.poc_lsb = r.read_bits(sps.log2_max_poc_lsb as u32)?;
            sh.short_term_ref_pic_set_sps_flag = r.read_flag()?;
            if !sh.short_term_ref_pic_set_sps_flag {
                let start = r.bit_pos();
                sh.st_rps = parse_st_ref_pic_set(r, sps.st_rps.len(), &sps.st_rps, true)?;
                sh.st_rps_bits = (r.bit_pos() - start) as u32;
            } else {
                if sps.st_rps.is_empty() {
                    return Err(Error::invalid("short_term_ref_pic_set_sps_flag with no SPS sets"));
                }
                let idx = if sps.st_rps.len() > 1 { r.read_bits(ceil_log2(sps.st_rps.len() as u32))? as usize } else { 0 };
                sh.st_rps = sps.st_rps.get(idx).ok_or_else(|| Error::invalid("short_term_ref_pic_set_idx"))?.clone();
            }
            if sps.long_term_ref_pics_present {
                if !sps.lt_ref_pics.is_empty() {
                    sh.num_long_term_sps = r.read_ue_max(sps.lt_ref_pics.len() as u32)?;
                }
                let num_long_term_pics = r.read_ue_max(32)?;
                let total = sh.num_long_term_sps + num_long_term_pics;
                let mut prev_cycle = 0u32;
                for i in 0..total {
                    let (poc_lsb, used) = if i < sh.num_long_term_sps {
                        let idx = if sps.lt_ref_pics.len() > 1 {
                            r.read_bits(ceil_log2(sps.lt_ref_pics.len() as u32))? as usize
                        } else {
                            0
                        };
                        sps.lt_ref_pics[idx]
                    } else {
                        let lsb = r.read_bits(sps.log2_max_poc_lsb as u32)?;
                        (lsb, r.read_flag()?)
                    };
                    let delta_poc_msb_present = r.read_flag()?;
                    let mut cycle = 0;
                    if delta_poc_msb_present {
                        cycle = r.read_ue_max(1 << 24)?;
                    }
                    // (7-52)
                    if i != 0 && i != sh.num_long_term_sps {
                        cycle += prev_cycle;
                    }
                    prev_cycle = cycle;
                    sh.long_term.push(LongTermEntry {
                        poc_lsb,
                        used_by_curr_pic: used,
                        delta_poc_msb_present,
                        delta_poc_msb_cycle: cycle,
                    });
                }
            }
            if sps.temporal_mvp_enabled {
                sh.temporal_mvp_enabled = r.read_flag()?;
            }
        }
        if sps.sao_enabled {
            sh.sao_luma = r.read_flag()?;
            if sps.chroma_array_type != 0 {
                sh.sao_chroma = r.read_flag()?;
            }
        }
        // NumPicTotalCurr (7-55)
        let mut total = 0;
        for &(_, u) in sh.st_rps.neg.iter().chain(sh.st_rps.pos.iter()) {
            total += u as u32;
        }
        for lt in &sh.long_term {
            total += lt.used_by_curr_pic as u32;
        }
        sh.num_pic_total_curr = total;

        if !sh.slice_type.is_intra() {
            sh.num_ref_idx_l0_active = pps.num_ref_idx_l0_default_active;
            sh.num_ref_idx_l1_active = if sh.slice_type.is_b() { pps.num_ref_idx_l1_default_active } else { 0 };
            if r.read_flag()? {
                sh.num_ref_idx_l0_active = r.read_ue_max(14)? as u8 + 1;
                if sh.slice_type.is_b() {
                    sh.num_ref_idx_l1_active = r.read_ue_max(14)? as u8 + 1;
                }
            }
            if sh.num_pic_total_curr == 0 {
                return Err(Error::invalid("P/B slice with NumPicTotalCurr == 0"));
            }
            if pps.lists_modification_present && sh.num_pic_total_curr > 1 {
                let bits = ceil_log2(sh.num_pic_total_curr);
                if r.read_flag()? {
                    let mut l = Vec::with_capacity(sh.num_ref_idx_l0_active as usize);
                    for _ in 0..sh.num_ref_idx_l0_active {
                        l.push(r.read_bits(bits)?);
                    }
                    sh.ref_pic_list_modification_l0 = Some(l);
                }
                if sh.slice_type.is_b() && r.read_flag()? {
                    let mut l = Vec::with_capacity(sh.num_ref_idx_l1_active as usize);
                    for _ in 0..sh.num_ref_idx_l1_active {
                        l.push(r.read_bits(bits)?);
                    }
                    sh.ref_pic_list_modification_l1 = Some(l);
                }
            }
            if sh.slice_type.is_b() {
                sh.mvd_l1_zero = r.read_flag()?;
            }
            if pps.cabac_init_present {
                sh.cabac_init_flag = r.read_flag()?;
            }
            if sh.temporal_mvp_enabled {
                if sh.slice_type.is_b() {
                    sh.collocated_from_l0 = r.read_flag()?;
                }
                if (sh.collocated_from_l0 && sh.num_ref_idx_l0_active > 1) || (!sh.collocated_from_l0 && sh.num_ref_idx_l1_active > 1) {
                    sh.collocated_ref_idx = r.read_ue_max(14)? as u8;
                    let n = if sh.collocated_from_l0 { sh.num_ref_idx_l0_active } else { sh.num_ref_idx_l1_active };
                    if sh.collocated_ref_idx >= n {
                        return Err(Error::invalid("collocated_ref_idx out of range"));
                    }
                }
            }
            if (pps.weighted_pred && sh.slice_type == SliceType::P) || (pps.weighted_bipred && sh.slice_type.is_b()) {
                sh.pred_weight = Some(parse_pred_weight_table(r, sps, &sh)?);
            }
            let five_minus = r.read_ue_max(4)?;
            sh.max_num_merge_cand = 5 - five_minus as u8;
        }
        sh.slice_qp_delta = r.read_se()?;
        sh.slice_qp = pps.init_qp + sh.slice_qp_delta;
        if sh.slice_qp < -sps.qp_bd_offset_y || sh.slice_qp > 51 {
            return Err(Error::invalid("SliceQpY out of range"));
        }
        if pps.slice_chroma_qp_offsets_present {
            sh.cb_qp_offset = r.read_se()?;
            sh.cr_qp_offset = r.read_se()?;
            if !(-12..=12).contains(&sh.cb_qp_offset)
                || !(-12..=12).contains(&sh.cr_qp_offset)
                || !(-12..=12).contains(&(pps.cb_qp_offset + sh.cb_qp_offset))
                || !(-12..=12).contains(&(pps.cr_qp_offset + sh.cr_qp_offset))
            {
                return Err(Error::invalid("slice_cb/cr_qp_offset"));
            }
        }
        if pps.chroma_qp_offset_list_enabled {
            r.read_flag()?; // cu_chroma_qp_offset_enabled_flag (RExt)
        }
        let mut deblocking_override = false;
        if pps.deblocking_filter_override_enabled {
            deblocking_override = r.read_flag()?;
        }
        if deblocking_override {
            sh.deblocking_filter_disabled = r.read_flag()?;
            if !sh.deblocking_filter_disabled {
                sh.beta_offset_div2 = r.read_se()?;
                sh.tc_offset_div2 = r.read_se()?;
                if !(-6..=6).contains(&sh.beta_offset_div2) || !(-6..=6).contains(&sh.tc_offset_div2) {
                    return Err(Error::invalid("slice beta/tc offset"));
                }
            }
        }
        if pps.loop_filter_across_slices_enabled && (sh.sao_luma || sh.sao_chroma || !sh.deblocking_filter_disabled) {
            sh.loop_filter_across_slices_enabled = r.read_flag()?;
        }
    }

    if pps.tiles_enabled || pps.entropy_coding_sync_enabled {
        let num = r.read_ue_max(sps.pic_size_in_ctbs.max(1) * 2)?;
        if num > 0 {
            let len = r.read_ue_max(31)? + 1;
            for _ in 0..num {
                sh.entry_point_offsets.push(r.read_bits(len)?.wrapping_add(1));
            }
        }
    }
    if pps.slice_segment_header_extension_present {
        let len = r.read_ue_max(256)?;
        for _ in 0..len {
            r.read_bits(8)?;
        }
    }
    // byte_alignment(): a one bit then zeros to the boundary.
    if !r.read_flag()? {
        return Err(Error::invalid("alignment_bit_equal_to_one"));
    }
    r.align_to_byte()?;
    sh.data_offset = r.byte_pos();
    Ok(sh)
}
