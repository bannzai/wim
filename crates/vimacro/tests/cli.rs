//! End-to-end tests of the `vimacro` binary.
//!
//! Every example the README shows has a test of its own here, named after it, so that the
//! README cannot drift away from what the CLI does.

use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use tempfile::TempDir;

/// The binary under test, run from a directory of its own so that relative paths in the
/// arguments mean what the test wrote there.
fn vimacro(directory: &TempDir) -> Command {
    let mut command = Command::cargo_bin("vimacro").expect("the binary should be built");
    command.current_dir(directory.path());
    command
}

/// A directory holding `files`, given as `(name, content)` pairs.
fn directory(files: &[(&str, &str)]) -> TempDir {
    let directory = TempDir::new().expect("a temporary directory should be available");
    for (name, content) in files {
        std::fs::write(directory.path().join(name), content).expect("the file should be written");
    }
    directory
}

fn read(directory: &TempDir, name: &str) -> String {
    std::fs::read_to_string(directory.path().join(name)).expect("the file should be readable")
}

#[test]
fn readme_repeat_to_eof_changes_the_first_word_of_every_line() {
    let directory = directory(&[("notes.txt", "alpha one\nbravo two\ncharlie three\n")]);
    vimacro(&directory)
        .args(["ciwfoo<Esc>", "--repeat-to-eof", "notes.txt"])
        .assert()
        .success()
        .stdout("foo one\nfoo two\nfoo three\n");
}

#[test]
fn readme_global_appends_a_semicolon_to_every_import() {
    let directory = directory(&[("app.ts", "import a\nimport b\nconst x = 1\n")]);
    vimacro(&directory)
        .args(["--global", "^import", "A;<Esc>", "app.ts"])
        .assert()
        .success()
        .stdout("import a;\nimport b;\nconst x = 1\n");
}

#[test]
fn readme_ex_runs_a_substitution_over_the_whole_buffer() {
    let directory = directory(&[("notes.txt", "foo and foo\nfoo\n")]);
    vimacro(&directory)
        .args(["--ex", "%s/foo/bar/g", "notes.txt"])
        .assert()
        .success()
        .stdout("bar and bar\nbar\n");
}

#[test]
fn readme_reads_standard_input_and_writes_standard_output() {
    let directory = directory(&[]);
    vimacro(&directory)
        .arg("A!<Esc>")
        .write_stdin("alpha\nbravo\n")
        .assert()
        .success()
        .stdout("alpha!\nbravo\n");
}

#[test]
fn readme_in_place_runs_an_ex_command_over_several_files() {
    let directory = directory(&[("a.md", "# title\nkeep a\n"), ("b.md", "keep b\n# note\n")]);
    vimacro(&directory)
        .args(["-i", "--ex", "g/^#/d", "a.md", "b.md"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(read(&directory, "a.md"), "keep a\n");
    assert_eq!(read(&directory, "b.md"), "keep b\n");
}

#[test]
fn readme_keys_run_after_the_ex_command_they_are_combined_with() {
    let directory = directory(&[("notes.txt", "foo one\nfoo two\n")]);
    vimacro(&directory)
        .args(["--ex", "%s/foo/bar/g", "--keys", "ggA!<Esc>", "notes.txt"])
        .assert()
        .success()
        .stdout("bar one!\nbar two\n");
}

#[test]
fn a_dash_names_standard_input() {
    let directory = directory(&[]);
    vimacro(&directory)
        .args(["A!<Esc>", "-"])
        .write_stdin("alpha\n")
        .assert()
        .success()
        .stdout("alpha!\n");
}

#[test]
fn a_single_file_goes_to_standard_output_and_is_left_alone() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["A!<Esc>", "notes.txt"])
        .assert()
        .success()
        .stdout("alpha!\n");
    assert_eq!(read(&directory, "notes.txt"), "alpha\n");
}

#[test]
fn in_place_writes_the_result_back_to_the_file() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["--in-place", "A!<Esc>", "notes.txt"])
        .assert()
        .success()
        .stdout("");
    assert_eq!(read(&directory, "notes.txt"), "alpha!\n");
}

#[test]
fn the_cursor_starts_at_the_first_line_of_every_file() {
    let directory = directory(&[("a.txt", "alpha\n"), ("b.txt", "bravo\n")]);
    vimacro(&directory)
        .args(["-i", "A!<Esc>", "a.txt", "b.txt"])
        .assert()
        .success();
    assert_eq!(read(&directory, "a.txt"), "alpha!\n");
    assert_eq!(read(&directory, "b.txt"), "bravo!\n");
}

#[test]
fn several_files_without_in_place_are_refused_rather_than_run_together() {
    let directory = directory(&[("a.txt", "alpha\n"), ("b.txt", "bravo\n")]);
    vimacro(&directory)
        .args(["A!<Esc>", "a.txt", "b.txt"])
        .assert()
        .failure()
        .stderr(contains("--in-place"));
}

#[test]
fn in_place_is_refused_for_standard_input() {
    let directory = directory(&[]);
    vimacro(&directory)
        .args(["-i", "A!<Esc>"])
        .write_stdin("alpha\n")
        .assert()
        .failure()
        .stderr(contains("standard input"));
}

#[test]
fn the_keys_of_a_repeat_need_no_trailing_motion() {
    let directory = directory(&[("notes.txt", "alpha one\nbravo two\n")]);
    // The example in the issue ends its macro with `j`, the way a Vim macro walks the file
    // itself. vimacro puts the cursor on the next line either way, so the `j` changes
    // nothing.
    for keys in ["ciwfoo<Esc>", "ciwfoo<Esc>j"] {
        vimacro(&directory)
            .args([keys, "--repeat-to-eof", "notes.txt"])
            .assert()
            .success()
            .stdout("foo one\nfoo two\n");
    }
}

#[test]
fn a_repeat_that_takes_lines_away_carries_on_at_the_same_line() {
    let directory = directory(&[("notes.txt", "alpha\nbravo\ncharlie\n")]);
    vimacro(&directory)
        .args(["--repeat-to-eof", "--keys", "dd", "notes.txt"])
        .assert()
        .success()
        .stdout("");
}

#[test]
fn a_repeat_that_adds_lines_steps_over_what_it_added() {
    let directory = directory(&[("notes.txt", "alpha\nbravo\n")]);
    vimacro(&directory)
        .args(["ox<Esc>", "--repeat-to-eof", "notes.txt"])
        .assert()
        .success()
        .stdout("alpha\nx\nbravo\nx\n");
}

#[test]
fn a_repeat_that_both_takes_a_line_away_and_adds_one_runs_over_the_lines_it_was_given() {
    let directory = directory(&[("notes.txt", "alpha\nbravo\ncharlie\n")]);
    // The line count is where it started after every run of the macro, so nothing but
    // tracking the lines themselves keeps the walk off the ones the macro wrote.
    vimacro(&directory)
        .args(["ddox<Esc>", "--repeat-to-eof", "notes.txt"])
        .assert()
        .success()
        .stdout("x\nx\nx\n");
}

#[test]
fn a_global_pattern_that_matches_nothing_is_reported() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["--global", "^import", "A;<Esc>", "notes.txt"])
        .assert()
        .failure()
        .stderr(contains("notes.txt: pattern not found: ^import"))
        // The text still goes to standard output, so that a pipe keeps carrying it.
        .stdout("alpha\n");
}

#[test]
fn a_file_whose_run_was_rejected_keeps_the_text_it_had() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["-i", "--global", "^import", "A;<Esc>", "notes.txt"])
        .assert()
        .failure();
    assert_eq!(read(&directory, "notes.txt"), "alpha\n");
}

#[test]
fn a_slash_in_a_global_pattern_needs_no_escaping() {
    let directory = directory(&[("notes.txt", "/usr/bin\nalpha\n")]);
    vimacro(&directory)
        .args(["--global", "^/usr/", "A!<Esc>", "notes.txt"])
        .assert()
        .success()
        .stdout("/usr/bin!\nalpha\n");
}

#[test]
fn an_ex_command_may_be_written_with_its_leading_colon() {
    let directory = directory(&[("notes.txt", "foo\n")]);
    vimacro(&directory)
        .args(["--ex", ":%s/foo/bar/g", "notes.txt"])
        .assert()
        .success()
        .stdout("bar\n");
}

#[test]
fn an_ex_command_carries_the_key_notation_of_a_norm() {
    let directory = directory(&[("notes.txt", "import a\nalpha\n")]);
    vimacro(&directory)
        .args(["--ex", "g/^import/norm A;<Esc>", "notes.txt"])
        .assert()
        .success()
        .stdout("import a;\nalpha\n");
}

#[test]
fn a_quit_ends_the_run_over_that_input() {
    let directory = directory(&[("notes.txt", "alpha\nbravo\n")]);
    vimacro(&directory)
        .args(["A!<Esc>:q<CR>jA?<Esc>", "notes.txt"])
        .assert()
        .success()
        .stdout("alpha!\nbravo\n");
}

#[test]
fn a_write_names_a_further_file_to_write_the_result_to() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["-i", "A!<Esc>:w copy.txt<CR>", "notes.txt"])
        .assert()
        .success();
    assert_eq!(read(&directory, "notes.txt"), "alpha!\n");
    assert_eq!(read(&directory, "copy.txt"), "alpha!\n");
}

#[test]
fn a_write_without_in_place_leaves_the_file_system_alone() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["A!<Esc>:w copy.txt<CR>", "notes.txt"])
        .assert()
        .success()
        .stdout("alpha!\n");
    assert_eq!(read(&directory, "notes.txt"), "alpha\n");
    assert!(!Path::new(&directory.path().join("copy.txt")).exists());
}

#[test]
fn crlf_line_endings_are_written_back_as_they_came() {
    let directory = directory(&[("notes.txt", "alpha\r\nbravo\r\n")]);
    vimacro(&directory)
        .args(["-i", "--repeat-to-eof", "--keys", "A!<Esc>", "notes.txt"])
        .assert()
        .success();
    assert_eq!(read(&directory, "notes.txt"), "alpha!\r\nbravo!\r\n");
}

#[test]
fn a_file_that_mixes_line_endings_keeps_the_endings_it_came_with() {
    let directory = directory(&[("notes.txt", "alpha\r\nbravo\ncharlie\r\n")]);
    vimacro(&directory)
        .args(["-i", "--ex", "w", "notes.txt"])
        .assert()
        .success();
    assert_eq!(
        std::fs::read(directory.path().join("notes.txt")).expect("the file should be readable"),
        b"alpha\r\nbravo\ncharlie\r\n",
        "a run that edited nothing rewrites nothing, byte for byte"
    );
}

#[test]
fn in_place_sends_the_buffer_to_standard_output_when_the_run_was_rejected() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["-i", "--global", "^import", "A;<Esc>", "notes.txt"])
        .assert()
        .failure()
        // The file keeps the text it had, and the pipe keeps carrying it.
        .stdout("alpha\n");
    assert_eq!(read(&directory, "notes.txt"), "alpha\n");
}

#[cfg(unix)]
#[test]
fn in_place_leaves_a_file_the_permissions_it_had() {
    use std::os::unix::fs::PermissionsExt;

    let directory = directory(&[("run.sh", "echo alpha\n")]);
    let path = directory.path().join("run.sh");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("the permissions should be settable");
    vimacro(&directory)
        .args(["-i", "A!<Esc>", "run.sh"])
        .assert()
        .success();
    assert_eq!(read(&directory, "run.sh"), "echo alpha!\n");
    assert_eq!(
        std::fs::metadata(&path)
            .expect("the file should be there")
            .permissions()
            .mode()
            & 0o777,
        0o755,
        "a script that was runnable before the run still is"
    );
}

#[cfg(unix)]
#[test]
fn in_place_writes_through_a_symbolic_link_to_the_file_it_names() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    let link = directory.path().join("link.txt");
    std::os::unix::fs::symlink("notes.txt", &link).expect("the link should be made");
    vimacro(&directory)
        .args(["-i", "A!<Esc>", "link.txt"])
        .assert()
        .success();
    assert_eq!(
        read(&directory, "notes.txt"),
        "alpha!\n",
        "the file the link names is the one the run read, and the one it writes"
    );
    assert!(
        std::fs::symlink_metadata(&link)
            .expect("the link should be there")
            .file_type()
            .is_symlink(),
        "the link is still a link rather than a file of its own"
    );
}

#[cfg(unix)]
#[test]
fn in_place_leaves_a_file_that_cannot_be_written_to_alone() {
    use std::os::unix::fs::PermissionsExt;

    let directory = directory(&[("notes.txt", "alpha\n")]);
    let path = directory.path().join("notes.txt");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
        .expect("the permissions should be settable");
    vimacro(&directory)
        .args(["-i", "A!<Esc>", "notes.txt"])
        .assert()
        .failure()
        // The directory is writable, so nothing but asking the file itself stops the write.
        .stderr(contains("notes.txt:"));
    assert_eq!(read(&directory, "notes.txt"), "alpha\n");
}

#[cfg(unix)]
#[test]
fn a_file_named_in_bytes_that_are_not_text_is_read_and_written() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let directory = directory(&[]);
    let name = OsStr::from_bytes(b"notes-\xff.txt");
    let path = directory.path().join(name);
    if std::fs::write(&path, "alpha\n").is_err() {
        // Linux holds a file name as bytes, and clap is what would refuse this one; macOS
        // holds it as text and refuses it itself, which leaves nothing here to test.
        return;
    }
    vimacro(&directory)
        .args([
            OsStr::new("-i"),
            OsStr::new("--keys"),
            OsStr::new("A!<Esc>"),
        ])
        .arg(name)
        .assert()
        .success();
    assert_eq!(
        std::fs::read_to_string(&path).expect("the file should be readable"),
        "alpha!\n"
    );
}

#[cfg(unix)]
#[test]
fn a_key_sequence_that_is_not_text_is_refused() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .arg(OsStr::from_bytes(b"A\xff<Esc>"))
        .arg("notes.txt")
        .assert()
        .failure()
        .stderr(contains("the key sequence is not text"));
}

#[test]
fn a_key_sequence_that_does_not_parse_is_reported_before_a_file_is_touched() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["-i", "a<Escape>", "notes.txt"])
        .assert()
        .failure()
        .stderr(contains("the key sequence does not parse"));
    assert_eq!(read(&directory, "notes.txt"), "alpha\n");
}

#[test]
fn keys_of_a_global_that_do_not_parse_are_reported_before_a_file_is_touched() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["-i", "--global", "^alpha", "A;<Escape>", "notes.txt"])
        .assert()
        .failure()
        .stderr(contains("--global was given keys that do not parse"));
    assert_eq!(read(&directory, "notes.txt"), "alpha\n");
}

#[test]
fn a_file_that_cannot_be_read_is_reported_with_its_name() {
    let directory = directory(&[]);
    vimacro(&directory)
        .args(["A!<Esc>", "missing.txt"])
        .assert()
        .failure()
        .stderr(contains("vimacro: missing.txt:"));
}

#[test]
fn a_run_with_nothing_to_do_is_refused() {
    let directory = directory(&[]);
    vimacro(&directory)
        .assert()
        .failure()
        .stderr(contains("there is nothing to run"));
}

#[test]
fn a_repeat_without_a_key_sequence_is_refused() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args(["--repeat-to-eof", "--ex", "%s/a/b/", "notes.txt"])
        .assert()
        .failure()
        .stderr(contains("--repeat-to-eof"));
}

#[test]
fn a_key_sequence_and_a_global_are_refused_together() {
    let directory = directory(&[("notes.txt", "alpha\n")]);
    vimacro(&directory)
        .args([
            "--keys",
            "A!<Esc>",
            "--global",
            "^alpha",
            "A;<Esc>",
            "notes.txt",
        ])
        .assert()
        .failure()
        .stderr(contains("cannot be used with"));
}

#[test]
fn a_rejected_key_is_reported_once_and_ends_the_run_over_that_line() {
    let directory = directory(&[("notes.txt", "alpha\nbravo\n")]);
    vimacro(&directory)
        .args(["--repeat-to-eof", "--keys", "<C-x>A!<Esc>", "notes.txt"])
        .assert()
        .failure()
        .stderr(contains("<C-x> does nothing").count(1))
        .stdout("alpha\nbravo\n");
}

#[test]
fn the_help_describes_how_a_repeat_walks_the_buffer() {
    let directory = directory(&[]);
    vimacro(&directory)
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("taken before anything runs").and(contains("no trailing 'j'")));
}
