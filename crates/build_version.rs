// The full version of qaros-rerun, derived at build time from VERSION and the git tags.
// Included by the build scripts of qaros-rerun and qaros-sink; the same rules as
// scripts/qaros-common.ps1 (Get-QarosRerunVersion):
//
// - HEAD tagged vX.Y.Z or vX.Y.Z-rc.N: that version; a tag whose X.Y.Z differs from VERSION
//   fails the build;
// - otherwise X.Y.Z-dev.<commits since the last v* tag>+<8-digit commit>, X.Y.Z from VERSION;
// - no git: X.Y.Z-dev.0+unknown.
//
// QAROS_RERUN_VERSION, when set, is taken as it is (its X.Y.Z must still match VERSION).

use std::path::{Path, PathBuf};
use std::process::Command;

fn git(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git").arg("-C").arg(workspace).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn is_release_tag(tag: &str) -> bool {
    let Some(version) = tag.strip_prefix('v') else { return false };
    let (core, rc) = match version.split_once("-rc.") {
        Some((core, rc)) => (core, Some(rc)),
        None => (version, None),
    };
    let numbers = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| numbers(p)) && rc.is_none_or(numbers)
}

fn core_of(version: &str) -> &str {
    version.split(['-', '+']).next().unwrap_or(version)
}

/// Prints the cargo directives and returns the full version.
pub fn full_version() -> String {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let version_file = workspace.join("VERSION");
    println!("cargo:rerun-if-changed={}", version_file.display());
    println!("cargo:rerun-if-env-changed=QAROS_RERUN_VERSION");
    let planned = std::fs::read_to_string(&version_file)
        .unwrap_or_else(|error| panic!("{} cannot be read: {error}", version_file.display()))
        .trim()
        .to_owned();

    if let Some(git_dir) = git(&workspace, &["rev-parse", "--absolute-git-dir"]) {
        let git_dir = PathBuf::from(git_dir);
        // A path that does not exist would rerun this script on every build.
        for file in ["HEAD", "packed-refs", "refs/tags"] {
            let path = git_dir.join(file);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
        // The branch HEAD names moves with every commit; it lives in the common git dir.
        if let Some(reference) = git(&workspace, &["symbolic-ref", "-q", "HEAD"])
            && let Some(common) = git(&workspace, &["rev-parse", "--git-common-dir"])
        {
            let common = PathBuf::from(common);
            let common = if common.is_absolute() { common } else { workspace.join(common) };
            let path = common.join(&reference);
            if path.exists() {
                println!("cargo:rerun-if-changed={}", path.display());
            }
        }
    }

    let version = if let Ok(given) = std::env::var("QAROS_RERUN_VERSION") {
        given
    } else if let Some(tag) = git(&workspace, &["tag", "--points-at", "HEAD", "--list", "v[0-9]*"])
        .and_then(|tags| tags.lines().map(str::trim).find(|tag| is_release_tag(tag)).map(str::to_owned))
    {
        tag[1..].to_owned()
    } else if let Some(commit) = git(&workspace, &["rev-parse", "--short=8", "HEAD"]) {
        let last_tag = git(&workspace, &["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*"]);
        let range = last_tag.map_or_else(|| "HEAD".to_owned(), |tag| format!("{tag}..HEAD"));
        let count = git(&workspace, &["rev-list", "--count", &range]).unwrap_or_else(|| "0".to_owned());
        format!("{planned}-dev.{count}+{commit}")
    } else {
        format!("{planned}-dev.0+unknown")
    };
    assert!(
        core_of(&version) == planned,
        "version {version} does not match VERSION ({planned}); a release tag must name the version in VERSION"
    );
    version
}
