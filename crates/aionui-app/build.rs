//! Copy `assets/builtin-assistants/` into the binary's target directory so
//! `BuiltinAssistantRegistry::load` can find it at runtime via
//! `{exe_dir}/assets/builtin-assistants/`.
//!
//! Placement rule: walk up from `OUT_DIR` (`target/<profile>/build/<pkg>/out`)
//! to `target/<profile>/` and copy into `assets/builtin-assistants/` there.
//! This makes the files available for both `cargo build` and `cargo run`.

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=assets/builtin-assistants");

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("assets/builtin-assistants");
    if !src.exists() {
        println!("cargo:warning=assets/builtin-assistants missing; skipping copy");
        return;
    }

    let out_dir =
        PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is always set in build scripts"));
    // OUT_DIR is e.g. target/<profile>/build/<pkg>-<hash>/out.
    // Walk up to target/<profile>/.
    let Some(target_dir) = out_dir.ancestors().nth(3) else {
        println!("cargo:warning=could not locate target/<profile> for asset copy");
        return;
    };
    let dst = target_dir.join("assets/builtin-assistants");

    if let Err(e) = copy_dir_recursive(&src, &dst) {
        println!("cargo:warning=failed to copy built-in assets: {e}");
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}
