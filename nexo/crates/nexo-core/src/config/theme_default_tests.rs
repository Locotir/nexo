#[test]
fn a_fresh_config_uses_the_nexo_theme() {
    assert_eq!(super::Config::default().ui.theme, "nexo");
}
