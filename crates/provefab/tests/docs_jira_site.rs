//! The README documents the Jira `site` rule (R5).

#[test]
fn readme_states_the_jira_site_rule() {
    let readme = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../README.md"),
    )
    .unwrap();
    assert!(
        readme.contains("DNS host name"),
        "README lacks the Jira site rule"
    );
    assert!(readme.contains("no IP address"));
    assert!(!readme.contains('\u{2014}'), "em-dash in README");
}
