use super::tests::load_pair;
use super::*;

#[test]
fn the_defaults_are_the_installed_browser_clean_and_1280_by_800() {
    let b = Config::default().browser;
    assert_eq!(b.source, BrowserSource::Installed);
    assert_eq!(b.profile, BrowserProfile::Ephemeral);
    assert_eq!((b.headless, b.path.as_deref()), (None, None));
    assert_eq!((b.window.width, b.window.height), (1280, 800));
    assert_eq!(Config::parse("").unwrap().browser, b, "a file without [browser] is the default");
}

#[test]
fn every_key_parses() {
    let c = Config::parse(
        "[browser]\nsource = 'own-chrome'\nprofile = 'persistent'\nheadless = false\npath = '/opt/chrome/chrome'\nwindow = '1440x900'\n",
    )
    .unwrap();
    let b = c.browser;
    assert_eq!((b.source, b.profile, b.headless), (BrowserSource::OwnChrome, BrowserProfile::Persistent, Some(false)));
    assert_eq!(b.path.as_deref(), Some("/opt/chrome/chrome"));
    assert_eq!((b.window.width, b.window.height), (1440, 900));
    for (word, want) in [
        ("installed", BrowserSource::Installed),
        ("own-chrome", BrowserSource::OwnChrome),
        ("auto", BrowserSource::Auto),
    ] {
        let c = Config::parse(&format!("[browser]\nsource = '{word}'\n")).unwrap();
        assert_eq!(c.browser.source, want, "{word}");
    }
}

#[test]
fn a_word_that_is_not_an_option_is_an_error_that_lists_the_options() {
    let e = Config::parse("[browser]\nsource = 'firefox'\n").unwrap_err().to_string();
    assert!(e.contains("installed") && e.contains("own-chrome") && e.contains("auto"), "{e}");
    let e = Config::parse("[browser]\nprofile = 'forever'\n").unwrap_err().to_string();
    assert!(e.contains("ephemeral") && e.contains("persistent"), "{e}");
    assert!(Config::parse("[browser]\nheadless = 'yes'\n").is_err(), "headless is a true or false");
}

#[test]
fn a_window_is_width_by_height() {
    for (text, want) in
        [("1280x800", (1280, 800)), ("1024X768", (1024, 768)), (" 800, 600 ", (800, 600)), ("1920 1080", (1920, 1080))]
    {
        let w = WindowSize::parse(text).unwrap_or_else(|| panic!("{text:?}"));
        assert_eq!((w.width, w.height), want, "{text:?}");
    }
    for bad in ["", "1280", "1280x", "x800", "axb", "1280x800x2", "100x100", "20000x800", "-5x800", "1280*800"] {
        assert_eq!(WindowSize::parse(bad), None, "{bad:?}");
    }
    let e = Config::parse("[browser]\nwindow = 'wide'\n").unwrap_err().to_string();
    assert!(e.contains("WIDTHxHEIGHT") && e.contains("wide"), "{e}");
    assert_eq!(WindowSize { width: 640, height: 480 }.to_string(), "640x480");
}

#[test]
fn the_settings_survive_the_round_trip_the_merge_makes() {
    let c = Config::parse("[browser]\nprofile = 'persistent'\nsource = 'auto'\nwindow = '900x700'\n").unwrap();
    let text = toml::to_string(&c).unwrap();
    assert!(text.contains("window = \"900x700\"") && text.contains("source = \"auto\""), "{text}");
    assert_eq!(Config::parse(&text).unwrap().browser, c.browser);
}

#[test]
fn a_project_layers_over_the_user_key_by_key() {
    let c =
        load_pair("[browser]\nprofile = 'persistent'\nwindow = '1000x700'\n", "[browser]\nheadless = true\n", false);
    assert_eq!(c.browser.profile, BrowserProfile::Persistent, "the user's key stays");
    assert_eq!(c.browser.headless, Some(true), "the project's key applies");
    assert_eq!((c.browser.window.width, c.browser.window.height), (1000, 700));
}

#[test]
fn an_untrusted_project_cannot_name_the_program_to_run() {
    let user = "[browser]\npath = '/usr/bin/chromium'\n";
    let hostile = "[browser]\npath = '/tmp/evil'\nprofile = 'persistent'\n";
    let c = load_pair(user, hostile, false);
    assert_eq!(c.browser.path.as_deref(), Some("/usr/bin/chromium"), "the user's own path stays");
    assert_eq!(c.browser.profile, BrowserProfile::Persistent, "a harmless setting still applies");
    assert!(c.notices.iter().any(|n| n.contains("browser.path")), "{:?}", c.notices);
    let c = load_pair("", hostile, false);
    assert_eq!(c.browser.path, None);
    let c = load_pair(user, hostile, true);
    assert_eq!(c.browser.path.as_deref(), Some("/tmp/evil"), "a trusted project may");
}

#[test]
fn a_misspelt_key_is_reported() {
    let text = "[browser]\nprofil = 'persistent'\n";
    let c = Config::parse(text).unwrap();
    let said = unread_keys(text, &c).unwrap();
    assert!(said.contains("browser.profil") && said.contains("did you mean profile"), "{said}");
    assert_eq!(
        unread_keys(
            "[browser]\nprofile = 'persistent'\nheadless = true\nwindow = '800x600'\n",
            &Config::parse("[browser]\nprofile = 'persistent'\nheadless = true\nwindow = '800x600'\n").unwrap()
        ),
        None
    );
}
