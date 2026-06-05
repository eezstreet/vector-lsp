use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=contrib/d2rdoc/sync-schemas.ps1");

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
