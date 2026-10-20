use super::*;

/// Confirms that every scan table contains each index up to its length
/// exactly once (i.e. it is a permutation of 0..N-1).
fn assert_is_permutation(name: &str, table: &[u16]) {
    let mut seen = vec![false; table.len()];
    for &v in table {
        let idx = v as usize;
        assert!(idx < table.len(), "{name}: index {v} out of range");
        assert!(!seen[idx], "{name}: index {v} duplicated");
        seen[idx] = true;
    }
    assert!(seen.iter().all(|&b| b), "{name}: some indices are missing");
}

#[test]
fn all_scan_tables_are_permutations() {
    assert_is_permutation("default_scan_4x4", &DEFAULT_SCAN_4X4);
    assert_is_permutation("col_scan_4x4", &COL_SCAN_4X4);
    assert_is_permutation("row_scan_4x4", &ROW_SCAN_4X4);
    assert_is_permutation("default_scan_8x8", &DEFAULT_SCAN_8X8);
    assert_is_permutation("col_scan_8x8", &COL_SCAN_8X8);
    assert_is_permutation("row_scan_8x8", &ROW_SCAN_8X8);
    assert_is_permutation("default_scan_16x16", &DEFAULT_SCAN_16X16);
    assert_is_permutation("col_scan_16x16", &COL_SCAN_16X16);
    assert_is_permutation("row_scan_16x16", &ROW_SCAN_16X16);
    assert_is_permutation("default_scan_32x32", &DEFAULT_SCAN_32X32);
}

/// Each scan order always starts at index 0 (the DC coefficient position).
#[test]
fn all_scan_tables_start_at_dc() {
    assert_eq!(DEFAULT_SCAN_4X4[0], 0);
    assert_eq!(COL_SCAN_4X4[0], 0);
    assert_eq!(ROW_SCAN_4X4[0], 0);
    assert_eq!(DEFAULT_SCAN_8X8[0], 0);
    assert_eq!(COL_SCAN_8X8[0], 0);
    assert_eq!(ROW_SCAN_8X8[0], 0);
    assert_eq!(DEFAULT_SCAN_16X16[0], 0);
    assert_eq!(COL_SCAN_16X16[0], 0);
    assert_eq!(ROW_SCAN_16X16[0], 0);
    assert_eq!(DEFAULT_SCAN_32X32[0], 0);
}

#[test]
fn get_scan_selects_expected_table() {
    assert_eq!(get_scan(TxSize::Tx4x4, TxType::DctDct), &DEFAULT_SCAN_4X4);
    assert_eq!(get_scan(TxSize::Tx4x4, TxType::AdstAdst), &DEFAULT_SCAN_4X4);
    assert_eq!(get_scan(TxSize::Tx4x4, TxType::AdstDct), &ROW_SCAN_4X4);
    assert_eq!(get_scan(TxSize::Tx4x4, TxType::DctAdst), &COL_SCAN_4X4);

    assert_eq!(get_scan(TxSize::Tx8x8, TxType::AdstDct), &ROW_SCAN_8X8);
    assert_eq!(get_scan(TxSize::Tx8x8, TxType::DctAdst), &COL_SCAN_8X8);

    assert_eq!(get_scan(TxSize::Tx16x16, TxType::AdstDct), &ROW_SCAN_16X16);
    assert_eq!(get_scan(TxSize::Tx16x16, TxType::DctAdst), &COL_SCAN_16X16);

    // TX_32X32 always uses default_scan_32x32.
    assert_eq!(
        get_scan(TxSize::Tx32x32, TxType::DctDct),
        &DEFAULT_SCAN_32X32
    );
    assert_eq!(
        get_scan(TxSize::Tx32x32, TxType::AdstAdst),
        &DEFAULT_SCAN_32X32
    );
}
