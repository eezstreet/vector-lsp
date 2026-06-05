use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=contrib/d2rdoc/sync-schemas.ps1");
    println!("cargo:rerun-if-changed=contrib/");

    if std::env::var("CARGO_FEATURE_D2RDOC").is_err() {
        return;
    }

    if !schemas_present() {
        println!("cargo:warning=Schema files missing — running sync-schemas.ps1...");

        let status = Command::new("powershell")
            .args([
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "contrib\\d2rdoc\\sync-schemas.ps1",
            ])
            .status();

        match status {
            Ok(s) if s.success() => {}
            Ok(s) => println!(
                "cargo:warning=sync-schemas.ps1 exited with {s}; \
                 schema files may be incomplete"
            ),
            Err(e) => println!(
                "cargo:warning=Could not run sync-schemas.ps1: {e}; \
                 schema files may be missing at runtime"
            ),
        }
    }

    copy_contrib_to_target();
}

/// Returns true if at least one contrib/d2rdoc/<version>/schema/ directory
/// contains files, indicating schemas have been synced.
fn schemas_present() -> bool {
    let contrib = Path::new("contrib/d2rdoc");
    let Ok(entries) = std::fs::read_dir(contrib) else {
        return false;
    };
    for entry in entries.flatten() {
        let schema_dir = entry.path().join("schema");
        if schema_dir.is_dir() {
            if let Ok(mut inner) = std::fs::read_dir(&schema_dir) {
                if inner.next().is_some() {
                    return true;
                }
            }
        }
    }
    false
}

/// Copy the contrib/ tree into the profile output directory (e.g.
/// target/release/) so the binary and its runtime assets are co-located.
///
/// OUT_DIR is target/{profile}/build/vector-lsp-{hash}/out — four levels up
/// is target/{profile}/.
fn copy_contrib_to_target() {
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR not set");
    let target_profile_dir = Path::new(&out_dir)
        .ancestors()
        .nth(3)
        .expect("unexpected OUT_DIR depth")
        .to_owned();

    copy_dir_recursive(Path::new("contrib"), &target_profile_dir.join("contrib"));
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    let Ok(entries) = std::fs::read_dir(src) else {
        return;
    };
    std::fs::create_dir_all(dst).expect("failed to create output dir");
    for entry in entries.flatten() {
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if src_path.is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            std::fs::copy(&src_path, &dst_path).unwrap_or_else(|e| {
                println!("cargo:warning=Failed to copy {}: {e}", src_path.display());
                0
            });
        }
    }
}
