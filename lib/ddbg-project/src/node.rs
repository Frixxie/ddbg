//! Node.js projects (`package.json`), in JavaScript or TypeScript.
//!
//! The program is the package's `main` entry, the script run by its `start`
//! script, or a conventional entry file such as `src/index.ts`.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{Project, ProjectKind};

/// Extensions Node.js runs directly (TypeScript through type stripping).
pub const SCRIPT_EXTENSIONS: &[&str] = &["ts", "mts", "cts", "js", "mjs", "cjs"];

const ENTRY_FILES: &[&str] = &["index", "main", "src/index", "src/main"];

pub(crate) fn detect_in(dir: &Path) -> Option<Project> {
    let manifest = dir.join("package.json");
    if !manifest.is_file() {
        return None;
    }
    let package: Value = std::fs::read_to_string(&manifest)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let mut candidates = Vec::new();
    let mut add = |p: PathBuf| {
        if is_script(&p) && p.is_file() && !candidates.contains(&p) {
            candidates.push(p);
        }
    };
    if let Some(main) = package["main"].as_str() {
        add(dir.join(main));
    }
    if let Some(start) = package["scripts"]["start"].as_str() {
        start_script(start)
            .into_iter()
            .for_each(|s| add(dir.join(s)));
    }
    for name in ENTRY_FILES {
        for ext in SCRIPT_EXTENSIONS {
            add(dir.join(format!("{name}.{ext}")));
        }
    }
    Some(Project {
        kind: ProjectKind::Node,
        root: dir.to_owned(),
        binary: candidates.first().cloned(),
        candidates,
    })
}

pub fn is_script(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| SCRIPT_EXTENSIONS.contains(&e))
}

/// The script in a `start` script such as `node --watch src/main.ts`.
fn start_script(script: &str) -> Option<&str> {
    let mut words = script.split_whitespace();
    let runner = words.next()?;
    if !matches!(runner, "node" | "tsx" | "ts-node" | "bun" | "deno") {
        return None;
    }
    words.find(|w| !w.starts_with('-') && is_script(Path::new(w)))
}

/// A tool installed in `node_modules/.bin`, looking in `dir` and its
/// ancestors as Node.js does (workspaces hoist dependencies to the root).
pub fn node_bin(dir: &Path, tool: &str) -> Option<PathBuf> {
    dir.ancestors()
        .map(|d| d.join("node_modules").join(".bin").join(tool))
        .find(|p| p.is_file())
}

/// Whether the package in `dir` depends on `package`.
pub fn has_dependency(dir: &Path, package: &str) -> bool {
    let Some(manifest) = std::fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
    else {
        return false;
    };
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .any(|k| manifest[k].get(package).is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_script_entry() {
        assert_eq!(start_script("node src/main.ts"), Some("src/main.ts"));
        assert_eq!(
            start_script("node --watch --env-file=.env app.js"),
            Some("app.js")
        );
        assert_eq!(start_script("vite"), None);
        assert_eq!(start_script("node"), None);
    }

    #[test]
    fn detects_fixture() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/hello-node");
        let p = detect_in(&dir).unwrap();
        assert_eq!(p.kind, ProjectKind::Node);
        assert_eq!(p.binary, Some(dir.join("src/main.ts")));
        assert!(has_dependency(&dir, "vitest"));
        assert!(!has_dependency(&dir, "jest"));
    }
}
