//! The README documents the Jira `site` rule: the stricter validation is visible
//! to users, and repository rule R5 wants the README updated with `docs/guide/`.

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
