//! Parameter sets: VPS (§7.3.2.1), SPS (§7.3.2.2), PPS (§7.3.2.3), with
//! `profile_tier_level` (§7.3.3), `scaling_list_data` (§7.3.4),
//! `st_ref_pic_set` (§7.3.7) and the VUI/HRD skip (Annex E).
//!
//! Everything a later stage needs is derived here once (CTB geometry, tile
//! boundaries, CtbAddrRsToTs, scaling factors) so the slice decoder reads
//! fields, never recomputes them.

use crate::bits::BitReader;
use crate::error::{Error, Result};
// VENDOR(uefi-player): alloc prelude for the no_std build.
use alloc::{vec, vec::Vec};

/// `general_profile_idc` values we know by name (Annex A).
pub const PROFILE_MAIN: u8 = 1;
pub const PROFILE_MAIN10: u8 = 2;
pub const PROFILE_MAIN_STILL: u8 = 3;
pub const PROFILE_REXT: u8 = 4;

/// `profile_tier_level()` — the general (top) layer only; sub-layer PTL is
/// skipped exactly (its presence flags still have to be read).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProfileTierLevel {
    pub profile_space: u8,
    pub tier_flag: bool,
    pub profile_idc: u8,
    pub profile_compatibility_flags: u32,
    pub progressive_source_flag: bool,
    pub interlaced_source_flag: bool,
    pub non_packed_constraint_flag: bool,
    pub frame_only_constraint_flag: bool,
    pub level_idc: u8,
}

impl ProfileTierLevel {
    /// The profile as decided by `general_profile_idc` or the compatibility
    /// flags (a stream may signal idc 0 and flag its profile through the
    /// compatibility bits).
    pub fn profile(&self) -> u8 {
        if self.profile_idc != 0 {
            return self.profile_idc;
        }
        for p in [PROFILE_MAIN, PROFILE_MAIN10, PROFILE_MAIN_STILL, PROFILE_REXT] {
            if self.profile_compatibility_flags & (1 << (31 - p)) != 0 {
                return p;
            }
        }
        0
    }
}

fn parse_ptl(r: &mut BitReader, profile_present: bool, max_sub_layers_minus1: u32) -> Result<ProfileTierLevel> {
    let mut ptl = ProfileTierLevel::default();
    if profile_present {
        ptl.profile_space = r.read_bits(2)? as u8;
        ptl.tier_flag = r.read_flag()?;
        ptl.profile_idc = r.read_bits(5)? as u8;
        ptl.profile_compatibility_flags = r.read_bits(32)?;
        ptl.progressive_source_flag = r.read_flag()?;
        ptl.interlaced_source_flag = r.read_flag()?;
        ptl.non_packed_constraint_flag = r.read_flag()?;
        ptl.frame_only_constraint_flag = r.read_flag()?;
        // general_reserved_zero_43bits + general_inbld_flag / reserved (44 bits)
        r.read_bits(32)?;
        r.read_bits(12)?;
    }
    ptl.level_idc = r.read_bits(8)? as u8;
    let n = max_sub_layers_minus1 as usize;
    let mut sub_profile = [false; 8];
    let mut sub_level = [false; 8];
    for i in 0..n {
        sub_profile[i] = r.read_flag()?;
        sub_level[i] = r.read_flag()?;
    }
    if n > 0 {
        for _ in n..8 {
            r.read_bits(2)?; // reserved_zero_2bits
        }
    }
    for i in 0..n {
        if sub_profile[i] {
            r.read_bits(32)?;
            r.read_bits(32)?;
            r.read_bits(24)?; // 88 bits
        }
        if sub_level[i] {
            r.read_bits(8)?;
        }
    }
    Ok(ptl)
}

/// `sps_max_dec_pic_buffering_minus1` etc. per sub-layer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubLayerOrdering {
    pub max_dec_pic_buffering_minus1: u32,
    pub max_num_reorder_pics: u32,
    pub max_latency_increase_plus1: u32,
}

fn parse_sub_layer_ordering(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<[SubLayerOrdering; 8]> {
    let present = r.read_flag()?;
    let mut out = [SubLayerOrdering::default(); 8];
    let start = if present { 0 } else { max_sub_layers_minus1 };
    for i in start..=max_sub_layers_minus1 {
        let o = SubLayerOrdering {
            max_dec_pic_buffering_minus1: r.read_ue_max(16)?,
            max_num_reorder_pics: r.read_ue_max(16)?,
            max_latency_increase_plus1: r.read_ue()?,
        };
        out[i as usize] = o;
    }
    if !present {
        for i in 0..max_sub_layers_minus1 as usize {
            out[i] = out[max_sub_layers_minus1 as usize];
        }
    }
    Ok(out)
}

/// Video parameter set (§7.3.2.1). Nothing in it is needed to decode a
/// single-layer stream; it is parsed to validate the syntax and to expose the
/// sub-layer info.
#[derive(Debug, Clone, Default)]
pub struct Vps {
    pub id: u8,
    pub max_layers_minus1: u8,
    pub max_sub_layers_minus1: u8,
    pub temporal_id_nesting_flag: bool,
    pub ptl: ProfileTierLevel,
    pub ordering: [SubLayerOrdering; 8],
    pub max_layer_id: u8,
    pub num_layer_sets_minus1: u32,
    pub timing_info_present: bool,
    pub num_units_in_tick: u32,
    pub time_scale: u32,
}

/// Parses a VPS RBSP.
pub fn parse_vps(rbsp: &[u8]) -> Result<Vps> {
    let mut r = BitReader::new(rbsp);
    let mut v = Vps {
        id: r.read_bits(4)? as u8,
        ..Default::default()
    };
    // vps_base_layer_internal_flag + vps_base_layer_available_flag
    // (vps_reserved_three_2bits in v1: must be 3)
    r.read_bits(2)?;
    v.max_layers_minus1 = r.read_bits(6)? as u8;
    v.max_sub_layers_minus1 = r.read_bits(3)? as u8;
    if v.max_sub_layers_minus1 > 6 {
        return Err(Error::invalid("vps_max_sub_layers_minus1 > 6"));
    }
    v.temporal_id_nesting_flag = r.read_flag()?;
    if r.read_bits(16)? != 0xffff {
        return Err(Error::invalid("vps_reserved_0xffff_16bits"));
    }
    v.ptl = parse_ptl(&mut r, true, v.max_sub_layers_minus1 as u32)?;
    v.ordering = parse_sub_layer_ordering(&mut r, v.max_sub_layers_minus1 as u32)?;
    v.max_layer_id = r.read_bits(6)? as u8;
    v.num_layer_sets_minus1 = r.read_ue_max(1023)?;
    for _ in 1..=v.num_layer_sets_minus1 {
        for _ in 0..=v.max_layer_id {
            r.read_flag()?; // layer_id_included_flag
        }
    }
    v.timing_info_present = r.read_flag()?;
    if v.timing_info_present {
        v.num_units_in_tick = r.read_bits(32)?;
        v.time_scale = r.read_bits(32)?;
        if r.read_flag()? {
            r.read_ue()?; // vps_num_ticks_poc_diff_one_minus1
        }
        let num_hrd = r.read_ue_max(1024)?;
        for i in 0..num_hrd {
            r.read_ue()?; // hrd_layer_set_idx
            let cprms_present = if i > 0 { r.read_flag()? } else { true };
            skip_hrd_parameters(&mut r, cprms_present, v.max_sub_layers_minus1 as u32)?;
        }
    }
    // vps_extension_flag and beyond: ignored.
    Ok(v)
}

fn skip_sub_layer_hrd(r: &mut BitReader, cpb_cnt: u32, sub_pic_params_present: bool) -> Result<()> {
    for _ in 0..cpb_cnt {
        r.read_ue()?; // bit_rate_value_minus1
        r.read_ue()?; // cpb_size_value_minus1
        if sub_pic_params_present {
            r.read_ue()?; // cpb_size_du_value_minus1
            r.read_ue()?; // bit_rate_du_value_minus1
        }
        r.read_flag()?; // cbr_flag
    }
    Ok(())
}

/// `hrd_parameters()` (§E.2.2) — skipped exactly.
fn skip_hrd_parameters(r: &mut BitReader, common_inf_present: bool, max_sub_layers_minus1: u32) -> Result<()> {
    let mut nal_hrd = false;
    let mut vcl_hrd = false;
    let mut sub_pic_params_present = false;
    if common_inf_present {
        nal_hrd = r.read_flag()?;
        vcl_hrd = r.read_flag()?;
        if nal_hrd || vcl_hrd {
            sub_pic_params_present = r.read_flag()?;
            if sub_pic_params_present {
                r.read_bits(8)?; // tick_divisor_minus2
                r.read_bits(5)?; // du_cpb_removal_delay_increment_length_minus1
                r.read_flag()?; // sub_pic_cpb_params_in_pic_timing_sei_flag
                r.read_bits(5)?; // dpb_output_delay_du_length_minus1
            }
            r.read_bits(4)?; // bit_rate_scale
            r.read_bits(4)?; // cpb_size_scale
            if sub_pic_params_present {
                r.read_bits(4)?; // cpb_size_du_scale
            }
            r.read_bits(5)?; // initial_cpb_removal_delay_length_minus1
            r.read_bits(5)?; // au_cpb_removal_delay_length_minus1
            r.read_bits(5)?; // dpb_output_delay_length_minus1
        }
    }
    for _ in 0..=max_sub_layers_minus1 {
        let fixed_pic_rate_general = r.read_flag()?;
        let mut fixed_pic_rate_within_cvs = true;
        if !fixed_pic_rate_general {
            fixed_pic_rate_within_cvs = r.read_flag()?;
        }
        let mut low_delay = false;
        if fixed_pic_rate_within_cvs {
            r.read_ue()?; // elemental_duration_in_tc_minus1
        } else {
            low_delay = r.read_flag()?;
        }
        let mut cpb_cnt = 1;
        if !low_delay {
            cpb_cnt = r.read_ue_max(31)? + 1;
        }
        if nal_hrd {
            skip_sub_layer_hrd(r, cpb_cnt, sub_pic_params_present)?;
        }
        if vcl_hrd {
            skip_sub_layer_hrd(r, cpb_cnt, sub_pic_params_present)?;
        }
    }
    Ok(())
}

/// Scaling lists (§7.3.4 / §7.4.5): `ScalingFactor` is derived by the
/// dequantizer; here we keep the coded lists in coefficient (diagonal-scan)
/// order plus the DC values for the 16×16 and 32×32 sizes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScalingList {
    /// `[sizeId][matrixId][i]`: 16 entries for sizeId 0, 64 otherwise.
    pub lists: [[[u8; 64]; 6]; 4],
    /// `scaling_list_dc_coef_minus8 + 8` for sizeId 2 and 3 (index 0/1).
    pub dc: [[u8; 6]; 2],
}

/// Table 7-6, intra (matrixId 0..2) for sizeId 1..3, in diagonal scan order.
pub const DEFAULT_SCALING_INTRA: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 16, 17, 16, 17, 18, 17, 18, 18, 17, 18, 21, 19, 20, 21, 20, 19, 21, 24, 22, 22, 24, 24, 22, 22, 24, 25, 25, 27, 30, 27, 25, 25, 29, 31, 35, 35, 31, 29,
    36, 41, 44, 41, 36, 47, 54, 54, 47, 65, 70, 65, 88, 88, 115,
];
/// Table 7-6, inter (matrixId 3..5).
pub const DEFAULT_SCALING_INTER: [u8; 64] = [
    16, 16, 16, 16, 16, 16, 16, 16, 16, 16, 17, 17, 17, 17, 17, 18, 18, 18, 18, 18, 18, 20, 20, 20, 20, 20, 20, 20, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 28, 28, 28, 28, 28, 28,
    33, 33, 33, 33, 33, 41, 41, 41, 41, 54, 54, 54, 71, 71, 91,
];

impl Default for ScalingList {
    /// The default lists (Table 7-5 / 7-6), `scaling_list_enabled_flag = 1`
    /// without explicit data.
    fn default() -> Self {
        let mut lists = [[[16u8; 64]; 6]; 4];
        for size_id in 1..4 {
            for matrix_id in 0..6 {
                lists[size_id][matrix_id] = if matrix_id < 3 { DEFAULT_SCALING_INTRA } else { DEFAULT_SCALING_INTER };
            }
        }
        ScalingList { lists, dc: [[16; 6]; 2] }
    }
}

fn parse_scaling_list_data(r: &mut BitReader) -> Result<ScalingList> {
    let mut sl = ScalingList::default();
    for size_id in 0..4usize {
        let step = if size_id == 3 { 3 } else { 1 };
        let coef_num = 64usize.min(1 << (4 + (size_id << 1)));
        let mut matrix_id = 0usize;
        while matrix_id < 6 {
            let pred_mode_flag = r.read_flag()?;
            if !pred_mode_flag {
                let delta = r.read_ue_max(5)? as usize * step;
                if delta == 0 {
                    // default list
                    sl.lists[size_id][matrix_id] = if size_id == 0 {
                        [16; 64]
                    } else if matrix_id < 3 {
                        DEFAULT_SCALING_INTRA
                    } else {
                        DEFAULT_SCALING_INTER
                    };
                    if size_id > 1 {
                        sl.dc[size_id - 2][matrix_id] = 16;
                    }
                } else {
                    if delta > matrix_id {
                        return Err(Error::invalid("scaling_list_pred_matrix_id_delta"));
                    }
                    let ref_id = matrix_id - delta;
                    sl.lists[size_id][matrix_id] = sl.lists[size_id][ref_id];
                    if size_id > 1 {
                        sl.dc[size_id - 2][matrix_id] = sl.dc[size_id - 2][ref_id];
                    }
                }
            } else {
                let mut next_coef: i32 = 8;
                if size_id > 1 {
                    let dc = r.read_se()?;
                    if !(-7..=247).contains(&dc) {
                        return Err(Error::invalid("scaling_list_dc_coef_minus8"));
                    }
                    next_coef = dc + 8;
                    sl.dc[size_id - 2][matrix_id] = next_coef as u8;
                }
                for i in 0..coef_num {
                    let delta = r.read_se()?;
                    if !(-128..=127).contains(&delta) {
                        return Err(Error::invalid("scaling_list_delta_coef"));
                    }
                    next_coef = (next_coef + delta + 256) & 255;
                    sl.lists[size_id][matrix_id][i] = next_coef as u8;
                }
            }
            matrix_id += step;
        }
    }
    // 32×32 chroma lists (matrixId 1,2,4,5 of sizeId 3) are inferred from
    // the 16×16 ones when ChromaArrayType == 3 (§7.4.5); for 4:2:0 they are
    // never used. Copy them anyway so every index is defined.
    for m in [1usize, 2, 4, 5] {
        sl.lists[3][m] = sl.lists[2][m];
        sl.dc[1][m] = sl.dc[0][m];
    }
    Ok(sl)
}

/// A short-term reference picture set (§7.4.8), fully derived.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShortTermRps {
    /// `DeltaPocS0` (negative, descending: closest first) and `UsedByCurrPicS0`.
    pub neg: Vec<(i32, bool)>,
    /// `DeltaPocS1` (positive, ascending) and `UsedByCurrPicS1`.
    pub pos: Vec<(i32, bool)>,
}

impl ShortTermRps {
    pub fn num_delta_pocs(&self) -> usize {
        self.neg.len() + self.pos.len()
    }
}

/// `st_ref_pic_set(stRpsIdx)`. `sets` holds the SPS sets parsed so far;
/// `idx == sets.len()` when parsing the slice-header set.
pub fn parse_st_ref_pic_set(r: &mut BitReader, idx: usize, sets: &[ShortTermRps], in_slice_header: bool) -> Result<ShortTermRps> {
    let inter_rps_pred = if idx != 0 { r.read_flag()? } else { false };
    let mut rps = ShortTermRps::default();
    if inter_rps_pred {
        let delta_idx_minus1 = if in_slice_header { r.read_ue_max(idx as u32 - 1)? as usize } else { 0 };
        let delta_rps_sign = r.read_flag()?;
        let abs_delta_rps_minus1 = r.read_ue_max(32767)?;
        let ref_idx = idx - (delta_idx_minus1 + 1);
        let delta_rps = (1 - 2 * delta_rps_sign as i32) * (abs_delta_rps_minus1 as i32 + 1);
        let rf = sets.get(ref_idx).ok_or_else(|| Error::invalid("st_ref_pic_set ref index"))?;
        let n = rf.num_delta_pocs();
        let mut used = vec![false; n + 1];
        let mut use_delta = vec![true; n + 1];
        for j in 0..=n {
            used[j] = r.read_flag()?;
            if !used[j] {
                use_delta[j] = r.read_flag()?;
            }
        }
        // (7-61)
        for j in (0..rf.pos.len()).rev() {
            let d = rf.pos[j].0 + delta_rps;
            let k = rf.neg.len() + j;
            if d < 0 && use_delta[k] {
                rps.neg.push((d, used[k]));
            }
        }
        if delta_rps < 0 && use_delta[n] {
            rps.neg.push((delta_rps, used[n]));
        }
        for j in 0..rf.neg.len() {
            let d = rf.neg[j].0 + delta_rps;
            if d < 0 && use_delta[j] {
                rps.neg.push((d, used[j]));
            }
        }
        // (7-62)
        for j in (0..rf.neg.len()).rev() {
            let d = rf.neg[j].0 + delta_rps;
            if d > 0 && use_delta[j] {
                rps.pos.push((d, used[j]));
            }
        }
        if delta_rps > 0 && use_delta[n] {
            rps.pos.push((delta_rps, used[n]));
        }
        for j in 0..rf.pos.len() {
            let d = rf.pos[j].0 + delta_rps;
            let k = rf.neg.len() + j;
            if d > 0 && use_delta[k] {
                rps.pos.push((d, used[k]));
            }
        }
    } else {
        let num_negative = r.read_ue_max(16)?;
        let num_positive = r.read_ue_max(16)?;
        let mut prev = 0i32;
        for _ in 0..num_negative {
            let delta = r.read_ue_max(32767)? as i32 + 1;
            let used = r.read_flag()?;
            prev -= delta;
            rps.neg.push((prev, used));
        }
        prev = 0;
        for _ in 0..num_positive {
            let delta = r.read_ue_max(32767)? as i32 + 1;
            let used = r.read_flag()?;
            prev += delta;
            rps.pos.push((prev, used));
        }
    }
    if rps.num_delta_pocs() > 16 {
        return Err(Error::invalid("st_ref_pic_set has more than 16 pictures"));
    }
    Ok(rps)
}

/// The VUI fields the decoder itself needs (the rest is skipped exactly).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Vui {
    pub sar_width: u16,
    pub sar_height: u16,
    pub video_full_range_flag: bool,
    pub colour_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coeffs: u8,
    pub timing_info_present: bool,
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub min_spatial_segmentation_idc: u32,
    /// `default_display_window` offsets (in chroma units like the conf window).
    pub def_disp_win: [u32; 4],
}

fn parse_vui(r: &mut BitReader, max_sub_layers_minus1: u32) -> Result<Vui> {
    let mut v = Vui::default();
    if r.read_flag()? {
        let aspect_ratio_idc = r.read_bits(8)?;
        if aspect_ratio_idc == 255 {
            v.sar_width = r.read_bits(16)? as u16;
            v.sar_height = r.read_bits(16)? as u16;
        }
    }
    if r.read_flag()? {
        r.read_flag()?; // overscan_appropriate_flag
    }
    if r.read_flag()? {
        r.read_bits(3)?; // video_format
        v.video_full_range_flag = r.read_flag()?;
        if r.read_flag()? {
            v.colour_primaries = r.read_bits(8)? as u8;
            v.transfer_characteristics = r.read_bits(8)? as u8;
            v.matrix_coeffs = r.read_bits(8)? as u8;
        }
    }
    if r.read_flag()? {
        r.read_ue()?; // chroma_sample_loc_type_top_field
        r.read_ue()?; // chroma_sample_loc_type_bottom_field
    }
    r.read_flag()?; // neutral_chroma_indication_flag
    r.read_flag()?; // field_seq_flag
    r.read_flag()?; // frame_field_info_present_flag
    if r.read_flag()? {
        for o in v.def_disp_win.iter_mut() {
            *o = r.read_ue()?;
        }
    }
    v.timing_info_present = r.read_flag()?;
    if v.timing_info_present {
        v.num_units_in_tick = r.read_bits(32)?;
        v.time_scale = r.read_bits(32)?;
        if r.read_flag()? {
            r.read_ue()?; // vui_num_ticks_poc_diff_one_minus1
        }
        if r.read_flag()? {
            skip_hrd_parameters(r, true, max_sub_layers_minus1)?;
        }
    }
    if r.read_flag()? {
        // bitstream_restriction
        r.read_flag()?; // tiles_fixed_structure_flag
        r.read_flag()?; // motion_vectors_over_pic_boundaries_flag
        r.read_flag()?; // restricted_ref_pic_lists_flag
        v.min_spatial_segmentation_idc = r.read_ue()?;
        r.read_ue()?; // max_bytes_per_pic_denom
        r.read_ue()?; // max_bits_per_min_cu_denom
        r.read_ue()?; // log2_max_mv_length_horizontal
        r.read_ue()?; // log2_max_mv_length_vertical
    }
    Ok(v)
}

/// Sequence parameter set (§7.3.2.2) with its derived variables (§7.4.3.2).
#[derive(Debug, Clone, Default)]
pub struct Sps {
    pub vps_id: u8,
    pub id: u8,
    pub max_sub_layers_minus1: u8,
    pub temporal_id_nesting_flag: bool,
    pub ptl: ProfileTierLevel,
    pub chroma_format_idc: u8,
    pub separate_colour_plane_flag: bool,
    pub width: u32,
    pub height: u32,
    /// `conf_win_*_offset` in units of `SubWidthC`/`SubHeightC` (left, right, top, bottom).
    pub conf_win: [u32; 4],
    pub bit_depth_luma: u8,
    pub bit_depth_chroma: u8,
    pub log2_max_poc_lsb: u8,
    pub ordering: [SubLayerOrdering; 8],
    pub log2_min_cb_size: u8,
    pub log2_ctb_size: u8,
    pub log2_min_tb_size: u8,
    pub log2_max_tb_size: u8,
    pub max_transform_hierarchy_depth_inter: u8,
    pub max_transform_hierarchy_depth_intra: u8,
    pub scaling_list_enabled: bool,
    /// The SPS scaling list when `scaling_list_enabled` (explicit or default).
    pub scaling_list: Option<ScalingList>,
    pub amp_enabled: bool,
    pub sao_enabled: bool,
    pub pcm_enabled: bool,
    pub pcm_bit_depth_luma: u8,
    pub pcm_bit_depth_chroma: u8,
    pub log2_min_pcm_cb_size: u8,
    pub log2_max_pcm_cb_size: u8,
    pub pcm_loop_filter_disabled: bool,
    pub st_rps: Vec<ShortTermRps>,
    pub long_term_ref_pics_present: bool,
    /// `(lt_ref_pic_poc_lsb_sps, used_by_curr_pic_lt_sps_flag)`.
    pub lt_ref_pics: Vec<(u32, bool)>,
    pub temporal_mvp_enabled: bool,
    pub strong_intra_smoothing_enabled: bool,
    pub vui: Option<Vui>,
    // extension presence — refused by name when set (v1 scope)
    pub range_extension: bool,
    pub multilayer_extension: bool,
    pub ext_3d: bool,
    pub scc_extension: bool,

    // ---- derived (§7.4.3.2.1) ----
    /// `ChromaArrayType`.
    pub chroma_array_type: u8,
    pub sub_width_c: u8,
    pub sub_height_c: u8,
    pub ctb_size: u32,
    pub pic_width_in_ctbs: u32,
    pub pic_height_in_ctbs: u32,
    pub pic_size_in_ctbs: u32,
    pub min_cb_size: u32,
    pub pic_width_in_min_cbs: u32,
    pub pic_height_in_min_cbs: u32,
    /// `QpBdOffsetY` / `QpBdOffsetC`.
    pub qp_bd_offset_y: i32,
    pub qp_bd_offset_c: i32,
}

impl Sps {
    /// Cropped output width / height in luma samples.
    pub fn output_size(&self) -> (u32, u32) {
        let sw = self.sub_width_c as u32;
        let sh = self.sub_height_c as u32;
        let w = self.width.saturating_sub(sw * (self.conf_win[0] + self.conf_win[1]));
        let h = self.height.saturating_sub(sh * (self.conf_win[2] + self.conf_win[3]));
        (w, h)
    }
}

/// Parses an SPS RBSP.
pub fn parse_sps(rbsp: &[u8]) -> Result<Sps> {
    let mut r = BitReader::new(rbsp);
    let mut s = Sps {
        vps_id: r.read_bits(4)? as u8,
        max_sub_layers_minus1: r.read_bits(3)? as u8,
        ..Default::default()
    };
    if s.max_sub_layers_minus1 > 6 {
        return Err(Error::invalid("sps_max_sub_layers_minus1 > 6"));
    }
    s.temporal_id_nesting_flag = r.read_flag()?;
    s.ptl = parse_ptl(&mut r, true, s.max_sub_layers_minus1 as u32)?;
    s.id = r.read_ue_max(15)? as u8;
    s.chroma_format_idc = r.read_ue_max(3)? as u8;
    if s.chroma_format_idc == 3 {
        s.separate_colour_plane_flag = r.read_flag()?;
    }
    s.width = r.read_ue_max(16888)?;
    s.height = r.read_ue_max(16888)?;
    if s.width == 0 || s.height == 0 {
        return Err(Error::invalid("zero picture size"));
    }
    if r.read_flag()? {
        for o in s.conf_win.iter_mut() {
            *o = r.read_ue_max(16888)?;
        }
    }
    s.bit_depth_luma = r.read_ue_max(8)? as u8 + 8;
    s.bit_depth_chroma = r.read_ue_max(8)? as u8 + 8;
    s.log2_max_poc_lsb = r.read_ue_max(12)? as u8 + 4;
    s.ordering = parse_sub_layer_ordering(&mut r, s.max_sub_layers_minus1 as u32)?;
    s.log2_min_cb_size = r.read_ue_max(3)? as u8 + 3;
    s.log2_ctb_size = s.log2_min_cb_size + r.read_ue_max(3)? as u8;
    if s.log2_ctb_size > 6 || s.log2_ctb_size < 4 {
        return Err(Error::invalid("CtbLog2SizeY outside 4..=6"));
    }
    s.log2_min_tb_size = r.read_ue_max(3)? as u8 + 2;
    s.log2_max_tb_size = s.log2_min_tb_size + r.read_ue_max(3)? as u8;
    if s.log2_max_tb_size > 5 || s.log2_max_tb_size > s.log2_ctb_size || s.log2_min_tb_size >= s.log2_min_cb_size {
        return Err(Error::invalid("transform block size constraints"));
    }
    s.max_transform_hierarchy_depth_inter = r.read_ue_max(4)? as u8;
    s.max_transform_hierarchy_depth_intra = r.read_ue_max(4)? as u8;
    s.scaling_list_enabled = r.read_flag()?;
    if s.scaling_list_enabled {
        s.scaling_list = Some(if r.read_flag()? { parse_scaling_list_data(&mut r)? } else { ScalingList::default() });
    }
    s.amp_enabled = r.read_flag()?;
    s.sao_enabled = r.read_flag()?;
    s.pcm_enabled = r.read_flag()?;
    if s.pcm_enabled {
        s.pcm_bit_depth_luma = r.read_bits(4)? as u8 + 1;
        s.pcm_bit_depth_chroma = r.read_bits(4)? as u8 + 1;
        s.log2_min_pcm_cb_size = r.read_ue_max(3)? as u8 + 3;
        s.log2_max_pcm_cb_size = s.log2_min_pcm_cb_size + r.read_ue_max(3)? as u8;
        s.pcm_loop_filter_disabled = r.read_flag()?;
        if s.pcm_bit_depth_luma > s.bit_depth_luma || s.pcm_bit_depth_chroma > s.bit_depth_chroma || s.log2_max_pcm_cb_size > 5.min(s.log2_ctb_size) {
            return Err(Error::invalid("pcm parameters"));
        }
    }
    let num_st_rps = r.read_ue_max(64)? as usize;
    for i in 0..num_st_rps {
        let set = parse_st_ref_pic_set(&mut r, i, &s.st_rps, false)?;
        s.st_rps.push(set);
    }
    s.long_term_ref_pics_present = r.read_flag()?;
    if s.long_term_ref_pics_present {
        let n = r.read_ue_max(32)?;
        for _ in 0..n {
            let lsb = r.read_bits(s.log2_max_poc_lsb as u32)?;
            let used = r.read_flag()?;
            s.lt_ref_pics.push((lsb, used));
        }
    }
    s.temporal_mvp_enabled = r.read_flag()?;
    s.strong_intra_smoothing_enabled = r.read_flag()?;
    if r.read_flag()? {
        s.vui = Some(parse_vui(&mut r, s.max_sub_layers_minus1 as u32)?);
    }
    if r.read_flag()? {
        s.range_extension = r.read_flag()?;
        s.multilayer_extension = r.read_flag()?;
        s.ext_3d = r.read_flag()?;
        s.scc_extension = r.read_flag()?;
        r.read_bits(4)?; // sps_extension_4bits
    }

    // derived
    s.chroma_array_type = if s.separate_colour_plane_flag { 0 } else { s.chroma_format_idc };
    (s.sub_width_c, s.sub_height_c) = match s.chroma_format_idc {
        1 => (2, 2),
        2 => (2, 1),
        _ => (1, 1),
    };
    s.ctb_size = 1 << s.log2_ctb_size;
    s.pic_width_in_ctbs = s.width.div_ceil(s.ctb_size);
    s.pic_height_in_ctbs = s.height.div_ceil(s.ctb_size);
    s.pic_size_in_ctbs = s.pic_width_in_ctbs * s.pic_height_in_ctbs;
    s.min_cb_size = 1 << s.log2_min_cb_size;
    if s.width % s.min_cb_size != 0 || s.height % s.min_cb_size != 0 {
        return Err(Error::invalid("picture size not a multiple of MinCbSizeY"));
    }
    s.pic_width_in_min_cbs = s.width / s.min_cb_size;
    s.pic_height_in_min_cbs = s.height / s.min_cb_size;
    s.qp_bd_offset_y = 6 * (s.bit_depth_luma as i32 - 8);
    s.qp_bd_offset_c = 6 * (s.bit_depth_chroma as i32 - 8);
    let sw = s.sub_width_c as u32;
    let sh = s.sub_height_c as u32;
    if sw * (s.conf_win[0] + s.conf_win[1]) >= s.width || sh * (s.conf_win[2] + s.conf_win[3]) >= s.height {
        return Err(Error::invalid("conformance window larger than the picture"));
    }
    Ok(s)
}

/// Picture parameter set (§7.3.2.3) with the tile geometry derived (§6.5.1).
#[derive(Debug, Clone, Default)]
pub struct Pps {
    pub id: u8,
    pub sps_id: u8,
    pub dependent_slice_segments_enabled: bool,
    pub output_flag_present: bool,
    pub num_extra_slice_header_bits: u8,
    pub sign_data_hiding_enabled: bool,
    pub cabac_init_present: bool,
    pub num_ref_idx_l0_default_active: u8,
    pub num_ref_idx_l1_default_active: u8,
    pub init_qp: i32,
    pub constrained_intra_pred: bool,
    pub transform_skip_enabled: bool,
    pub cu_qp_delta_enabled: bool,
    pub diff_cu_qp_delta_depth: u8,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub slice_chroma_qp_offsets_present: bool,
    pub weighted_pred: bool,
    pub weighted_bipred: bool,
    pub transquant_bypass_enabled: bool,
    pub tiles_enabled: bool,
    pub entropy_coding_sync_enabled: bool,
    pub num_tile_columns: u32,
    pub num_tile_rows: u32,
    pub uniform_spacing: bool,
    /// Explicit `column_width_minus1 + 1` / `row_height_minus1 + 1` (all but the last).
    pub column_widths: Vec<u32>,
    pub row_heights: Vec<u32>,
    pub loop_filter_across_tiles_enabled: bool,
    pub loop_filter_across_slices_enabled: bool,
    pub deblocking_filter_control_present: bool,
    pub deblocking_filter_override_enabled: bool,
    pub deblocking_filter_disabled: bool,
    pub beta_offset_div2: i32,
    pub tc_offset_div2: i32,
    pub scaling_list: Option<ScalingList>,
    pub lists_modification_present: bool,
    pub log2_parallel_merge_level: u8,
    pub slice_segment_header_extension_present: bool,
    pub range_extension: bool,
    pub multilayer_extension: bool,
    pub ext_3d: bool,
    pub scc_extension: bool,
    // range extension fields we accept in v1 syntax form (values must be v1-neutral)
    pub log2_max_transform_skip_block_size: u8,
    pub cross_component_prediction_enabled: bool,
    pub chroma_qp_offset_list_enabled: bool,
}

impl Pps {
    /// Bytes past which `pps_range_extension()` etc. begin are consumed by
    /// `parse_pps`; nothing more to do here.
    pub fn is_v1_only(&self) -> bool {
        !(self.multilayer_extension || self.ext_3d || self.scc_extension)
    }
}

/// Parses a PPS RBSP. The tile geometry needs the SPS and is derived by
/// [`TileLayout::new`] at activation time.
pub fn parse_pps(rbsp: &[u8]) -> Result<Pps> {
    let mut r = BitReader::new(rbsp);
    let mut p = Pps {
        id: r.read_ue_max(63)? as u8,
        sps_id: r.read_ue_max(15)? as u8,
        ..Default::default()
    };
    p.dependent_slice_segments_enabled = r.read_flag()?;
    p.output_flag_present = r.read_flag()?;
    p.num_extra_slice_header_bits = r.read_bits(3)? as u8;
    p.sign_data_hiding_enabled = r.read_flag()?;
    p.cabac_init_present = r.read_flag()?;
    p.num_ref_idx_l0_default_active = r.read_ue_max(14)? as u8 + 1;
    p.num_ref_idx_l1_default_active = r.read_ue_max(14)? as u8 + 1;
    p.init_qp = 26 + r.read_se()?;
    if !(-(6 * 8)..=51).contains(&p.init_qp) {
        return Err(Error::invalid("init_qp_minus26"));
    }
    p.constrained_intra_pred = r.read_flag()?;
    p.transform_skip_enabled = r.read_flag()?;
    p.cu_qp_delta_enabled = r.read_flag()?;
    if p.cu_qp_delta_enabled {
        p.diff_cu_qp_delta_depth = r.read_ue_max(3)? as u8;
    }
    p.cb_qp_offset = r.read_se()?;
    p.cr_qp_offset = r.read_se()?;
    if !(-12..=12).contains(&p.cb_qp_offset) || !(-12..=12).contains(&p.cr_qp_offset) {
        return Err(Error::invalid("pps_cb/cr_qp_offset"));
    }
    p.slice_chroma_qp_offsets_present = r.read_flag()?;
    p.weighted_pred = r.read_flag()?;
    p.weighted_bipred = r.read_flag()?;
    p.transquant_bypass_enabled = r.read_flag()?;
    p.tiles_enabled = r.read_flag()?;
    p.entropy_coding_sync_enabled = r.read_flag()?;
    p.num_tile_columns = 1;
    p.num_tile_rows = 1;
    p.loop_filter_across_tiles_enabled = true;
    if p.tiles_enabled {
        p.num_tile_columns = r.read_ue_max(19)? + 1;
        p.num_tile_rows = r.read_ue_max(21)? + 1;
        p.uniform_spacing = r.read_flag()?;
        if !p.uniform_spacing {
            for _ in 0..p.num_tile_columns - 1 {
                p.column_widths.push(r.read_ue_max(1023)? + 1);
            }
            for _ in 0..p.num_tile_rows - 1 {
                p.row_heights.push(r.read_ue_max(1023)? + 1);
            }
        }
        p.loop_filter_across_tiles_enabled = r.read_flag()?;
    }
    p.loop_filter_across_slices_enabled = r.read_flag()?;
    p.deblocking_filter_control_present = r.read_flag()?;
    if p.deblocking_filter_control_present {
        p.deblocking_filter_override_enabled = r.read_flag()?;
        p.deblocking_filter_disabled = r.read_flag()?;
        if !p.deblocking_filter_disabled {
            p.beta_offset_div2 = r.read_se()?;
            p.tc_offset_div2 = r.read_se()?;
            if !(-6..=6).contains(&p.beta_offset_div2) || !(-6..=6).contains(&p.tc_offset_div2) {
                return Err(Error::invalid("pps beta/tc offset"));
            }
        }
    }
    if r.read_flag()? {
        p.scaling_list = Some(parse_scaling_list_data(&mut r)?);
    }
    p.lists_modification_present = r.read_flag()?;
    p.log2_parallel_merge_level = r.read_ue_max(4)? as u8 + 2;
    p.slice_segment_header_extension_present = r.read_flag()?;
    p.log2_max_transform_skip_block_size = 2;
    if r.read_flag()? {
        p.range_extension = r.read_flag()?;
        p.multilayer_extension = r.read_flag()?;
        p.ext_3d = r.read_flag()?;
        p.scc_extension = r.read_flag()?;
        r.read_bits(4)?;
        if p.range_extension {
            if p.transform_skip_enabled {
                p.log2_max_transform_skip_block_size = r.read_ue_max(3)? as u8 + 2;
            }
            p.cross_component_prediction_enabled = r.read_flag()?;
            p.chroma_qp_offset_list_enabled = r.read_flag()?;
            // The remaining RExt fields are only reached by RExt streams,
            // which are refused at activation (v1 scope).
        }
    }
    Ok(p)
}

/// Tile geometry (§6.5.1) plus the CTB raster ↔ tile-scan tables.
#[derive(Debug, Clone, Default)]
pub struct TileLayout {
    /// `colBd[0..=num_tile_columns]` in CTBs.
    pub col_bd: Vec<u32>,
    /// `rowBd[0..=num_tile_rows]` in CTBs.
    pub row_bd: Vec<u32>,
    /// `CtbAddrRsToTs`.
    pub rs_to_ts: Vec<u32>,
    /// `CtbAddrTsToRs`.
    pub ts_to_rs: Vec<u32>,
    /// `TileId[ctbAddrTs]`.
    pub tile_id: Vec<u32>,
}

impl TileLayout {
    pub fn new(sps: &Sps, pps: &Pps) -> Result<TileLayout> {
        let cols = pps.num_tile_columns;
        let rows = pps.num_tile_rows;
        let w = sps.pic_width_in_ctbs;
        let h = sps.pic_height_in_ctbs;
        if cols > w || rows > h {
            return Err(Error::invalid("more tiles than CTBs"));
        }
        let mut col_bd = vec![0u32; cols as usize + 1];
        let mut row_bd = vec![0u32; rows as usize + 1];
        if pps.uniform_spacing || !pps.tiles_enabled {
            for i in 0..cols {
                col_bd[i as usize + 1] = ((i + 1) * w) / cols;
            }
            for j in 0..rows {
                row_bd[j as usize + 1] = ((j + 1) * h) / rows;
            }
        } else {
            let mut acc = 0;
            for i in 0..cols as usize - 1 {
                acc += pps.column_widths[i];
                if acc >= w {
                    return Err(Error::invalid("tile column widths exceed the picture"));
                }
                col_bd[i + 1] = acc;
            }
            col_bd[cols as usize] = w;
            acc = 0;
            for j in 0..rows as usize - 1 {
                acc += pps.row_heights[j];
                if acc >= h {
                    return Err(Error::invalid("tile row heights exceed the picture"));
                }
                row_bd[j + 1] = acc;
            }
            row_bd[rows as usize] = h;
        }
        let n = (w * h) as usize;
        let mut rs_to_ts = vec![0u32; n];
        let mut ts_to_rs = vec![0u32; n];
        let mut tile_id = vec![0u32; n];
        // (6-5)
        for rs in 0..n as u32 {
            let tb_x = rs % w;
            let tb_y = rs / w;
            let mut tile_x = 0;
            for i in 0..cols as usize {
                if tb_x >= col_bd[i] {
                    tile_x = i;
                }
            }
            let mut tile_y = 0;
            for j in 0..rows as usize {
                if tb_y >= row_bd[j] {
                    tile_y = j;
                }
            }
            let mut ts = 0;
            for i in 0..tile_x {
                ts += (row_bd[tile_y + 1] - row_bd[tile_y]) * (col_bd[i + 1] - col_bd[i]);
            }
            for j in 0..tile_y {
                ts += w * (row_bd[j + 1] - row_bd[j]);
            }
            ts += (tb_y - row_bd[tile_y]) * (col_bd[tile_x + 1] - col_bd[tile_x]) + tb_x - col_bd[tile_x];
            rs_to_ts[rs as usize] = ts;
            ts_to_rs[ts as usize] = rs;
        }
        // (6-7)
        let mut tid = 0u32;
        for j in 0..rows as usize {
            for i in 0..cols as usize {
                for y in row_bd[j]..row_bd[j + 1] {
                    for x in col_bd[i]..col_bd[i + 1] {
                        tile_id[rs_to_ts[(y * w + x) as usize] as usize] = tid;
                    }
                }
                tid += 1;
            }
        }
        Ok(TileLayout {
            col_bd,
            row_bd,
            rs_to_ts,
            ts_to_rs,
            tile_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scaling_tables_have_64_entries_and_are_monotone_ish() {
        assert_eq!(DEFAULT_SCALING_INTRA.len(), 64);
        assert_eq!(DEFAULT_SCALING_INTER.len(), 64);
        assert_eq!(DEFAULT_SCALING_INTRA[63], 115);
        assert_eq!(DEFAULT_SCALING_INTER[63], 91);
        assert_eq!(DEFAULT_SCALING_INTRA[0], 16);
        assert_eq!(DEFAULT_SCALING_INTER[0], 16);
        assert_eq!(DEFAULT_SCALING_INTRA.iter().map(|&v| v as u32).sum::<u32>(), 1939);
        assert_eq!(DEFAULT_SCALING_INTER.iter().map(|&v| v as u32).sum::<u32>(), 1752);
    }

    #[test]
    fn st_rps_explicit_and_predicted() {
        // Explicit: num_neg=2 (deltas 1,2 used), num_pos=1 (delta 1 used)
        // ue(2)=011 ue(1)=010 | ue(0)=1 f=1 | ue(1)=010 f=1 | ue(0)=1 f=1
        let bits = "011010".to_string() + "11" + "0101" + "11";
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let s0 = parse_st_ref_pic_set(&mut r, 0, &[], false).unwrap();
        assert_eq!(s0.neg, vec![(-1, true), (-3, true)]);
        assert_eq!(s0.pos, vec![(1, true)]);

        // Predicted from s0 with deltaRps = -1: inter=1, sign=1, abs_minus1=0 (ue 1)
        // then used flags for j in 0..=3 all 1.
        let bits = "1".to_string() + "1" + "1" + "1111";
        let bytes = bits_to_bytes(&bits);
        let mut r = BitReader::new(&bytes);
        let s1 = parse_st_ref_pic_set(&mut r, 1, core::slice::from_ref(&s0), false).unwrap();
        // pos j=0: 1-1=0 -> dropped; deltaRps<0 -> -1; neg: -2, -4
        assert_eq!(s1.neg, vec![(-1, true), (-2, true), (-4, true)]);
        assert_eq!(s1.pos, Vec::<(i32, bool)>::new());
    }

    fn bits_to_bytes(bits: &str) -> Vec<u8> {
        let mut out = vec![0u8; bits.len().div_ceil(8) + 1];
        for (i, c) in bits.chars().enumerate() {
            if c == '1' {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }

    #[test]
    fn tile_scan_is_a_permutation() {
        let sps = Sps {
            pic_width_in_ctbs: 7,
            pic_height_in_ctbs: 5,
            ..Default::default()
        };
        let pps = Pps {
            tiles_enabled: true,
            num_tile_columns: 3,
            num_tile_rows: 2,
            uniform_spacing: true,
            ..Default::default()
        };
        let t = TileLayout::new(&sps, &pps).unwrap();
        assert_eq!(t.col_bd, vec![0, 2, 4, 7]);
        assert_eq!(t.row_bd, vec![0, 2, 5]);
        let mut seen = [false; 35];
        for rs in 0..35 {
            let ts = t.rs_to_ts[rs] as usize;
            assert_eq!(t.ts_to_rs[ts], rs as u32);
            assert!(!seen[ts]);
            seen[ts] = true;
        }
        assert_eq!(t.tile_id[0], 0);
        assert_eq!(t.tile_id[34], 5);
    }
}
