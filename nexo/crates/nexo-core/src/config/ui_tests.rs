use super::tests::load_pair;
use super::*;

#[test]
fn a_key_nexo_no_longer_reads_is_ignored() {
    let c: Config = toml::from_str("[ui]\nretired_key = true\ntheme = \"nord\"\n").unwrap();
    assert_eq!(c.ui.theme, "nord");
    assert_eq!(c.ui.notify_after, 15);
}

#[test]
fn the_screen_is_quiet_unless_detail_is_full() {
    let c: Config = toml::from_str("[ui]\ntheme = \"nord\"\n").unwrap();
    assert_eq!(c.ui.detail, Detail::Quiet);
    let c: Config = toml::from_str("[ui]\ndetail = \"full\"\n").unwrap();
    assert_eq!(c.ui.detail, Detail::Full);
    let c: Config = toml::from_str("[ui]\ndetail = \"Quiet\"\n").unwrap();
    assert_eq!(c.ui.detail, Detail::Quiet);
    // an unknown word reads as the default
    let c: Config = toml::from_str("[ui]\ndetail = \"loud\"\n").unwrap();
    assert_eq!(c.ui.detail, Detail::Quiet);
}

#[test]
fn the_welcome_figure_is_the_galaxy_unless_another_is_named() {
    let c: Config = toml::from_str("[ui]\ntheme = \"nord\"\n").unwrap();
    assert_eq!(c.ui.figure, Figure::Galaxy);
    let c: Config = toml::from_str("[ui]\nfigure = \"constellation\"\n").unwrap();
    assert_eq!(c.ui.figure, Figure::Constellation);
    let c: Config = toml::from_str("[ui]\nfigure = \" None \"\n").unwrap();
    assert_eq!(c.ui.figure, Figure::None);
    // an unknown word reads as the default
    let c: Config = toml::from_str("[ui]\nfigure = \"comet\"\n").unwrap();
    assert_eq!(c.ui.figure, Figure::Galaxy);
}

#[test]
fn the_home_stars_and_the_bursts_are_on_unless_turned_off() {
    let c: Config = toml::from_str("[ui]\ntheme = \"nord\"\n").unwrap();
    assert!(c.ui.home_stars && c.ui.bursts);
    let c: Config = toml::from_str("[ui]\nhome_stars = false\nbursts = false\n").unwrap();
    assert!(!c.ui.home_stars && !c.ui.bursts);
}

#[test]
fn animations_are_on_unless_turned_off() {
    let c: Config = toml::from_str("[ui]\ntheme = \"nord\"\n").unwrap();
    assert!(c.ui.animations);
    let c: Config = toml::from_str("[ui]\nanimations = false\n").unwrap();
    assert!(!c.ui.animations);
}

#[test]
fn the_todo_strip_is_off_unless_turned_on() {
    let c: Config = toml::from_str("[ui]\ntheme = \"nord\"\n").unwrap();
    assert!(!c.ui.todo_strip);
    let c: Config = toml::from_str("[ui]\ntodo_strip = true\n").unwrap();
    assert!(c.ui.todo_strip);
}

#[test]
fn sandbox_settings_parse_in_every_mode() {
    use crate::sandbox::SandboxMode;
    let c = Config::default();
    assert_eq!(c.permissions.sandbox, SandboxMode::Off);
    assert!(!c.permissions.sandbox_network);
    for (text, mode) in [
        ("off", SandboxMode::Off),
        ("workspace-write", SandboxMode::WorkspaceWrite),
        ("read-only", SandboxMode::ReadOnly),
    ] {
        let c: Config = toml::from_str(&format!("[permissions]\nsandbox = \"{text}\"\n")).unwrap();
        assert_eq!(c.permissions.sandbox, mode, "{text}");
    }
    let c: Config = toml::from_str(
        "[permissions]\nsandbox = \"workspace-write\"\nsandbox_network = true\nsandbox_write_dirs = [\"/srv/data\"]\n",
    )
    .unwrap();
    assert!(c.permissions.sandbox_network);
    assert_eq!(c.permissions.sandbox_write_dirs, vec![PathBuf::from("/srv/data")]);
    assert!(toml::from_str::<Config>("[permissions]\nsandbox = \"sideways\"\n").is_err());
}

#[test]
fn an_untrusted_project_cannot_loosen_the_sandbox() {
    use crate::sandbox::SandboxMode;
    let user = "[permissions]\nsandbox = \"workspace-write\"\n";
    let proj = "[permissions]\nsandbox = \"off\"\nsandbox_network = true\nsandbox_write_dirs = [\"/\"]\n";
    let c = load_pair(user, proj, false);
    assert_eq!(c.permissions.sandbox, SandboxMode::WorkspaceWrite);
    assert!(!c.permissions.sandbox_network && c.permissions.sandbox_write_dirs.is_empty());
    let t = load_pair(user, proj, true);
    assert_eq!(t.permissions.sandbox, SandboxMode::Off);
    assert!(t.permissions.sandbox_network);
    let c = load_pair(user, "[permissions]\nsandbox = \"read-only\"\n", false);
    assert_eq!(c.permissions.sandbox, SandboxMode::ReadOnly, "a project may tighten it");
}
