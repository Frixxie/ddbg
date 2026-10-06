use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn main() {
    // Metadata describes the whole executable, not just this frontend crate.
    for path in [
        "../",
        "../../bin",
        "../../Cargo.toml",
        "../../Cargo.lock",
        "../../.git/HEAD",
        "../../.git/index",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }
    println!("cargo:rerun-if-env-changed=DDBG_BUILD_REVISION");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
    // Git metadata is also available in colocated jj repositories; source
    // archives can supply the revision explicitly, or report it as unknown.
    let revision = std::env::var("DDBG_BUILD_REVISION")
        .ok()
        .or_else(|| {
            let mut revision = git(&["rev-parse", "--short=12", "HEAD"])?;
            if git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty())
            {
                revision.push_str("+dirty");
            }
            Some(revision)
        })
        .unwrap_or_else(|| "unknown".into());
    let timestamp = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        });
    println!("cargo:rustc-env=DDBG_BUILD_REVISION={revision}");
    println!("cargo:rustc-env=DDBG_BUILD_TIMESTAMP={timestamp}");
}
