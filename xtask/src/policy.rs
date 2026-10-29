//! Repository policy checks (`cargo xtask policy`).
//!
//! The checks enforce the project's absolute technology requirements:
//!
//! 1. Every library and binary crate root declares `#![forbid(unsafe_code)]`
//!    and every workspace member inherits the workspace lints.
//! 2. No Rust code in the workspace uses the `unsafe` keyword, declares
//!    `extern` items or ABIs, links native libraries, exports unmangled
//!    symbols, or contains inline assembly. Comments and string literals are
//!    ignored, so documentation may mention these words.
//! 3. No C, C++ or assembly source files exist in the repository.
//! 4. Workspace crates have no build scripts.
//! 5. No resolved dependency links a native library or builds native code,
//!    unless it is listed in [`ALLOWLIST`] with a justification.
//! 6. The shipped crates contain no code that writes files (milestone 1 is
//!    read-only).

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Crates that are shipped to users. During milestone 1 their non-test
/// sources must not contain any file-writing operation.
const READ_ONLY_CRATES: &[&str] = &["ferrufus-core", "ferrufus-cli", "ferrufus-gui"];

/// Build-time dependencies that mean a crate compiles or links native code.
const NATIVE_BUILD_TOOLS: &[&str] = &["cc", "cmake", "bindgen", "pkg-config", "vcpkg"];

/// A dependency allowed to declare a native build tool, under conditions that
/// guarantee no native code is compiled or linked. The conditions are checked
/// against the features actually enabled in the resolved graph, so a feature
/// change that would start compiling C fails the policy again.
struct Allowed {
    package: &'static str,
    tool: &'static str,
    required_features: &'static [&'static str],
    forbidden_features: &'static [&'static str],
    reason: &'static str,
}

const ALLOWLIST: &[Allowed] = &[
    Allowed {
        package: "wayland-backend",
        tool: "cc",
        required_features: &[],
        forbidden_features: &["log"],
        reason: "build.rs compiles its C log shims only when the `log` feature is enabled",
    },
    Allowed {
        package: "wayland-sys",
        tool: "pkg-config",
        required_features: &["dlopen"],
        forbidden_features: &[],
        reason: "with `dlopen`, build.rs returns before probing; libwayland is loaded at run time",
    },
    Allowed {
        package: "x11-dl",
        tool: "pkg-config",
        required_features: &[],
        forbidden_features: &[],
        reason: "build.rs only records library directories for run-time loading and links glibc's libdl",
    },
];

/// File extensions of native source code that must not exist in the repository.
const NATIVE_SOURCE_EXTENSIONS: &[&str] = &[
    "c", "cc", "cpp", "cxx", "c++", "h", "hh", "hpp", "hxx", "s", "S", "asm", "m", "mm",
];

/// Directories that are never scanned.
const SKIPPED_DIRS: &[&str] = &["target", ".git", "dist"];

/// A pattern searched for in code (comments and literals removed).
struct Pattern {
    text: &'static str,
    /// Require non-identifier characters on both sides.
    whole_word: bool,
    reason: &'static str,
}

const FORBIDDEN_CODE: &[Pattern] = &[
    Pattern {
        text: "unsafe",
        whole_word: true,
        reason: "`unsafe` code is forbidden",
    },
    Pattern {
        text: "extern",
        whole_word: true,
        reason: "`extern` items and foreign ABIs (FFI) are forbidden",
    },
    Pattern {
        text: "#[link",
        whole_word: false,
        reason: "linking native libraries is forbidden",
    },
    Pattern {
        text: "no_mangle",
        whole_word: false,
        reason: "exporting unmangled symbols is forbidden",
    },
    Pattern {
        text: "export_name",
        whole_word: false,
        reason: "exporting symbols is forbidden",
    },
    Pattern {
        text: "asm!",
        whole_word: false,
        reason: "inline assembly is forbidden",
    },
];

/// File-writing operations that the shipped crates must not contain in
/// milestone 1.
const WRITE_OPERATIONS: &[&str] = &[
    "File::create",
    "fs::write",
    ".write(true)",
    ".append(true)",
    "create_new(true)",
    "fs::copy",
    "fs::rename",
    "remove_file",
    "remove_dir",
    "create_dir",
    "set_permissions",
];

pub fn run(root: &Path) -> Result<(), String> {
    let metadata = cargo_metadata(root)?;
    let mut violations = Vec::new();

    let members = workspace_members(&metadata)?;
    check_crate_roots_and_lints(&members, &mut violations)?;
    check_sources(root, &members, &mut violations)?;
    check_native_sources(root, &mut violations)?;
    let checked = check_dependencies(&metadata, &members, &mut violations)?;

    println!(
        "policy: {} workspace crates, {checked} resolved dependencies checked",
        members.len()
    );
    if violations.is_empty() {
        println!("policy: OK");
        Ok(())
    } else {
        for violation in &violations {
            eprintln!("policy violation: {violation}");
        }
        Err(format!("{} policy violation(s)", violations.len()))
    }
}

struct Member {
    name: String,
    id: String,
    manifest: PathBuf,
    dir: PathBuf,
    targets: Vec<(Vec<String>, PathBuf)>,
}

fn cargo_metadata(root: &Path) -> Result<Value, String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let host = host_triple()?;
    let output = Command::new(cargo)
        .current_dir(root)
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--filter-platform",
            &host,
        ])
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("cannot parse cargo metadata: {e}"))
}

fn host_triple() -> Result<String, String> {
    let output = Command::new("rustc")
        .arg("-vV")
        .output()
        .map_err(|e| format!("cannot run rustc -vV: {e}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("host: ").map(str::to_string))
        .ok_or_else(|| "rustc -vV did not report a host triple".to_string())
}

fn workspace_members(metadata: &Value) -> Result<Vec<Member>, String> {
    let ids: BTreeSet<&str> = metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata has no workspace_members")?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    let packages = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?;
    let mut members = Vec::new();
    for package in packages {
        let id = package["id"].as_str().unwrap_or_default();
        if !ids.contains(id) {
            continue;
        }
        let manifest = PathBuf::from(package["manifest_path"].as_str().unwrap_or_default());
        let dir = manifest.parent().map(Path::to_path_buf).unwrap_or_default();
        let mut targets = Vec::new();
        for target in package["targets"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let kinds = target["kind"]
                .as_array()
                .map(|kinds| {
                    kinds
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            let src = PathBuf::from(target["src_path"].as_str().unwrap_or_default());
            targets.push((kinds, src));
        }
        members.push(Member {
            name: package["name"].as_str().unwrap_or_default().to_string(),
            id: id.to_string(),
            manifest,
            dir,
            targets,
        });
    }
    Ok(members)
}

fn check_crate_roots_and_lints(
    members: &[Member],
    violations: &mut Vec<String>,
) -> Result<(), String> {
    const FORBID: &str = "#![forbid(unsafe_code)]";
    for member in members {
        let manifest = read(&member.manifest)?;
        if !inherits_workspace_lints(&manifest) {
            violations.push(format!(
                "{}: Cargo.toml must contain `[lints] workspace = true`",
                member.name
            ));
        }
        for (kinds, src) in &member.targets {
            if kinds.iter().any(|kind| kind == "custom-build") {
                violations.push(format!(
                    "{}: build scripts are not allowed ({})",
                    member.name,
                    src.display()
                ));
                continue;
            }
            let is_crate_root = kinds.iter().any(|kind| {
                matches!(
                    kind.as_str(),
                    "lib" | "rlib" | "dylib" | "cdylib" | "staticlib" | "proc-macro" | "bin"
                )
            });
            if is_crate_root && !code_only(&read(src)?).contains(FORBID) {
                violations.push(format!(
                    "{}: {} must declare {FORBID}",
                    member.name,
                    src.display()
                ));
            }
        }
    }
    Ok(())
}

fn inherits_workspace_lints(manifest: &str) -> bool {
    let mut in_lints = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_lints = line == "[lints]";
        } else if in_lints && line.replace(' ', "") == "workspace=true" {
            return true;
        }
    }
    false
}

fn check_sources(
    root: &Path,
    members: &[Member],
    violations: &mut Vec<String>,
) -> Result<(), String> {
    for member in members {
        let mut files = Vec::new();
        collect_files(&member.dir, &mut files)?;
        let read_only = READ_ONLY_CRATES.contains(&member.name.as_str());
        let src_dir = member.dir.join("src");
        for file in files
            .iter()
            .filter(|f| f.extension().is_some_and(|e| e == "rs"))
        {
            let code = code_only(&read(file)?);
            let shown = file
                .strip_prefix(root)
                .unwrap_or(file)
                .display()
                .to_string();
            let check_writes = read_only && file.starts_with(&src_dir);
            for (number, line) in code.lines().enumerate() {
                for pattern in FORBIDDEN_CODE {
                    if matches_pattern(line, pattern) {
                        violations.push(format!("{shown}:{}: {}", number + 1, pattern.reason));
                    }
                }
                if !check_writes {
                    continue;
                }
                if let Some(operation) = WRITE_OPERATIONS.iter().find(|op| line.contains(**op)) {
                    violations.push(format!(
                        "{shown}:{}: `{operation}` writes files; shipped crates are read-only in milestone 1",
                        number + 1
                    ));
                }
            }
        }
    }
    Ok(())
}

fn matches_pattern(line: &str, pattern: &Pattern) -> bool {
    if !pattern.whole_word {
        return line.contains(pattern.text);
    }
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(pattern.text).any(|(start, matched)| {
        let before = line[..start].chars().next_back();
        let after = line[start + matched.len()..].chars().next();
        !before.is_some_and(is_ident) && !after.is_some_and(is_ident)
    })
}

/// Returns `source` with comments and the contents of string and character
/// literals replaced by spaces. Line breaks are kept, so line numbers stay
/// valid. This is a small lexer, sufficient for policy checks: it understands
/// line and (nested) block comments, plain, byte and C strings, raw strings,
/// character literals and lifetimes.
fn code_only(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let blank = |c: char| if c == '\n' { '\n' } else { ' ' };
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        let next = chars.get(i + 1).copied();
        if c == '/' && next == Some('/') {
            while let Some(&c) = chars.get(i) {
                if c == '\n' {
                    break;
                }
                out.push(' ');
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            let mut depth = 0usize;
            while let Some(&c) = chars.get(i) {
                let next = chars.get(i + 1).copied();
                if c == '/' && next == Some('*') {
                    depth += 1;
                    out.push_str("  ");
                    i += 2;
                } else if c == '*' && next == Some('/') {
                    depth = depth.saturating_sub(1);
                    out.push_str("  ");
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    out.push(blank(c));
                    i += 1;
                }
            }
        } else if let Some(hashes) = raw_string_start(&chars, i) {
            // Keep the prefix and quote, blank the contents.
            let prefix_len = chars[i..].iter().position(|&c| c == '"').unwrap_or(0) + 1;
            for _ in 0..prefix_len {
                out.push(' ');
            }
            i += prefix_len;
            while let Some(&c) = chars.get(i) {
                if c == '"' && (1..=hashes).all(|k| chars.get(i + k) == Some(&'#')) {
                    for _ in 0..=hashes {
                        out.push(' ');
                    }
                    i += hashes + 1;
                    break;
                }
                out.push(blank(c));
                i += 1;
            }
        } else if c == '"' {
            out.push(' ');
            i += 1;
            while let Some(&c) = chars.get(i) {
                if c == '\\' {
                    out.push(' ');
                    if let Some(&escaped) = chars.get(i + 1) {
                        out.push(blank(escaped));
                    }
                    i += 2;
                } else if c == '"' {
                    out.push(' ');
                    i += 1;
                    break;
                } else {
                    out.push(blank(c));
                    i += 1;
                }
            }
        } else if c == '\'' {
            // Character literal or lifetime.
            if next == Some('\\') {
                out.push(' ');
                i += 1;
                while let Some(&c) = chars.get(i) {
                    out.push(' ');
                    i += 1;
                    if c == '\'' {
                        break;
                    }
                }
            } else if chars.get(i + 2) == Some(&'\'') {
                out.push_str("   ");
                i += 3;
            } else {
                out.push(c);
                i += 1;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    out
}

/// If a raw string (`r"`, `r#"`, `br"`, `cr#"`, ...) starts at `i`, returns
/// its number of `#` characters.
fn raw_string_start(chars: &[char], i: usize) -> Option<usize> {
    let previous_is_ident = i
        .checked_sub(1)
        .and_then(|p| chars.get(p))
        .is_some_and(|c| c.is_ascii_alphanumeric() || *c == '_');
    if previous_is_ident {
        return None;
    }
    let mut j = i;
    if matches!(chars.get(j), Some('b' | 'c')) {
        j += 1;
    }
    if chars.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let mut hashes = 0;
    while chars.get(j) == Some(&'#') {
        hashes += 1;
        j += 1;
    }
    (chars.get(j) == Some(&'"')).then_some(hashes)
}

fn check_native_sources(root: &Path, violations: &mut Vec<String>) -> Result<(), String> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    for file in files {
        let extension = file
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default();
        if NATIVE_SOURCE_EXTENSIONS.contains(&extension) {
            let shown = file
                .strip_prefix(root)
                .unwrap_or(&file)
                .display()
                .to_string();
            violations.push(format!(
                "{shown}: C, C++ and assembly sources are not allowed"
            ));
        }
    }
    Ok(())
}

fn check_dependencies(
    metadata: &Value,
    members: &[Member],
    violations: &mut Vec<String>,
) -> Result<usize, String> {
    let packages: BTreeMap<&str, &Value> = metadata["packages"]
        .as_array()
        .ok_or("cargo metadata has no packages")?
        .iter()
        .filter_map(|package| package["id"].as_str().map(|id| (id, package)))
        .collect();
    let member_ids: BTreeSet<&str> = members.iter().map(|member| member.id.as_str()).collect();
    let nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or("cargo metadata has no resolve graph")?;

    let mut checked = 0;
    for node in nodes {
        let id = node["id"].as_str().unwrap_or_default();
        let Some(package) = packages.get(id) else {
            continue;
        };
        let name = package["name"].as_str().unwrap_or_default();
        let features: BTreeSet<&str> = node["features"]
            .as_array()
            .map(|features| features.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let allowed = |tool: &str| {
            ALLOWLIST.iter().find(|entry| {
                entry.package == name
                    && entry.tool == tool
                    && entry.required_features.iter().all(|f| features.contains(f))
                    && !entry
                        .forbidden_features
                        .iter()
                        .any(|f| features.contains(f))
            })
        };
        if !member_ids.contains(id) {
            checked += 1;
            if let Some(links) = package["links"].as_str() {
                violations.push(format!(
                    "dependency `{name}` links the native library `{links}`"
                ));
            }
        }
        for dep in node["deps"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let is_build = dep["dep_kinds"].as_array().is_some_and(|kinds| {
                kinds
                    .iter()
                    .any(|kind| kind["kind"].as_str() == Some("build"))
            });
            let dep_name = dep["pkg"]
                .as_str()
                .and_then(|pkg| packages.get(pkg))
                .and_then(|package| package["name"].as_str())
                .unwrap_or_default();
            if !is_build || !NATIVE_BUILD_TOOLS.contains(&dep_name) {
                continue;
            }
            match allowed(dep_name) {
                Some(entry) => {
                    println!("policy: allowed `{name}` → `{dep_name}`: {}", entry.reason)
                }
                None => violations.push(format!(
                    "dependency `{name}` uses `{dep_name}` at build time (native code)"
                )),
            }
        }
    }
    Ok(checked)
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))
}

fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("cannot stat {}: {e}", path.display()))?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if file_type.is_dir() {
            if !SKIPPED_DIRS.contains(&name.as_ref()) {
                collect_files(&path, files)?;
            }
        } else if file_type.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unsafe_pattern() -> &'static Pattern {
        &FORBIDDEN_CODE[0]
    }

    #[test]
    fn keywords_in_code_are_found() {
        let code = code_only("fn f() { unsafe { g() } }\n");
        assert!(matches_pattern(&code, unsafe_pattern()));
        let code = code_only("pub unsafe fn f() {}\n");
        assert!(matches_pattern(&code, unsafe_pattern()));
    }

    #[test]
    fn comments_and_literals_are_ignored() {
        let source = "// unsafe\n/* unsafe /* nested */ unsafe */\nlet s = \"unsafe\";\nlet r = r#\"unsafe \"x\" \"#;\nlet c = 'u';\n";
        let code = code_only(source);
        assert!(!matches_pattern(&code, unsafe_pattern()));
        assert_eq!(code.lines().count(), source.lines().count());
    }

    #[test]
    fn identifiers_containing_the_keyword_are_not_matched() {
        let code = code_only("#![forbid(unsafe_code)]\nlet not_unsafe = 1;\n");
        assert!(!matches_pattern(&code, unsafe_pattern()));
        assert!(code.contains("#![forbid(unsafe_code)]"));
    }

    #[test]
    fn lifetimes_do_not_start_literals() {
        let code = code_only("fn f<'a>(x: &'a str) -> &'a str { unsafe { x } }\n");
        assert!(matches_pattern(&code, unsafe_pattern()));
    }

    #[test]
    fn workspace_lints_are_detected() {
        assert!(inherits_workspace_lints(
            "[package]\nname = \"x\"\n\n[lints]\nworkspace = true\n"
        ));
        assert!(!inherits_workspace_lints(
            "[package]\nname = \"x\"\n[dependencies]\nworkspace = true\n"
        ));
    }
}
