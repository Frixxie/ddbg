//! Project detection.
//!
//! Walks upward from a directory to find a project root and, when possible,
//! the binary that should be debugged. Supports Rust (Cargo), .NET
//! (`*.csproj` / `*.fsproj` / `*.vbproj`, `*.sln` / `*.slnx`), Python
//! (`pyproject.toml`, `setup.py`, ...) and C/C++ (CMake, Meson, Make)
//! projects.

mod c;
mod dotnet;
mod python;

pub use dotnet::{DotNetLaunch, dotnet_launch, projects_in as dotnet_projects};
pub use python::python_interpreter;

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Kind of project discovered in a directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectKind {
    Rust,
    DotNet,
    Python,
    /// C or C++ (built with CMake, Meson or Make).
    C,
}

/// A detected project root.
#[derive(Debug, Clone)]
pub struct Project {
    pub kind: ProjectKind,
    pub root: PathBuf,
    /// Binary to debug, if one could be determined unambiguously.
    pub binary: Option<PathBuf>,
    /// All binary candidates found in the project.
    pub candidates: Vec<PathBuf>,
}

/// Detect the project containing `dir`, if any. The nearest project marker
/// wins; within one directory, Cargo takes precedence over .NET, then Python,
/// then C (a Makefile often just wraps another toolchain).
pub fn detect(dir: &Path) -> Option<Project> {
    for d in dir.ancestors() {
        let manifest = d.join("Cargo.toml");
        if manifest.is_file() {
            return detect_rust(dir, &manifest);
        }
        if let Some(p) = dotnet::detect_in(d) {
            return Some(p);
        }
        if let Some(p) = python::detect_in(d) {
            return Some(p);
        }
        if let Some(p) = c::detect_in(d) {
            return Some(p);
        }
    }
    None
}

fn detect_rust(dir: &Path, manifest: &Path) -> Option<Project> {
    let out = Command::new("cargo")
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(manifest)
        .current_dir(dir)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let meta: Value = serde_json::from_slice(&out.stdout).ok()?;
    let mut project = parse_cargo_metadata(&meta, manifest)?;
    // Prefer a build that exists; fall back to debug profile path.
    if let Some(bin) = &project.binary {
        let release = release_variant(bin);
        if !bin.exists() && release.as_ref().is_some_and(|r| r.exists()) {
            project.binary = release;
        }
    }
    Some(project)
}

fn release_variant(bin: &Path) -> Option<PathBuf> {
    let name = bin.file_name()?;
    let target_dir = bin.parent()?.parent()?;
    Some(target_dir.join("release").join(name))
}

/// Build a [`Project`] from `cargo metadata` output. `manifest` is the nearest
/// `Cargo.toml`, used to prefer the package the user is standing in.
pub fn parse_cargo_metadata(meta: &Value, manifest: &Path) -> Option<Project> {
    let root = PathBuf::from(meta["workspace_root"].as_str()?);
    let target_dir = PathBuf::from(meta["target_directory"].as_str()?);
    let debug_dir = target_dir.join("debug");
    let exe = |name: &str| debug_dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));

    let packages = meta["packages"].as_array()?;
    let bins_of = |pkg: &Value| -> Vec<String> {
        pkg["targets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| {
                t["kind"]
                    .as_array()
                    .is_some_and(|k| k.iter().any(|k| k == "bin"))
            })
            .filter_map(|t| t["name"].as_str().map(str::to_owned))
            .collect()
    };
    let pick = |pkg: &Value, bins: &[String]| -> Option<String> {
        if let Some(d) = pkg["default_run"].as_str() {
            return Some(d.to_owned());
        }
        match bins {
            [only] => Some(only.clone()),
            _ => None,
        }
    };

    let all: Vec<String> = packages.iter().flat_map(bins_of).collect();
    let candidates: Vec<PathBuf> = all.iter().map(|n| exe(n)).collect();

    let manifest = manifest
        .canonicalize()
        .unwrap_or_else(|_| manifest.to_owned());
    let current = packages.iter().find(|p| {
        p["manifest_path"]
            .as_str()
            .map(|m| Path::new(m).canonicalize().unwrap_or_else(|_| m.into()))
            .is_some_and(|m| m == manifest)
    });

    let name = current
        .and_then(|p| pick(p, &bins_of(p)))
        .or_else(|| match all.as_slice() {
            [only] => Some(only.clone()),
            _ => None,
        });

    Some(Project {
        kind: ProjectKind::Rust,
        root,
        binary: name.map(|n| exe(&n)),
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn meta(packages: Value) -> Value {
        json!({
            "workspace_root": "/ws",
            "target_directory": "/ws/target",
            "packages": packages,
        })
    }

    fn pkg(manifest: &str, bins: &[&str], default_run: Option<&str>) -> Value {
        let mut targets: Vec<Value> = bins
            .iter()
            .map(|b| json!({ "name": b, "kind": ["bin"] }))
            .collect();
        targets.push(json!({ "name": "lib", "kind": ["lib"] }));
        json!({ "manifest_path": manifest, "targets": targets, "default_run": default_run })
    }

    fn exe(name: &str) -> PathBuf {
        PathBuf::from(format!(
            "/ws/target/debug/{name}{}",
            std::env::consts::EXE_SUFFIX
        ))
    }

    #[test]
    fn single_binary() {
        let m = meta(json!([pkg("/ws/Cargo.toml", &["app"], None)]));
        let p = parse_cargo_metadata(&m, Path::new("/ws/Cargo.toml")).unwrap();
        assert_eq!(p.kind, ProjectKind::Rust);
        assert_eq!(p.root, PathBuf::from("/ws"));
        assert_eq!(p.binary, Some(exe("app")));
    }

    #[test]
    fn workspace_with_one_binary_from_root() {
        let m = meta(json!([
            pkg("/ws/lib/a/Cargo.toml", &[], None),
            pkg("/ws/bin/b/Cargo.toml", &["b"], None),
        ]));
        let p = parse_cargo_metadata(&m, Path::new("/ws/Cargo.toml")).unwrap();
        assert_eq!(p.binary, Some(exe("b")));
    }

    #[test]
    fn ambiguous_prefers_current_package() {
        let m = meta(json!([
            pkg("/ws/x/Cargo.toml", &["x"], None),
            pkg("/ws/y/Cargo.toml", &["y"], None),
        ]));
        let p = parse_cargo_metadata(&m, Path::new("/ws/y/Cargo.toml")).unwrap();
        assert_eq!(p.binary, Some(exe("y")));
        let p = parse_cargo_metadata(&m, Path::new("/ws/Cargo.toml")).unwrap();
        assert_eq!(p.binary, None);
        assert_eq!(p.candidates.len(), 2);
    }

    #[test]
    fn default_run_wins() {
        let m = meta(json!([pkg("/ws/Cargo.toml", &["a", "b"], Some("b"))]));
        let p = parse_cargo_metadata(&m, Path::new("/ws/Cargo.toml")).unwrap();
        assert_eq!(p.binary, Some(exe("b")));
    }
}
