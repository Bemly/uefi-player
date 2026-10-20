use super::*;

#[test]
fn add_assign_sums_every_counter_field() {
    let mut a = Counts::new();
    let mut b = Counts::new();

    b.intra_mode[0][0] = 1;
    b.uv_mode[0][0] = 2;
    b.partition[0][0] = 3;
    b.interp_filter[0][0] = 4;
    b.inter_mode[0][0] = 5;
    b.tx_size[0][0][0] = 6;
    b.is_inter[0][0] = 7;
    b.comp_mode[0][0] = 8;
    b.single_ref[0][0][0] = 9;
    b.comp_ref[0][0] = 10;
    b.skip[0][0] = 11;
    b.mv_joint[0] = 12;
    b.mv_sign[0][0] = 13;
    b.mv_class[0][0] = 14;
    b.mv_class0_bit[0][0] = 15;
    b.mv_class0_fr[0][0][0] = 16;
    b.mv_class0_hp[0][0] = 17;
    b.mv_bits[0][0][0] = 18;
    b.mv_fr[0][0] = 19;
    b.mv_hp[0][0] = 20;
    b.token[0][0][0][0][0][0] = 21;
    b.more_coefs[0][0][0][0][0][0] = 22;
    b.token[3][1][1][5][5][2] = 23;
    b.more_coefs[3][1][1][5][5][1] = 24;

    a.add_assign(&b);

    assert_eq!(a.intra_mode[0][0], 1);
    assert_eq!(a.uv_mode[0][0], 2);
    assert_eq!(a.partition[0][0], 3);
    assert_eq!(a.interp_filter[0][0], 4);
    assert_eq!(a.inter_mode[0][0], 5);
    assert_eq!(a.tx_size[0][0][0], 6);
    assert_eq!(a.is_inter[0][0], 7);
    assert_eq!(a.comp_mode[0][0], 8);
    assert_eq!(a.single_ref[0][0][0], 9);
    assert_eq!(a.comp_ref[0][0], 10);
    assert_eq!(a.skip[0][0], 11);
    assert_eq!(a.mv_joint[0], 12);
    assert_eq!(a.mv_sign[0][0], 13);
    assert_eq!(a.mv_class[0][0], 14);
    assert_eq!(a.mv_class0_bit[0][0], 15);
    assert_eq!(a.mv_class0_fr[0][0][0], 16);
    assert_eq!(a.mv_class0_hp[0][0], 17);
    assert_eq!(a.mv_bits[0][0][0], 18);
    assert_eq!(a.mv_fr[0][0], 19);
    assert_eq!(a.mv_hp[0][0], 20);
    assert_eq!(a.token[0][0][0][0][0][0], 21);
    assert_eq!(a.more_coefs[0][0][0][0][0][0], 22);
    assert_eq!(a.token[3][1][1][5][5][2], 23);
    assert_eq!(a.more_coefs[3][1][1][5][5][1], 24);
}

#[test]
fn add_assign_preserves_wrapping_overflow() {
    let mut a = Counts::new();
    let mut b = Counts::new();
    a.skip[0][0] = u32::MAX;
    b.skip[0][0] = 1;

    a.add_assign(&b);

    assert_eq!(a.skip[0][0], 0);
}

#[test]
fn merge_prob_with_no_observations_keeps_preprob_untouched_direction() {
    // When den == 0, prob=128 and count=0, so factor=0, and out=preProb is returned as-is.
    assert_eq!(merge_prob(200, 0, 0, 20, 128), 200);
}

#[test]
fn merge_prob_saturates_toward_observed_ratio_with_enough_counts() {
    // ct0=0, ct1=100 (reaches count_sat=20) -> when factor=maxUpdateFactor=128,
    // outProb moves away from preProb toward 1, roughly Round2( preProb*128 + 1*128, 8 ).
    let out = merge_prob(200, 0, 100, 20, 128);
    assert!(out < 200);
}

#[test]
fn merge_probs_binary_tree_updates_single_prob() {
    let mut probs = [100u8, 0, 0];
    let counts = [10u32, 30u32];
    merge_probs(&BINARY_TREE, 0, &mut probs, &counts, 20, 128);
    // Since ct1 (=30) is larger, prob moves in the decreasing direction.
    assert!(probs[0] < 100);
}
