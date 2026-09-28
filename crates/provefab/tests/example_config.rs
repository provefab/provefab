//! The example config at the repository root is what new users copy: it must load.

#[test]
fn the_example_config_loads() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../provefab.example.toml");
    let text = std::fs::read_to_string(path).unwrap();
    let config = provefab::config::Config::from_toml_str(&text).unwrap();
    assert_eq!(config.repos[0].label, "provefab");
    assert!(config.repos[0].merge.is_none());
}
