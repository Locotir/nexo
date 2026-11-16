//! The opening of a session: the banner coming in while its mark ignites in the sky under it (see
//! `fx_ignite`), skipped to its end state by a key and shown in that end state with the motion off.

use super::*;
use crate::frame::Load;
use crate::render::blocks::INTRO_MS;
use crossterm::event::Event;

const W: u16 = 100;
const H: u16 = 30;

/// The braille dots on the rows of the transcript under the banner (the banner takes the first seven).
fn sky_dots(rows: &[String]) -> usize {
    rows[7..rows.len() - 4].iter().flat_map(|r| r.chars()).filter(|c| ('\u{2801}'..='\u{28ff}').contains(c)).count()
}

fn opened_at(ms: u64) -> App {
    let (mut a, _rx) = app();
    let t0 = a.opened;
    fx_at(&mut a, t0, ms);
    a
}

fn press(a: &mut App, c: char) {
    crate::event::handle(a, Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)));
}

#[test]
fn the_mark_ignites_under_the_banner_and_is_gone_when_the_opening_ends() {
    let mut a = opened_at(700);
    let mid = screen(&mut a, W, H);
    assert!(sky_dots(&mid) > 40, "the large mark is in the sky: {mid:#?}");
    assert!(mid[1].contains("nexo"), "and the banner is not touched by it");
    let mut end = opened_at(INTRO_MS);
    assert_eq!(sky_dots(&screen(&mut end, W, H)), 0, "nothing of it is left");
    assert_eq!(end.intro_ms(), None);
    // one more frame clears its last dots, then it asks for none
    assert_eq!(end.load(), Load::Motion);
    let mut after = opened_at(INTRO_MS + 101);
    assert_ne!(after.load(), Load::Motion, "no frames are asked for once it is over");
    assert_eq!(sky_dots(&screen(&mut after, W, H)), 0);
}

#[test]
fn a_key_skips_the_opening_to_its_end_state_and_still_does_what_it_does() {
    let mut a = opened_at(300);
    assert_eq!(a.load(), Load::Motion);
    press(&mut a, 'h');
    assert_eq!((a.intro_ms(), a.intro_running()), (None, false));
    assert_ne!(a.load(), Load::Motion, "no frames are asked for it any more");
    assert_eq!(a.input.text(), "h", "the key is typed");
    let cut = screen(&mut a, W, H);
    let mut still = opened_at(INTRO_MS + 200);
    still.input.set_text("h");
    let settled = screen(&mut still, W, H);
    assert_eq!(cut[..7], settled[..7], "the banner is the still one");
    assert_eq!(sky_dots(&cut), 0, "the sky is empty");
    // a key after the opening changes nothing of it
    press(&mut still, 'i');
    assert!(!still.fx.intro_cut);
}

#[test]
fn with_the_motion_off_the_opening_is_its_end_state_from_the_first_frame() {
    let mut a = opened_at(0);
    a.opts.animations = false;
    assert_eq!((a.intro_ms(), a.intro_running()), (None, false));
    assert_eq!(a.load(), Load::Idle);
    let first = screen(&mut a, W, H);
    let mut still = opened_at(INTRO_MS + 200);
    assert_eq!(first[..7], screen(&mut still, W, H)[..7]);
    assert_eq!(sky_dots(&first), 0);
}

#[test]
fn the_sky_stays_dark_without_the_starfield_on_a_mono_theme_or_without_room() {
    let mut off = opened_at(700);
    off.opts.home_stars = false;
    let rows = screen(&mut off, W, H);
    assert_eq!(sky_dots(&rows), 0);
    assert!(!rows[2].contains("auto") && !rows[2].contains("YOLO"), "the banner still comes in: {:?}", rows[2]);
    let mut mono = opened_at(700);
    mono.theme = crate::theme::Theme::mono();
    assert_eq!(sky_dots(&screen(&mut mono, W, H)), 0);
    let mut small = opened_at(700);
    assert_eq!(sky_dots(&screen(&mut small, 40, 16)), 0, "no room for it under the banner");
}

#[test]
fn the_mark_keeps_clear_of_a_notice_under_the_banner() {
    let mut a = opened_at(700);
    a.push(Block::Notice { level: Level::Info, text: "a notice under the banner".into() });
    assert!(a.fresh());
    let mid = screen(&mut a, W, H);
    let row = mid.iter().position(|r| r.contains("a notice under the banner")).expect("the notice");
    let mut end = opened_at(INTRO_MS);
    end.push(Block::Notice { level: Level::Info, text: "a notice under the banner".into() });
    let after = screen(&mut end, W, H);
    for r in row.saturating_sub(1)..=row + 1 {
        assert_eq!(mid[r], after[r], "row {r}: the notice and the rows by it are left alone");
    }
    assert!(sky_dots(&mid) > 0, "the mark is drawn where there is room");
}
