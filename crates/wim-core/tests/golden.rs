//! The golden test runner: every TOML file under `tests/golden/cases/` is one case of
//! "start from this text, type these keys, end up with that text".
//!
//! Adding a case is adding a file — see `tests/golden/README.md` for the format. All cases
//! run inside one test so that a single run reports every failure at once.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use wim_core::{Editor, Position};

/// One golden case, as written in a TOML file.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    /// What the case is about. Only used in failure output.
    #[serde(default)]
    name: Option<String>,
    /// The buffer before the keys are typed. The cursor always starts at line 0, column 0.
    input: String,
    /// The keys to type, in the notation `wim_core::parse_keys` reads.
    keys: String,
    /// The buffer the keys should leave behind.
    expected: String,
    /// The cursor the keys should leave behind, `[line, col]`. Not checked when absent.
    #[serde(default)]
    expected_cursor: Option<[usize; 2]>,
}

#[test]
fn golden_cases_hold() {
    let directory = cases_directory();
    let files = case_files(&directory);
    assert!(
        !files.is_empty(),
        "no golden cases found in {}",
        directory.display()
    );

    let failures: Vec<String> = files.iter().filter_map(|file| check(file)).collect();
    assert!(
        failures.is_empty(),
        "{} of {} golden case(s) failed:\n\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
}

fn cases_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/cases")
}

/// The case files, sorted so that failures are reported in a stable order.
fn case_files(directory: &Path) -> Vec<PathBuf> {
    let entries = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", directory.display()));
    let mut files: Vec<PathBuf> = entries
        .map(|entry| entry.expect("cannot read a directory entry").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .collect();
    files.sort();
    files
}

/// Runs one case, returning the report to print when it fails.
fn check(file: &Path) -> Option<String> {
    let name = file
        .file_name()
        .expect("a case path always ends in a file name")
        .to_string_lossy()
        .into_owned();

    let text = match fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) => return Some(format!("{name}: cannot be read: {error}")),
    };
    let case: Case = match toml::from_str(&text) {
        Ok(case) => case,
        Err(error) => return Some(format!("{name}: is not a valid case: {error}")),
    };
    // Windows checkouts turn the line breaks inside the TOML strings into CRLF, which is
    // the checkout's doing rather than the case's.
    let input = case.input.replace("\r\n", "\n");
    let expected = case.expected.replace("\r\n", "\n");

    let mut editor = Editor::new(&input);
    if let Err(error) = editor.handle_keys(&case.keys) {
        return Some(report(
            &name,
            &case,
            &format!("the keys do not parse: {error}"),
        ));
    }

    let mut problems = String::new();
    let actual = editor.text();
    if actual != expected {
        problems.push_str("  text:\n");
        problems.push_str(&diff(&expected, &actual));
    }
    if let Some([line, col]) = case.expected_cursor {
        let wanted = Position::new(line, col);
        if editor.cursor() != wanted {
            let actual = editor.cursor();
            let _ = writeln!(
                problems,
                "  cursor:\n    - expected [{}, {}]\n    + actual   [{}, {}]",
                wanted.line, wanted.col, actual.line, actual.col
            );
        }
    }
    if problems.is_empty() {
        return None;
    }
    Some(report(&name, &case, &problems))
}

fn report(name: &str, case: &Case, problems: &str) -> String {
    let mut report = name.to_owned();
    if let Some(title) = &case.name {
        let _ = write!(report, " — {title}");
    }
    let _ = writeln!(report, "\n  keys: {}", case.keys);
    report.push_str(problems);
    report
}

/// A line by line diff, with every line quoted so that trailing blanks and the empty line a
/// trailing newline leaves behind are visible.
///
/// The comparison walks both texts in step: golden cases are a handful of lines, so a line
/// that moved shows up as the lines around it differing, which reads fine at that size.
fn diff(expected: &str, actual: &str) -> String {
    let expected: Vec<&str> = expected.split('\n').collect();
    let actual: Vec<&str> = actual.split('\n').collect();
    let mut diff = String::new();
    for index in 0..expected.len().max(actual.len()) {
        match (expected.get(index), actual.get(index)) {
            (Some(expected), Some(actual)) if expected == actual => {
                let _ = writeln!(diff, "      {expected:?}");
            }
            (expected, actual) => {
                if let Some(expected) = expected {
                    let _ = writeln!(diff, "    - {expected:?}");
                }
                if let Some(actual) = actual {
                    let _ = writeln!(diff, "    + {actual:?}");
                }
            }
        }
    }
    diff
}
