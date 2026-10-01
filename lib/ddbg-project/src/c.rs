//! C / C++ projects (CMake, Meson, Make).
//!
//! There is no common machine-readable interface for build outputs, so
//! candidates are native executables found in the project root and the usual
//! build directories.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::{Project, ProjectKind};

const MARKERS: &[&str] = &[
    "CMakeLists.txt",
    "meson.build",
    "Makefile",
    "makefile",
    "GNUmakefile",
    "compile_commands.json",
];

/// Directories searched (recursively, up to [`MAX_DEPTH`]) for executables.
const BUILD_DIRS: &[&str] = &["build", "out", "bin", "builddir", "cmake-build-debug"];
const MAX_DEPTH: usize = 3;

pub(crate) fn detect_in(dir: &Path) -> Option<Project> {
    if !MARKERS.iter().any(|m| dir.join(m).is_file()) {
        return None;
    }
    let mut candidates = Vec::new();
    collect(dir, 0, &mut candidates);
    for b in BUILD_DIRS {
        collect(&dir.join(b), 1, &mut candidates);
    }
    candidates.sort();
    candidates.dedup();
    let binary = match candidates.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    Some(Project {
        kind: ProjectKind::C,
        root: dir.to_owned(),
        binary,
        candidates,
    })
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || name.ends_with(".dSYM") || name == "CMakeFiles" {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            // The root itself is only scanned shallowly; build dirs go deeper.
            if depth > 0 && depth < MAX_DEPTH {
                collect(&path, depth + 1, out);
            }
        } else if ft.is_file() && is_native_executable(&path) {
            out.push(path);
        }
    }
}

/// Executable bit set (on Unix) and an ELF, Mach-O or PE header.
pub(crate) fn is_native_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match path.metadata() {
            Ok(m) if m.permissions().mode() & 0o111 != 0 => {}
            _ => return false,
        }
    }
    if path
        .extension()
        .is_some_and(|e| matches!(e.to_str(), Some("so" | "dylib" | "dll" | "a" | "o")))
    {
        return false;
    }
    let mut magic = [0u8; 4];
    let Ok(mut f) = fs::File::open(path) else {
        return false;
    };
    if f.read_exact(&mut magic).is_err() {
        return false;
    }
    matches!(
        magic,
        [0x7f, b'E', b'L', b'F']
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
    ) || magic[..2] == *b"MZ"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_a_marker() {
        let dir = std::env::temp_dir().join("ddbg-c-nomarker");
        let _ = fs::create_dir_all(&dir);
        assert!(detect_in(&dir).is_none());
    }

    #[test]
    fn scripts_are_not_native() {
        let dir = std::env::temp_dir().join("ddbg-c-script");
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("run.sh");
        fs::write(&script, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(!is_native_executable(&script));
    }
}
