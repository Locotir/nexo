//! An image moved to the address its module has in a running program, and back.

use std::path::PathBuf;

use crate::fixtures;
use crate::Image;

const LIVE: u64 = 0x5555_5555_4000;

fn img() -> Image {
    let f = fixtures::elf64(false);
    Image::from_bytes(f.bytes, PathBuf::from("/fx/prog")).expect("the fixture opens")
}

fn main_addr(img: &Image) -> u64 {
    img.symbols.iter().find(|s| s.name == "main").expect("the fixture has main").addr
}

#[test]
fn every_address_moves_by_live_base_minus_image_base() {
    let mut img = img();
    let (base, entry, main) = (img.image_base, img.entry, main_addr(&img));
    let secs: Vec<u64> = img.sections.iter().map(|s| s.addr).collect();
    let delta = img.slide_to(LIVE).expect("an ELF moves");
    assert_eq!(delta, LIVE.wrapping_sub(base));
    assert_eq!(img.image_base, LIVE);
    assert_eq!(img.entry, entry.wrapping_add(delta));
    for (s, old) in img.sections.iter().zip(secs) {
        assert_eq!(s.addr, old.wrapping_add(delta), "{}", s.name);
    }
    // file address to live address, and the name follows
    assert_eq!(main_addr(&img), main + delta);
    assert_eq!(img.name_at(main + delta), Some("main"));
    assert_eq!(img.name_at(main), None);
    assert!(img.is_exec(main + delta));
    assert!(!img.is_exec(main));
}

#[test]
fn sliding_by_the_opposite_amount_gives_the_file_back() {
    let mut img = img();
    let (base, entry, main) = (img.image_base, img.entry, main_addr(&img));
    let delta = img.slide_to(LIVE).expect("moves");
    img.slide(delta.wrapping_neg()).expect("moves back");
    assert_eq!((img.image_base, img.entry, main_addr(&img)), (base, entry, main));
    assert_eq!(img.name_at(main), Some("main"));
}

#[test]
fn a_module_already_at_its_base_is_left_alone() {
    let mut img = img();
    let base = img.image_base;
    assert_eq!(img.slide_to(base).expect("no move"), 0);
    assert_eq!(img.image_base, base);
}
