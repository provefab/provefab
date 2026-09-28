//! Spec §1 criterion 2: no paid code in the public repo. Symbols are built by
//! concatenation so this file never matches itself.

use std::path::Path;

fn scan(dir: &Path, needles: &[String], hits: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        if name == "target" || name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            scan(&path, needles, hits);
        } else if let Ok(text) = std::fs::read_to_string(&path) {
            for n in needles {
                if text.contains(n.as_str()) {
                    hits.push(format!("{}: {n}", path.display()));
                }
            }
        }
    }
}

#[test]
fn no_paid_code_in_the_public_tree() {
    let needles: Vec<String> = [
        ["merge", "_decision"],
        ["Merge", "Facts"],
        ["try", "_merge"],
        ["Guarded", "Merge"],
    ]
    .iter()
    .map(|p| p.concat())
    .collect();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut hits = Vec::new();
    scan(&root, &needles, &mut hits);
    assert!(hits.is_empty(), "paid code in the public tree: {hits:#?}");
}
