//! Copies the built console frontend into `$OUT_DIR/dist` so `src/lib.rs`
//! can `include_dir!("$OUT_DIR/dist")` it into the binary.
//!
//! Never invokes `npm` — `console/dist` is produced by the frontend's own
//! `npm --prefix console run build`, out of band. When it hasn't been built
//! yet (a fresh checkout, or `cargo build` run before the frontend build
//! step), this falls back to a placeholder `index.html` so the workspace
//! still compiles; `af_console::is_placeholder()` lets a caller tell the
//! two cases apart at runtime.

use std::fs;
use std::path::Path;

/// What ships when `console/dist` hasn't been built. Also asserted almost
/// verbatim by `tests` in `src/lib.rs`.
const PLACEHOLDER_HTML: &str = "<!doctype html>\n<html>\n  <head>\n    <meta charset=\"utf-8\">\n    <title>af console</title>\n  </head>\n  <body>\n    <p>console not built &mdash; run <code>npm --prefix console run build</code> and rebuild</p>\n  </body>\n</html>\n";

fn main() {
    let out_dir = std::env::var("OUT_DIR").expect("cargo sets OUT_DIR for build scripts");
    let dist_out = Path::new(&out_dir).join("dist");
    // Relative to this crate's manifest directory (crates/af-console), which
    // is cargo's working directory for build scripts.
    let source_dist = Path::new("../../console/dist");
    let source_package_json = Path::new("../../console/package.json");

    // Cheap staleness signals: re-run if the built bundle changes, or if
    // package.json changes (a proxy for "the frontend moved on since the
    // last dist/ was built" even though it doesn't prove dist/ is stale).
    println!("cargo:rerun-if-changed={}", source_dist.display());
    println!("cargo:rerun-if-changed={}", source_package_json.display());

    // Drop any copy left over from a previous build so switching between a
    // present and an absent console/dist doesn't leave stale files behind
    // in $OUT_DIR/dist.
    let _ = fs::remove_dir_all(&dist_out);
    fs::create_dir_all(&dist_out).expect("create $OUT_DIR/dist");

    let is_placeholder = if source_dist.is_dir() {
        copy_dir_recursive(source_dist, &dist_out).expect("copy console/dist into $OUT_DIR/dist");
        false
    } else {
        fs::write(dist_out.join("index.html"), PLACEHOLDER_HTML)
            .expect("write placeholder index.html");
        println!("cargo:warning=af-console: console/dist not found, embedding placeholder");
        true
    };

    fs::write(
        Path::new(&out_dir).join("placeholder_flag.rs"),
        format!("const IS_PLACEHOLDER: bool = {is_placeholder};\n"),
    )
    .expect("write placeholder_flag.rs");
}

/// Recursively copies regular files and directories from `src` to `dst`.
/// Symlinks are skipped: a Vite `dist/` never produces them, and a build
/// script has no business following links outside its own tree.
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path)?;
        } else if file_type.is_file() {
            fs::copy(entry.path(), &dst_path)?;
        }
    }
    Ok(())
}
