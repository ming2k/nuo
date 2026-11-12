//! Dialog navigation unit tests.

use super::*;

#[test]
fn rotate_selectable_skips_unusable_items() {
    // [0: true, 1: false, 2: true]
    let is_selectable = |i: usize| i != 1;
    // Forward from 0 -> skips 1, lands on 2
    assert_eq!(rotate_selectable(0, 3, true, is_selectable), 2);
    // Forward from 2 -> wraps, lands on 0
    assert_eq!(rotate_selectable(2, 3, true, is_selectable), 0);
    // Backward from 0 -> wraps, skips 1, lands on 2
    assert_eq!(rotate_selectable(0, 3, false, is_selectable), 2);
    // Backward from 2 -> skips 1, lands on 0
    assert_eq!(rotate_selectable(2, 3, false, is_selectable), 0);
}

#[test]
fn rotate_selectable_handles_all_unusable_gracefully() {
    assert_eq!(rotate_selectable(0, 3, true, |_| false), 0);
    assert_eq!(rotate_selectable(1, 3, false, |_| false), 1);
    assert_eq!(rotate_selectable(0, 0, true, |_| true), 0);
}

#[test]
fn rotate_selectable_skips_multiple_consecutive_unusable() {
    // [0: false, 1: false, 2: true, 3: false]
    let is_selectable = |i: usize| i == 2;
    assert_eq!(rotate_selectable(2, 4, true, is_selectable), 2);
    assert_eq!(rotate_selectable(2, 4, false, is_selectable), 2);
    assert_eq!(rotate_selectable(0, 4, true, is_selectable), 2);
    assert_eq!(rotate_selectable(0, 4, false, is_selectable), 2);
}

#[test]
fn rotate_selectable_clamps_out_of_bounds_cur() {
    // count = 2, cur = 10 (e.g. after search filtering)
    // [0: true, 1: true]
    assert_eq!(rotate_selectable(10, 2, true, |_| true), 0);
    assert_eq!(rotate_selectable(10, 2, false, |_| true), 0);
}
