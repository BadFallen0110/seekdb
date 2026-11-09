use minivectordb::vector::select_topk;

#[test]
fn topk_keeps_smallest_distances_with_primary_key_ties() {
    let ranked = select_topk(vec![(1.0, 9), (0.1, 7), (0.1, 3), (0.2, 5), (0.3, 1)], 3);
    assert_eq!(ranked, vec![(0.1, 3), (0.1, 7), (0.2, 5)]);
}
