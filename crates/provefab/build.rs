// The worker plugins are embedded with include_dir!, which on stable Rust only
// tracks edits to files it already knows about. Watching the directory makes an
// added or removed plugin file rebuild the binary too (final review M8).
fn main() {
    println!("cargo::rerun-if-changed=../../plugins");
}
