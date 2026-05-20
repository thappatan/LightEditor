//! Flutter / Dart project detection (spec §4.5).
//!
//! Looks for `pubspec.yaml` in the workspace root and reads enough of
//! it to tell the host whether this is a Flutter project (a Dart
//! project that depends on the Flutter SDK) plus the package name.
//! Results feed the command palette: when a Flutter project is
//! detected, four entries appear — `Flutter: Run`, `Flutter: Hot
//! Reload`, `Flutter: Hot Restart`, and `Flutter: Stop` — and the
//! dispatch in `main.rs` writes the right bytes (`flutter run\n`, `r`,
//! `R`, `q`) into the embedded terminal.
//!
//! We deliberately avoid a YAML dep for this — the few keys we care
//! about (`name:` and `flutter:` inside `dependencies:`) are easy to
//! line-scan. A full YAML parser is a follow-up if we ever need more
//! structure.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// One device or emulator entry as reported by `flutter devices
/// --machine` (an array of these per invocation). We only consume
/// the fields the picker needs; `serde(default)` keeps unknown shapes
/// from breaking the parse.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FlutterDevice {
    /// Stable device id we hand back to `flutter run -d <id>`.
    pub id: String,
    /// Human label (`"iPhone 15 Pro"`, `"Chrome"`, `"macOS"`…).
    pub name: String,
    /// Whether this device is an emulator/simulator (vs a real
    /// connected device). Used purely for the label hint for now —
    /// the launch path is the same.
    #[serde(default)]
    pub emulator: bool,
    /// Platform tag from flutter's JSON output (`"ios"`, `"darwin"`,
    /// `"web-javascript"`, …). Empty when flutter didn't include it.
    #[serde(default, rename = "targetPlatform")]
    pub target_platform: String,
}

/// Run `flutter devices --machine` and parse the JSON output into a
/// list of `FlutterDevice`. Blocks the caller for as long as the
/// subprocess takes (typically 1–3 seconds) — host code should drive
/// this off a background thread and send the result back through the
/// event loop. Returns an empty Vec on any failure (binary missing,
/// non-zero exit, malformed JSON) so callers get a degrade-to-nothing
/// path.
pub fn list_devices() -> Vec<FlutterDevice> {
    let output = match std::process::Command::new("flutter")
        .args(["devices", "--machine"])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            log::warn!("flutter devices: spawn failed: {e}");
            return Vec::new();
        }
    };
    if !output.status.success() {
        log::warn!(
            "flutter devices: exit {:?} stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return Vec::new();
    }
    match serde_json::from_slice::<Vec<FlutterDevice>>(&output.stdout) {
        Ok(v) => v,
        Err(e) => {
            log::warn!(
                "flutter devices: json parse failed: {e}; raw={}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
            Vec::new()
        }
    }
}

/// One pubspec.yaml the host found in the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlutterProject {
    /// `name:` field from `pubspec.yaml`. Used as the palette label
    /// hint and (eventually) the status-bar indicator. Empty when
    /// `pubspec.yaml` had no `name:` line, which is unusual but
    /// shouldn't crash.
    pub name: String,
    /// The root the project lives in. In a multi-root workspace the
    /// Flutter app may not be the primary root, so `flutter run` `cd`s
    /// here first.
    pub root: PathBuf,
}

/// Inspect `root/pubspec.yaml`. Returns `Some(FlutterProject)` when
/// the manifest exists *and* declares a dependency on the Flutter SDK
/// (`flutter: { sdk: flutter }` under `dependencies:`). Pure-Dart
/// packages (server-side, CLI tools, libraries) return `None` since
/// `flutter run` doesn't apply to them.
pub fn detect_flutter(root: &Path) -> Option<FlutterProject> {
    let text = std::fs::read_to_string(root.join("pubspec.yaml")).ok()?;
    if !has_flutter_dependency(&text) {
        return None;
    }
    let name = parse_name(&text).unwrap_or_default();
    Some(FlutterProject {
        name,
        root: root.to_path_buf(),
    })
}

/// `name: <value>` extractor. The Flutter / Dart manifest puts this
/// at the document's top level so we just need the first
/// `^name:\s+<word>$` we see — no need to track YAML indentation.
fn parse_name(pubspec: &str) -> Option<String> {
    for line in pubspec.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            let v = rest.trim().trim_matches(|c: char| c == '"' || c == '\'');
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

/// Look for a `flutter:` entry nested under `dependencies:`. Spec
/// Flutter packages always have this; pure-Dart packages don't.
/// Cheap line scan: enter `dependencies:` mode on the section header,
/// exit it the next time we see a column-0 key, and inside the
/// section watch for any indented line starting with `flutter:`.
fn has_flutter_dependency(pubspec: &str) -> bool {
    let mut in_deps = false;
    for raw in pubspec.lines() {
        // Skip empty / comment lines so the section tracker doesn't
        // get confused.
        let line = raw.trim_end();
        if line.is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        // A line that starts at column 0 with a `:`-terminated key is
        // a new top-level section. Update state and continue.
        let unindented = !line.starts_with(' ') && !line.starts_with('\t');
        if unindented {
            in_deps = line.starts_with("dependencies:");
            continue;
        }
        if in_deps && line.trim_start().starts_with("flutter:") {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tempdir() -> PathBuf {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let pid = std::process::id();
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("editor-app-flutter-{pid}-{n}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn detect_returns_none_without_pubspec() {
        let root = tempdir();
        assert!(detect_flutter(&root).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_returns_none_for_pure_dart_package() {
        let root = tempdir();
        std::fs::write(
            root.join("pubspec.yaml"),
            "name: dart_tool\ndependencies:\n  args: ^2.4.0\n",
        )
        .unwrap();
        assert!(detect_flutter(&root).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_picks_up_flutter_sdk_dependency() {
        let root = tempdir();
        std::fs::write(
            root.join("pubspec.yaml"),
            "name: my_app\n\
             description: example\n\
             dependencies:\n  \
               flutter:\n    \
                 sdk: flutter\n  \
               cupertino_icons: ^1.0.2\n",
        )
        .unwrap();
        let detected = detect_flutter(&root).expect("Flutter project");
        assert_eq!(detected.name, "my_app");
        // The project remembers its root so `flutter run` can cd there.
        assert_eq!(detected.root, root);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_handles_quoted_name() {
        let root = tempdir();
        std::fs::write(
            root.join("pubspec.yaml"),
            "name: \"my_app\"\ndependencies:\n  flutter:\n    sdk: flutter\n",
        )
        .unwrap();
        let detected = detect_flutter(&root).expect("Flutter project");
        assert_eq!(detected.name, "my_app");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_ignores_flutter_keyword_outside_dependencies() {
        // The top-level `flutter:` block holds asset / font config
        // and isn't a dependency declaration; it shouldn't on its
        // own classify a project as Flutter.
        let root = tempdir();
        std::fs::write(
            root.join("pubspec.yaml"),
            "name: pure_dart\n\
             dependencies:\n  args: ^2.4.0\n\
             flutter:\n  uses-material-design: true\n",
        )
        .unwrap();
        assert!(detect_flutter(&root).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn detect_handles_comments_and_blank_lines() {
        let root = tempdir();
        std::fs::write(
            root.join("pubspec.yaml"),
            "# top-level comment\n\
             name: my_app\n\
             \n\
             dependencies:\n  \
               # inline comment\n  \
               flutter:\n    \
                 sdk: flutter\n",
        )
        .unwrap();
        assert!(detect_flutter(&root).is_some());
        std::fs::remove_dir_all(&root).ok();
    }
}
