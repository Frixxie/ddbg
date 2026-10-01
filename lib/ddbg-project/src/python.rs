//! Python projects (`pyproject.toml`, `setup.py`, ...).
//!
//! Candidates are entry scripts: files containing an
//! `if __name__ == "__main__"` guard and `__main__.py` package entry points.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Project, ProjectKind};

const MARKERS: &[&str] = &[
    "pyproject.toml",
    "setup.py",
    "setup.cfg",
    "requirements.txt",
    "Pipfile",
    "pytest.ini",
];

const SKIP_DIRS: &[&str] = &[
    "__pycache__",
    "venv",
    "env",
    "build",
    "dist",
    "node_modules",
    "site-packages",
    "tests",
    "test",
];
const MAX_DEPTH: usize = 3;

pub(crate) fn detect_in(dir: &Path) -> Option<Project> {
    if !MARKERS.iter().any(|m| dir.join(m).is_file()) {
        return None;
    }
    let mut candidates = Vec::new();
    collect(dir, 0, &mut candidates);
    candidates.sort();
    let binary = match candidates.as_slice() {
        [only] => Some(only.clone()),
        _ => ["main.py", "__main__.py", "app.py", "manage.py"]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| candidates.contains(p)),
    };
    Some(Project {
        kind: ProjectKind::Python,
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
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_ref()) {
            continue;
        }
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            if depth < MAX_DEPTH {
                collect(&path, depth + 1, out);
            }
        } else if name.ends_with(".py") && is_entry_point(&path) {
            out.push(path);
        }
    }
}

fn is_entry_point(path: &Path) -> bool {
    if path.file_name().is_some_and(|n| n == "__main__.py") {
        return true;
    }
    let Ok(src) = fs::read_to_string(path) else {
        return false;
    };
    src.lines().any(|l| {
        let l = l.trim_start();
        l.starts_with("if __name__") && l.contains("__main__")
    })
}

/// The interpreter to use for a project: a local virtualenv when present,
/// otherwise `None` (callers fall back to `python3` on `PATH`).
pub fn python_interpreter(root: &Path) -> Option<PathBuf> {
    let bin = if cfg!(windows) {
        "Scripts/python.exe"
    } else {
        "bin/python"
    };
    if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
        let p = Path::new(&venv).join(bin);
        if p.is_file() {
            return Some(p);
        }
    }
    [".venv", "venv", "env"]
        .iter()
        .map(|d| root.join(d).join(bin))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_entry_points() {
        let dir = std::env::temp_dir().join("ddbg-py-detect");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("pkg")).unwrap();
        fs::create_dir_all(dir.join("tests")).unwrap();
        fs::write(dir.join("pyproject.toml"), "").unwrap();
        fs::write(
            dir.join("main.py"),
            "if __name__ == \"__main__\":\n    pass\n",
        )
        .unwrap();
        fs::write(dir.join("lib.py"), "def f(): pass\n").unwrap();
        fs::write(dir.join("pkg/__main__.py"), "").unwrap();
        fs::write(
            dir.join("tests/test_x.py"),
            "if __name__ == '__main__': pass\n",
        )
        .unwrap();

        let p = detect_in(&dir).unwrap();
        assert_eq!(p.kind, ProjectKind::Python);
        assert_eq!(
            p.candidates,
            vec![dir.join("main.py"), dir.join("pkg/__main__.py")]
        );
        assert_eq!(p.binary, Some(dir.join("main.py")));
    }
}
