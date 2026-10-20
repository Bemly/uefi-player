use super::{merge_snapshot, reset, snapshot, Stage, STAGE_COUNT};

#[test]
fn merge_snapshot_adds_worker_counters() {
    reset();
    let mut first = [0; STAGE_COUNT];
    first[Stage::InterPredict as usize] = 17;
    first[Stage::TokenDequantTransform as usize] = 23;
    let mut second = [0; STAGE_COUNT];
    second[Stage::InterPredict as usize] = 5;

    merge_snapshot(&first);
    merge_snapshot(&second);

    let merged = snapshot();
    assert_eq!(merged[Stage::InterPredict as usize], 22);
    assert_eq!(merged[Stage::TokenDequantTransform as usize], 23);
    assert_eq!(merged[Stage::Total as usize], 0);
}
