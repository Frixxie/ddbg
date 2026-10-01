//! .NET project detection.
//!
//! Finds project files (`*.csproj`, `*.fsproj`, `*.vbproj`) or solutions
//! (`*.sln`, `*.slnx`) and locates the built assembly of the executable
//! project under `bin/<Configuration>/<TargetFramework>[/<RID>]/`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{Project, ProjectKind};

const PROJECT_EXTS: &[&str] = &["csproj", "fsproj", "vbproj"];
const SOLUTION_EXTS: &[&str] = &["sln", "slnx"];

/// Detect a .NET project or solution located directly in `dir`.
pub(crate) fn detect_in(dir: &Path) -> Option<Project> {
    let projects = projects_in(dir)?;
    let candidates: Vec<PathBuf> = projects
        .iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(p).ok()?;
            let info = ProjectInfo::parse(&text, p)?;
            Some(assembly_path(p.parent()?, &info))
        })
        .collect();
    let binary = match candidates.as_slice() {
        [only] => Some(only.clone()),
        _ => None,
    };
    Some(Project {
        kind: ProjectKind::DotNet,
        root: dir.to_owned(),
        binary,
        candidates,
    })
}

/// Project files in `dir`: the `*.csproj`/`*.fsproj`/`*.vbproj` files there,
/// or else the existing projects referenced by `*.sln`/`*.slnx` files there.
/// `None` if `dir` has neither.
pub fn projects_in(dir: &Path) -> Option<Vec<PathBuf>> {
    let files = files_with_ext(dir, PROJECT_EXTS);
    if !files.is_empty() {
        return Some(files);
    }
    let solutions = files_with_ext(dir, SOLUTION_EXTS);
    if solutions.is_empty() {
        return None;
    }
    let mut projects: Vec<PathBuf> = solutions
        .iter()
        .flat_map(|s| {
            let text = std::fs::read_to_string(s).unwrap_or_default();
            solution_projects(&text)
                .into_iter()
                .map(|p| dir.join(p))
                .collect::<Vec<_>>()
        })
        .filter(|p| p.is_file())
        .collect();
    projects.sort();
    projects.dedup();
    Some(projects)
}

/// Launch settings for a built .NET assembly, mirroring `dotnet run`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DotNetLaunch {
    /// Project directory (content root, where `appsettings*.json` live).
    pub cwd: PathBuf,
    /// Environment variables to set for the debuggee.
    pub env: BTreeMap<String, String>,
}

/// Find the project owning `assembly` (a `bin/...` dll) and compute the
/// working directory and environment `dotnet run` would use: variables from
/// the first `"commandName": "Project"` profile in
/// `Properties/launchSettings.json`, defaulting `ASPNETCORE_ENVIRONMENT` and
/// `DOTNET_ENVIRONMENT` to `Development`. Variables already set in the
/// current process environment are not overridden.
pub fn dotnet_launch(assembly: &Path) -> Option<DotNetLaunch> {
    let project_dir = assembly
        .ancestors()
        .skip(1)
        .find(|d| !files_with_ext(d, PROJECT_EXTS).is_empty())?
        .to_owned();
    let mut env = BTreeMap::new();
    if let Ok(text) = std::fs::read_to_string(project_dir.join("Properties/launchSettings.json")) {
        env = launch_settings_env(&text);
    }
    let environment = env
        .get("ASPNETCORE_ENVIRONMENT")
        .or_else(|| env.get("DOTNET_ENVIRONMENT"))
        .cloned()
        .unwrap_or_else(|| "Development".to_owned());
    for key in ["ASPNETCORE_ENVIRONMENT", "DOTNET_ENVIRONMENT"] {
        env.entry(key.to_owned())
            .or_insert_with(|| environment.clone());
    }
    env.retain(|k, _| std::env::var_os(k).is_none());
    Some(DotNetLaunch {
        cwd: project_dir,
        env,
    })
}

/// `environmentVariables` of the first `Project` profile in launchSettings.json.
fn launch_settings_env(text: &str) -> BTreeMap<String, String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return BTreeMap::new();
    };
    let Some(profiles) = v["profiles"].as_object() else {
        return BTreeMap::new();
    };
    profiles
        .values()
        .find(|p| p["commandName"] == "Project")
        .and_then(|p| p["environmentVariables"].as_object())
        .map(|vars| {
            vars.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

fn files_with_ext(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| exts.contains(&e))
        })
        .collect();
    out.sort();
    out
}

/// Relative project paths referenced by a `.sln` or `.slnx` file.
fn solution_projects(text: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let raw = if line.starts_with("Project(") {
            // Project("{GUID}") = "Name", "path\to\App.csproj", "{GUID}"
            line.split('"').nth(5)
        } else if line.starts_with("<Project ") {
            // <Project Path="src/App/App.csproj" />
            line.split("Path=\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
        } else {
            None
        };
        if let Some(raw) = raw {
            let path = PathBuf::from(raw.replace('\\', "/"));
            let is_project = path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| PROJECT_EXTS.contains(&e));
            if is_project {
                out.push(path);
            }
        }
    }
    out
}

#[derive(Debug, PartialEq)]
struct ProjectInfo {
    assembly: String,
    frameworks: Vec<String>,
}

impl ProjectInfo {
    /// Parse an executable, non-test project. Returns `None` for libraries
    /// and test projects.
    fn parse(text: &str, path: &Path) -> Option<Self> {
        let output = tag(text, "OutputType")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let web = text.contains("Sdk=\"Microsoft.NET.Sdk.Web\"")
            || text.contains("Sdk=\"Microsoft.NET.Sdk.Worker\"");
        let is_exe = output == "exe" || output == "winexe" || (output.is_empty() && web);
        let is_test = text.contains("Microsoft.NET.Test.Sdk")
            || tag(text, "IsTestProject").is_some_and(|v| v.eq_ignore_ascii_case("true"));
        if !is_exe || is_test {
            return None;
        }
        let assembly = tag(text, "AssemblyName")
            .map(str::to_owned)
            .or_else(|| Some(path.file_stem()?.to_str()?.to_owned()))?;
        let frameworks = tag(text, "TargetFramework")
            .or_else(|| tag(text, "TargetFrameworks"))
            .map(|v| {
                v.split(';')
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Some(Self {
            assembly,
            frameworks,
        })
    }
}

/// Text content of the first `<name>...</name>` element.
fn tag<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let start = text.find(&open)? + open.len();
    let end = text[start..].find(&format!("</{name}>"))? + start;
    Some(text[start..end].trim())
}

/// Most recently built `<assembly>.dll`, or the expected Debug path.
fn assembly_path(project_dir: &Path, info: &ProjectInfo) -> PathBuf {
    let dll = format!("{}.dll", info.assembly);
    let bin = project_dir.join("bin");
    let mut found: Vec<(SystemTime, PathBuf)> = Vec::new();
    for config in ["Debug", "Release"] {
        for tfm in subdirs(&bin.join(config)) {
            for dir in std::iter::once(tfm.clone()).chain(subdirs(&tfm)) {
                let p = dir.join(&dll);
                if let Ok(t) = p.metadata().and_then(|m| m.modified()) {
                    found.push((t, p));
                }
            }
        }
    }
    if let Some((_, p)) = found.into_iter().max_by_key(|(t, _)| *t) {
        return p;
    }
    let mut p = bin.join("Debug");
    if let Some(tfm) = info.frameworks.first() {
        p.push(tfm);
    }
    p.join(dll)
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| p.is_dir())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_console_project() {
        let text = r#"<Project Sdk="Microsoft.NET.Sdk">
  <PropertyGroup>
    <OutputType>Exe</OutputType>
    <TargetFramework>net8.0</TargetFramework>
  </PropertyGroup>
</Project>"#;
        let info = ProjectInfo::parse(text, Path::new("/x/App.csproj")).unwrap();
        assert_eq!(info.assembly, "App");
        assert_eq!(info.frameworks, vec!["net8.0"]);
    }

    #[test]
    fn assembly_name_and_web_sdk() {
        let text = r#"<Project Sdk="Microsoft.NET.Sdk.Web">
  <PropertyGroup>
    <TargetFrameworks>net8.0;net9.0</TargetFrameworks>
    <AssemblyName>My.Api</AssemblyName>
  </PropertyGroup>
</Project>"#;
        let info = ProjectInfo::parse(text, Path::new("/x/Api.csproj")).unwrap();
        assert_eq!(info.assembly, "My.Api");
        assert_eq!(info.frameworks, vec!["net8.0", "net9.0"]);
    }

    #[test]
    fn skips_libraries_and_tests() {
        let lib = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>
<TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>"#;
        assert_eq!(ProjectInfo::parse(lib, Path::new("Lib.csproj")), None);
        let test = r#"<Project Sdk="Microsoft.NET.Sdk"><PropertyGroup>
<OutputType>Exe</OutputType></PropertyGroup>
<ItemGroup><PackageReference Include="Microsoft.NET.Test.Sdk" /></ItemGroup></Project>"#;
        assert_eq!(ProjectInfo::parse(test, Path::new("T.csproj")), None);
    }

    #[test]
    fn parses_solutions() {
        let sln = r#"
Project("{FAE04EC0-301F-11D3-BF4B-00C04F79EFBC}") = "App", "src\App\App.csproj", "{1}"
EndProject
Project("{2150E333-8FDC-42A3-9474-1A3956D46DE8}") = "src", "src", "{2}"
EndProject"#;
        assert_eq!(
            solution_projects(sln),
            vec![PathBuf::from("src/App/App.csproj")]
        );
        let slnx = r#"<Solution>
  <Folder Name="/src/">
    <Project Path="src/Api/Api.csproj" />
  </Folder>
  <Project Path="tests/T/T.fsproj" />
</Solution>"#;
        assert_eq!(
            solution_projects(slnx),
            vec![
                PathBuf::from("src/Api/Api.csproj"),
                PathBuf::from("tests/T/T.fsproj")
            ]
        );
    }

    #[test]
    fn detects_project_dir_end_to_end() {
        let dir = std::env::temp_dir().join(format!("ddbg-dotnet-{}", std::process::id()));
        let app = dir.join("src/App");
        std::fs::create_dir_all(app.join("bin/Debug/net8.0")).unwrap();
        std::fs::write(
            app.join("App.csproj"),
            "<Project><PropertyGroup><OutputType>Exe</OutputType>\
             <TargetFramework>net8.0</TargetFramework></PropertyGroup></Project>",
        )
        .unwrap();
        std::fs::write(app.join("bin/Debug/net8.0/App.dll"), "").unwrap();
        std::fs::write(
            dir.join("All.slnx"),
            "<Solution>\n<Project Path=\"src/App/App.csproj\" />\n</Solution>",
        )
        .unwrap();

        let p = detect_in(&dir).unwrap();
        assert_eq!(p.kind, ProjectKind::DotNet);
        assert_eq!(p.binary, Some(app.join("bin/Debug/net8.0/App.dll")));
        let p = crate::detect(&app.join("bin")).unwrap();
        assert_eq!(p.root, app);

        std::fs::create_dir_all(app.join("Properties")).unwrap();
        std::fs::write(
            app.join("Properties/launchSettings.json"),
            r#"{"profiles":{"IIS":{"commandName":"IISExpress"},
                "App":{"commandName":"Project","environmentVariables":
                {"ASPNETCORE_ENVIRONMENT":"Local","FOO":"bar"}}}}"#,
        )
        .unwrap();
        let l = dotnet_launch(&app.join("bin/Debug/net8.0/App.dll")).unwrap();
        assert_eq!(l.cwd, app);
        if std::env::var_os("ASPNETCORE_ENVIRONMENT").is_none() {
            assert_eq!(l.env["ASPNETCORE_ENVIRONMENT"], "Local");
        }
        if std::env::var_os("DOTNET_ENVIRONMENT").is_none() {
            assert_eq!(l.env["DOTNET_ENVIRONMENT"], "Local");
        }
        if std::env::var_os("FOO").is_none() {
            assert_eq!(l.env["FOO"], "bar");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
