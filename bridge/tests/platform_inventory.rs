//! **THE PLATFORM INVENTORY CANNOT ROT.** `docs/platform-dependencies.md` lists every place
//! the bridge or the agent crate depends on macOS. This test scans the source for the
//! spellings that mark such a dependency and fails on any match the table does not cover,
//! and on any row whose spelling its file no longer contains.
//!
//! It lives under `tests/`, outside the scanned trees, so its own spelling list cannot match
//! itself; the scan also skips this file by name in case it ever moves.
//!
//! Matching scheme: the table's **File** cell is one backticked repo-relative path, and each
//! backticked entry of its **Spellings** cell is one spelling. A match is a (file, spelling)
//! pair, found on a line that is not a whole-line comment; it is covered when some row has
//! that file and lists that spelling.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The spellings that mark a macOS dependency in Rust source.
const RUST_SPELLINGS: &[&str] = &[
    "target_os = \"macos\"",
    "Command::new(\"sips\")",
    "Command::new(\"afconvert\")",
    "Command::new(\"security\")",
    "Command::new(\"launchctl\")",
    "Command::new(\"sandbox-exec\")",
    "core_graphics",
    "core_foundation",
    // The Core Graphics FFI uses no crate; its `#[link(..., kind = "framework")]` is the mark.
    "kind = \"framework\"",
    "Library/",
    // The code calls its tools by ABSOLUTE PATH, so these are what catch sips, afconvert,
    // plutil, sandbox-exec, codesign and the iMCP app.
    "/usr/bin/",
    "/bin/launchctl",
    "/System/",
    "/Applications/",
    "resolve_bin(",
    "\"sips\"",
    "\"afconvert\"",
    "\"AFCONVERT\"",
    "\"security\"",
    "\"launchctl\"",
    "\"codesign\"",
    "\"plutil\"",
    "\"sandbox-exec\"",
    "\"textutil\"",
    "\"osascript\"",
    "\"xcodebuild\"",
];

/// The spellings that mark a macOS dependency in a Cargo manifest.
const MANIFEST_SPELLINGS: &[&str] = &["target_os = \"macos\"", "\"metal\""];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("bridge/ has a parent")
        .to_path_buf()
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("platform_inventory.rs")
        {
            out.push(p);
        }
    }
}

/// The (file, spelling) pairs present in `text`, skipping whole-line comments.
fn matches_in(text: &str, comment: &str, spellings: &[&str]) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for line in text.lines() {
        if line.trim_start().starts_with(comment) {
            continue;
        }
        for s in spellings {
            if line.contains(s) {
                found.insert(s.to_string());
            }
        }
    }
    found
}

/// Every (file, spelling) match in the scanned trees and manifests.
fn scan() -> BTreeMap<String, BTreeSet<String>> {
    let root = repo_root();
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut files = Vec::new();
    rust_files(&root.join("bridge/src"), &mut files);
    rust_files(&root.join("agent/src"), &mut files);
    assert!(files.len() > 50, "the scan found the source trees");
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        let m = matches_in(&text, "//", RUST_SPELLINGS);
        if !m.is_empty() {
            out.insert(rel(&root, &f), m);
        }
    }
    for manifest in ["bridge/Cargo.toml", "agent/Cargo.toml"] {
        let text = std::fs::read_to_string(root.join(manifest)).unwrap();
        let m = matches_in(&text, "#", MANIFEST_SPELLINGS);
        if !m.is_empty() {
            out.insert(manifest.to_string(), m);
        }
    }
    out
}

/// One table row: its file, its spellings, its replacement word.
#[derive(Debug)]
struct Row {
    file: String,
    spellings: Vec<String>,
    replacement: String,
}

fn backticked(cell: &str) -> Vec<String> {
    cell.split('`')
        .enumerate()
        .filter(|(i, _)| i % 2 == 1)
        .map(|(_, s)| s.to_string())
        .collect()
}

/// The rows of the `## Platform dependencies` table.
fn parse_rows(doc: &str) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut in_section = false;
    for line in doc.lines() {
        if line.starts_with("## ") {
            in_section = line.trim() == "## Platform dependencies";
            continue;
        }
        if !in_section || !line.starts_with('|') {
            continue;
        }
        let cells: Vec<&str> = line
            .trim()
            .trim_matches('|')
            .split(" | ")
            .map(str::trim)
            .collect();
        if cells.len() != 7 || cells[0] == "Dependency" || cells[0].starts_with("---") {
            continue;
        }
        let file = backticked(cells[1]);
        assert_eq!(file.len(), 1, "one file per row: {line}");
        rows.push(Row {
            file: file[0].clone(),
            spellings: backticked(cells[3]),
            replacement: cells[6].to_string(),
        });
    }
    rows
}

fn doc() -> String {
    std::fs::read_to_string(repo_root().join("docs/platform-dependencies.md"))
        .expect("docs/platform-dependencies.md exists")
}

/// What `found` has that `rows` do not cover, as `file: spelling` lines.
fn uncovered(found: &BTreeMap<String, BTreeSet<String>>, rows: &[Row]) -> Vec<String> {
    let mut out = Vec::new();
    for (file, spellings) in found {
        for s in spellings {
            let covered = rows
                .iter()
                .any(|r| &r.file == file && r.spellings.iter().any(|x| x == s));
            if !covered {
                out.push(format!("{file}: {s}"));
            }
        }
    }
    out
}

#[test]
fn every_macos_spelling_in_the_code_has_a_row() {
    let rows = parse_rows(&doc());
    assert!(rows.len() >= 15, "the table parsed: {rows:?}");
    let missing = uncovered(&scan(), &rows);
    assert!(
        missing.is_empty(),
        "a macOS dependency with no row in docs/platform-dependencies.md; add a row naming \
         the file, the symbol, what it does, what breaks on Linux and its replacement:\n{}",
        missing.join("\n")
    );
}

#[test]
fn every_row_still_matches_its_file() {
    let found = scan();
    let mut stale = Vec::new();
    for r in parse_rows(&doc()) {
        for s in &r.spellings {
            if !found.get(&r.file).is_some_and(|set| set.contains(s)) {
                stale.push(format!("{}: {s}", r.file));
            }
        }
    }
    assert!(
        stale.is_empty(),
        "rows naming spellings their file no longer has:\n{}",
        stale.join("\n")
    );
}

#[test]
fn every_row_names_a_replacement() {
    for r in parse_rows(&doc()) {
        assert!(
            ["portable", "mac-edge", "drop"]
                .iter()
                .any(|w| r.replacement.starts_with(w)),
            "{} has no replacement word: {}",
            r.file,
            r.replacement
        );
        assert!(!r.spellings.is_empty(), "{} lists no spelling", r.file);
    }
}

/// EACH DEPENDENCY THE K01 SPEC NAMES IS CAUGHT BY AT LEAST ONE SPELLING. Without this the
/// scan could be blind to a known dependency (the code calls tools by absolute path, so a
/// scan for `Command::new("sips")` alone finds nothing) and still pass.
#[test]
fn every_known_macos_dependency_is_caught() {
    let found = scan();
    let known: &[(&str, &str, &str)] = &[
        (
            "Core Graphics PDF rendering",
            "bridge/src/cgpdf.rs",
            "kind = \"framework\"",
        ),
        (
            "sips in attachments",
            "bridge/src/attachments.rs",
            "/usr/bin/",
        ),
        ("sips in vision", "bridge/src/vision.rs", "/usr/bin/"),
        (
            "afconvert (AFCONVERT)",
            "bridge/src/speech/decode.rs",
            "/usr/bin/",
        ),
        ("whisper-rs with Metal", "bridge/Cargo.toml", "\"metal\""),
        (
            "security (Keychain)",
            "bridge/src/quota.rs",
            "Command::new(\"security\")",
        ),
        (
            "sentinel launchctl",
            "bridge/src/sentinel/mod.rs",
            "\"launchctl\"",
        ),
        (
            "sentinel codesign",
            "bridge/src/sentinel/mod.rs",
            "\"codesign\"",
        ),
        (
            "sentinel resolve_bin",
            "bridge/src/sentinel/mod.rs",
            "resolve_bin(",
        ),
        ("plutil", "bridge/src/bin/jesse-transcribe.rs", "/usr/bin/"),
        ("sandbox-exec", "bridge/src/buildsvc.rs", "/usr/bin/"),
        (
            "~/Library defaults in config",
            "bridge/src/config.rs",
            "Library/",
        ),
        (
            "the iMCP server",
            "bridge/src/harness/claude_code.rs",
            "/Applications/",
        ),
    ];
    for (what, file, spelling) in known {
        assert!(
            found.get(*file).is_some_and(|s| s.contains(*spelling)),
            "{what} is no longer caught by `{spelling}` in {file}; add a spelling that catches it"
        );
    }
}

/// THE NEGATIVE: a new dependency in a file with no row is reported, and a commented-out one
/// is not. Exercised on synthetic text so it does not depend on the tree.
#[test]
fn a_new_dependency_without_a_row_is_reported() {
    let text = "fn export() {\n    let _ = Command::new(\"/usr/bin/osascript\");\n}\n\
                // Command::new(\"textutil\") in a comment is not a dependency\n";
    let m = matches_in(text, "//", RUST_SPELLINGS);
    assert_eq!(
        m,
        BTreeSet::from(["/usr/bin/".to_string()]),
        "the absolute path is caught and the comment is skipped"
    );
    let mut found = BTreeMap::new();
    found.insert("bridge/src/newexport.rs".to_string(), m);
    let missing = uncovered(&found, &parse_rows(&doc()));
    assert_eq!(
        missing,
        vec!["bridge/src/newexport.rs: /usr/bin/".to_string()]
    );
    let literal = matches_in("let t = \"osascript\";", "//", RUST_SPELLINGS);
    assert!(literal.contains("\"osascript\""));
}
