use super::*;

#[test]
fn pareto_table_shape_and_spot_values() {
    assert_eq!(PARETO_TABLE.len(), 128);
    // Spot-check the first and last rows of the spec PDF extraction result.
    assert_eq!(PARETO_TABLE[0], [3, 86, 128, 6, 86, 23, 88, 29]);
    assert_eq!(PARETO_TABLE[127], [255, 246, 247, 255, 239, 255, 253, 255]);
}

#[test]
fn pareto_node_below_2_returns_prob_unmodified() {
    assert_eq!(pareto(0, 200), 200);
    assert_eq!(pareto(1, 7), 7);
}

#[test]
fn pareto_odd_prob_uses_single_row() {
    // prob=1 -> x=0, odd -> PARETO_TABLE[0]
    assert_eq!(pareto(2, 1), PARETO_TABLE[0][0]);
    assert_eq!(pareto(9, 1), PARETO_TABLE[0][7]);
}

#[test]
fn pareto_even_prob_averages_adjacent_rows() {
    // prob=2 -> x=0, even -> (PARETO_TABLE[0] + PARETO_TABLE[1]) >> 1
    let expected = ((PARETO_TABLE[0][0] as u32 + PARETO_TABLE[1][0] as u32) >> 1) as u8;
    assert_eq!(pareto(2, 2), expected);
}

#[test]
fn coefband_8x8plus_matches_extracted_pattern() {
    assert_eq!(coefband_8x8plus(0), 0);
    assert_eq!(coefband_8x8plus(1), 1);
    assert_eq!(coefband_8x8plus(2), 1);
    assert_eq!(coefband_8x8plus(20), 4);
    assert_eq!(coefband_8x8plus(21), 5);
    assert_eq!(coefband_8x8plus(1023), 5);
}

#[test]
fn coefband_4x4_matches_spec() {
    assert_eq!(
        COEFBAND_4X4,
        [0, 1, 1, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 5, 5, 5]
    );
}

#[test]
fn ss_size_lookup_matches_spec_spot_checks() {
    // BLOCK_8X8 (index 3), subx=1, suby=1 (4:2:0) -> BLOCK_4X4
    assert_eq!(SS_SIZE_LOOKUP[BLOCK_8X8 as usize][1][1], BLOCK_4X4);
    // BLOCK_64X64 (index 12), subx=0, suby=0 -> BLOCK_64X64
    assert_eq!(SS_SIZE_LOOKUP[BLOCK_64X64 as usize][0][0], BLOCK_64X64);
    // BLOCK_8X16 (index 4), subx=1, suby=0 -> BLOCK_INVALID
    assert_eq!(SS_SIZE_LOOKUP[BLOCK_8X16 as usize][1][0], BLOCK_INVALID);
}

#[test]
fn extra_bits_and_cat_probs_row_lengths_agree() {
    for (token, row) in EXTRA_BITS.iter().enumerate() {
        let num_extra = row[1] as usize;
        let cat = row[0] as usize;
        // Of the corresponding cat_probs row, the first numExtra elements must be
        // non-zero (except for the token=ZERO..FOUR rows where num_extra=0).
        if num_extra > 0 {
            assert!(
                CAT_PROBS[cat][..num_extra].iter().all(|&p| p != 0),
                "token={token}: cat_probs[{cat}][..{num_extra}] has an unexpected 0"
            );
        }
    }
}

#[test]
fn mode2txfm_map_matches_spec_table() {
    use crate::transform::TxType;
    assert_eq!(mode2txfm_map(DC_PRED), TxType::DctDct);
    assert_eq!(mode2txfm_map(V_PRED), TxType::AdstDct);
    assert_eq!(mode2txfm_map(H_PRED), TxType::DctAdst);
    assert_eq!(mode2txfm_map(D45_PRED), TxType::DctDct);
    assert_eq!(mode2txfm_map(D135_PRED), TxType::AdstAdst);
    assert_eq!(mode2txfm_map(D117_PRED), TxType::AdstDct);
    assert_eq!(mode2txfm_map(D153_PRED), TxType::DctAdst);
    assert_eq!(mode2txfm_map(D207_PRED), TxType::DctAdst);
    assert_eq!(mode2txfm_map(D63_PRED), TxType::AdstDct);
    assert_eq!(mode2txfm_map(TM_PRED), TxType::AdstAdst);
}
