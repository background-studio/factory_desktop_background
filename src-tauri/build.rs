use std::{env, fs, path::PathBuf};

fn extract_raw(source: &str, marker: &str) -> String {
    let start = source
        .find(marker)
        .unwrap_or_else(|| panic!("payload marker not found: {marker}"))
        + marker.len();
    let tail = &source[start..];
    let end = tail
        .find("`;")
        .unwrap_or_else(|| panic!("payload marker is not terminated: {marker}"));
    tail[..end].to_string()
}

fn main() {
    let payload_path = PathBuf::from("../src/main/payload.ts");
    println!("cargo:rerun-if-changed={}", payload_path.display());
    let source = fs::read_to_string(&payload_path).expect("read shared TypeScript payload");
    let background_css = extract_raw(&source, "const BACKGROUND_CSS = String.raw`");
    let review_shadow_css = extract_raw(&source, "const REVIEW_SHADOW_CSS = String.raw`");
    let runtime_template = extract_raw(&source, "  return String.raw`");
    let generated = format!(
        "pub const BACKGROUND_CSS: &str = {background_css:?};\n\
         pub const REVIEW_SHADOW_CSS: &str = {review_shadow_css:?};\n\
         pub const PAYLOAD_TEMPLATE: &str = {runtime_template:?};\n"
    );
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("payload_assets.rs");
    fs::write(output, generated).expect("write generated payload assets");
}
