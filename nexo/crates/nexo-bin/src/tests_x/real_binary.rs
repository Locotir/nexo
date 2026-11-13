//! Tests that read the test binary itself.

use super::*;

#[inline(never)]
fn xref_marker_target(x: u32) -> u32 {
    std::hint::black_box(x).wrapping_mul(0x9e37_79b1) ^ 0x1234_5678
}

#[inline(never)]
fn xref_marker_caller_one(x: u32) -> u32 {
    xref_marker_target(x).wrapping_add(1)
}

#[inline(never)]
fn xref_marker_caller_two(x: u32) -> u32 {
    xref_marker_target(x ^ 7).wrapping_add(2)
}

const XREF_STRING: &str = "xref-marker-string-3b9e5d0a";

#[inline(never)]
fn xref_marker_string_user() -> usize {
    std::hint::black_box(XREF_STRING).len()
}

#[test]
fn the_test_binary_shows_who_calls_a_marker_function_and_loads_a_marker_string() {
    let Some(img) = this_binary() else { return };
    let n = std::hint::black_box(3u32);
    let sum = xref_marker_caller_one(n).wrapping_add(xref_marker_caller_two(n));
    std::hint::black_box(sum);
    assert_eq!(xref_marker_string_user(), XREF_STRING.len());
    // an MSVC build keeps its names in a .pdb beside the binary, not in it: there is no name to ask by
    if cfg!(target_env = "msvc") {
        return;
    }
    let o = xref(&img, "xref_marker_target").unwrap();
    let calls_n = o.meta["counts"]["call"].as_u64().unwrap();
    assert!(calls_n >= 2, "{}", whole(&o));
    let t = whole(&o);
    assert!(t.contains("xref_marker_caller_one") && t.contains("xref_marker_caller_two"), "{t}");
    // calls says the same from the function's side
    let c = calls(&img, "xref_marker_caller_one").unwrap();
    assert!(whole(&c).contains("xref_marker_target"), "{}", whole(&c));
    // the string
    let o = xref_str(&img, XREF_STRING, false).unwrap();
    assert!(o.meta["references"].as_u64().unwrap() >= 1, "{}", whole(&o));
    assert!(whole(&o).contains("xref_marker_string_user"), "{}", whole(&o));
}
