//! Pure neighbor-context derivation for reference-frame-related syntax elements (spec §9.3.2):
//! `comp_mode`/`comp_ref`/`single_ref_p1`/`single_ref_p2` contexts. Called from
//! `tile::mode_info::read_ref_frames`.

use super::mode_info::NeighborRefInfo;
use super::TileDecoder;
use crate::prob_tables::{ALTREF_FRAME, GOLDEN_FRAME, LAST_FRAME};

impl TileDecoder {
    /// Context derivation for `comp_mode` (spec §9.3.2).
    pub(super) fn comp_mode_ctx(&self, n: &NeighborRefInfo) -> usize {
        let fixed = self.comp_fixed_ref;
        if n.avail_u && n.avail_l {
            if n.above_single && n.left_single {
                ((n.above_ref_frame[0] == fixed) ^ (n.left_ref_frame[0] == fixed)) as usize
            } else if n.above_single {
                2 + (n.above_ref_frame[0] == fixed || n.above_intra) as usize
            } else if n.left_single {
                2 + (n.left_ref_frame[0] == fixed || n.left_intra) as usize
            } else {
                4
            }
        } else if n.avail_u {
            if n.above_single {
                (n.above_ref_frame[0] == fixed) as usize
            } else {
                3
            }
        } else if n.avail_l {
            if n.left_single {
                (n.left_ref_frame[0] == fixed) as usize
            } else {
                3
            }
        } else {
            1
        }
    }

    /// Context derivation for `comp_ref` (spec §9.3.2).
    pub(super) fn comp_ref_ctx(&self, n: &NeighborRefInfo) -> usize {
        let fix_ref_idx = self.ref_frame_sign_bias[self.comp_fixed_ref as usize] as usize;
        let var_ref_idx = 1 - fix_ref_idx;
        let comp_var_ref = self.comp_var_ref;

        if n.avail_u && n.avail_l {
            if n.above_intra && n.left_intra {
                2
            } else if n.left_intra {
                if n.above_single {
                    1 + 2 * (n.above_ref_frame[0] != comp_var_ref[1]) as usize
                } else {
                    1 + 2 * (n.above_ref_frame[var_ref_idx] != comp_var_ref[1]) as usize
                }
            } else if n.above_intra {
                if n.left_single {
                    1 + 2 * (n.left_ref_frame[0] != comp_var_ref[1]) as usize
                } else {
                    1 + 2 * (n.left_ref_frame[var_ref_idx] != comp_var_ref[1]) as usize
                }
            } else {
                let vrfa = if n.above_single {
                    n.above_ref_frame[0]
                } else {
                    n.above_ref_frame[var_ref_idx]
                };
                let vrfl = if n.left_single {
                    n.left_ref_frame[0]
                } else {
                    n.left_ref_frame[var_ref_idx]
                };
                if vrfa == vrfl && comp_var_ref[1] == vrfa {
                    0
                } else if n.left_single && n.above_single {
                    if (vrfa == self.comp_fixed_ref && vrfl == comp_var_ref[0])
                        || (vrfl == self.comp_fixed_ref && vrfa == comp_var_ref[0])
                    {
                        4
                    } else if vrfa == vrfl {
                        3
                    } else {
                        1
                    }
                } else if n.left_single || n.above_single {
                    let vrfc = if n.left_single { vrfa } else { vrfl };
                    let rfs = if n.above_single { vrfa } else { vrfl };
                    if vrfc == comp_var_ref[1] && rfs != comp_var_ref[1] {
                        1
                    } else if rfs == comp_var_ref[1] && vrfc != comp_var_ref[1] {
                        2
                    } else {
                        4
                    }
                } else if vrfa == vrfl {
                    4
                } else {
                    2
                }
            }
        } else if n.avail_u {
            if n.above_intra {
                2
            } else if n.above_single {
                3 * (n.above_ref_frame[0] != comp_var_ref[1]) as usize
            } else {
                4 * (n.above_ref_frame[var_ref_idx] != comp_var_ref[1]) as usize
            }
        } else if n.avail_l {
            if n.left_intra {
                2
            } else if n.left_single {
                3 * (n.left_ref_frame[0] != comp_var_ref[1]) as usize
            } else {
                4 * (n.left_ref_frame[var_ref_idx] != comp_var_ref[1]) as usize
            }
        } else {
            2
        }
    }

    /// Context derivation for `single_ref_p1` (spec §9.3.2).
    pub(super) fn single_ref_p1_ctx(&self, n: &NeighborRefInfo) -> usize {
        if n.avail_u && n.avail_l {
            if n.above_intra && n.left_intra {
                2
            } else if n.left_intra {
                if n.above_single {
                    4 * (n.above_ref_frame[0] == LAST_FRAME) as usize
                } else {
                    1 + (n.above_ref_frame[0] == LAST_FRAME || n.above_ref_frame[1] == LAST_FRAME)
                        as usize
                }
            } else if n.above_intra {
                if n.left_single {
                    4 * (n.left_ref_frame[0] == LAST_FRAME) as usize
                } else {
                    1 + (n.left_ref_frame[0] == LAST_FRAME || n.left_ref_frame[1] == LAST_FRAME)
                        as usize
                }
            } else if n.above_single && n.left_single {
                2 * (n.above_ref_frame[0] == LAST_FRAME) as usize
                    + 2 * (n.left_ref_frame[0] == LAST_FRAME) as usize
            } else if !n.above_single && !n.left_single {
                1 + (n.above_ref_frame[0] == LAST_FRAME
                    || n.above_ref_frame[1] == LAST_FRAME
                    || n.left_ref_frame[0] == LAST_FRAME
                    || n.left_ref_frame[1] == LAST_FRAME) as usize
            } else {
                let (rfs, crf1, crf2) = if n.above_single {
                    (
                        n.above_ref_frame[0],
                        n.left_ref_frame[0],
                        n.left_ref_frame[1],
                    )
                } else {
                    (
                        n.left_ref_frame[0],
                        n.above_ref_frame[0],
                        n.above_ref_frame[1],
                    )
                };
                if rfs == LAST_FRAME {
                    3 + (crf1 == LAST_FRAME || crf2 == LAST_FRAME) as usize
                } else {
                    (crf1 == LAST_FRAME || crf2 == LAST_FRAME) as usize
                }
            }
        } else if n.avail_u {
            if n.above_intra {
                2
            } else if n.above_single {
                4 * (n.above_ref_frame[0] == LAST_FRAME) as usize
            } else {
                1 + (n.above_ref_frame[0] == LAST_FRAME || n.above_ref_frame[1] == LAST_FRAME)
                    as usize
            }
        } else if n.avail_l {
            if n.left_intra {
                2
            } else if n.left_single {
                4 * (n.left_ref_frame[0] == LAST_FRAME) as usize
            } else {
                1 + (n.left_ref_frame[0] == LAST_FRAME || n.left_ref_frame[1] == LAST_FRAME)
                    as usize
            }
        } else {
            2
        }
    }

    /// Context derivation for `single_ref_p2` (spec §9.3.2).
    pub(super) fn single_ref_p2_ctx(&self, n: &NeighborRefInfo) -> usize {
        if n.avail_u && n.avail_l {
            if n.above_intra && n.left_intra {
                2
            } else if n.left_intra {
                if n.above_single {
                    if n.above_ref_frame[0] == LAST_FRAME {
                        3
                    } else {
                        4 * (n.above_ref_frame[0] == GOLDEN_FRAME) as usize
                    }
                } else {
                    1 + 2
                        * (n.above_ref_frame[0] == GOLDEN_FRAME
                            || n.above_ref_frame[1] == GOLDEN_FRAME)
                            as usize
                }
            } else if n.above_intra {
                if n.left_single {
                    if n.left_ref_frame[0] == LAST_FRAME {
                        3
                    } else {
                        4 * (n.left_ref_frame[0] == GOLDEN_FRAME) as usize
                    }
                } else {
                    1 + 2
                        * (n.left_ref_frame[0] == GOLDEN_FRAME
                            || n.left_ref_frame[1] == GOLDEN_FRAME)
                            as usize
                }
            } else if n.above_single && n.left_single {
                if n.above_ref_frame[0] == LAST_FRAME && n.left_ref_frame[0] == LAST_FRAME {
                    3
                } else if n.above_ref_frame[0] == LAST_FRAME {
                    4 * (n.left_ref_frame[0] == GOLDEN_FRAME) as usize
                } else if n.left_ref_frame[0] == LAST_FRAME {
                    4 * (n.above_ref_frame[0] == GOLDEN_FRAME) as usize
                } else {
                    2 * (n.above_ref_frame[0] == GOLDEN_FRAME) as usize
                        + 2 * (n.left_ref_frame[0] == GOLDEN_FRAME) as usize
                }
            } else if !n.above_single && !n.left_single {
                if n.above_ref_frame[0] == n.left_ref_frame[0]
                    && n.above_ref_frame[1] == n.left_ref_frame[1]
                {
                    3 * (n.above_ref_frame[0] == GOLDEN_FRAME
                        || n.above_ref_frame[1] == GOLDEN_FRAME) as usize
                } else {
                    2
                }
            } else {
                let (rfs, crf1, crf2) = if n.above_single {
                    (
                        n.above_ref_frame[0],
                        n.left_ref_frame[0],
                        n.left_ref_frame[1],
                    )
                } else {
                    (
                        n.left_ref_frame[0],
                        n.above_ref_frame[0],
                        n.above_ref_frame[1],
                    )
                };
                if rfs == GOLDEN_FRAME {
                    3 + (crf1 == GOLDEN_FRAME || crf2 == GOLDEN_FRAME) as usize
                } else if rfs == ALTREF_FRAME {
                    (crf1 == GOLDEN_FRAME || crf2 == GOLDEN_FRAME) as usize
                } else {
                    1 + 2 * (crf1 == GOLDEN_FRAME || crf2 == GOLDEN_FRAME) as usize
                }
            }
        } else if n.avail_u {
            if n.above_intra || (n.above_ref_frame[0] == LAST_FRAME && n.above_single) {
                2
            } else if n.above_single {
                4 * (n.above_ref_frame[0] == GOLDEN_FRAME) as usize
            } else {
                3 * (n.above_ref_frame[0] == GOLDEN_FRAME || n.above_ref_frame[1] == GOLDEN_FRAME)
                    as usize
            }
        } else if n.avail_l {
            if n.left_intra || (n.left_ref_frame[0] == LAST_FRAME && n.left_single) {
                2
            } else if n.left_single {
                4 * (n.left_ref_frame[0] == GOLDEN_FRAME) as usize
            } else {
                3 * (n.left_ref_frame[0] == GOLDEN_FRAME || n.left_ref_frame[1] == GOLDEN_FRAME)
                    as usize
            }
        } else {
            2
        }
    }
}
