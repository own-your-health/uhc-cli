//! Black-box tests of the binary's surface. None reads a statement: each
//! renders help or info, or is refused by argument checks that run before
//! the import folder is read. The config directory is a temp dir, never
//! the developer's, and `UHC_IMPORT_DIR` is cleared.
//!
//! None reaches the real keychain or browser profile: `UHC_KEYCHAIN_SERVICE`
//! names a service that holds nothing (a missing item needs no approval),
//! and under it the binary never touches the kept profile. `XDG_DATA_HOME`,
//! where Linux keeps that profile, is a temp dir besides. `HOME` stays: on
//! macOS the keychain is found through it. No test stores a session.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn tmp() -> std::path::PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "uhc-cli-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The empty keychain service every test reads instead of `piekstra.uhc`.
const HERMETIC_SERVICE: &str = "piekstra.uhc.hermetic-tests";

fn hermetic(cmd: &mut Command) -> &mut Command {
    cmd.env("UHC_KEYCHAIN_SERVICE", HERMETIC_SERVICE)
        .env("XDG_DATA_HOME", tmp())
        .env_remove("UHC_CHROME")
}

fn uhc() -> Command {
    let mut cmd = Command::cargo_bin("uhc").unwrap();
    hermetic(&mut cmd)
        .env("XDG_CONFIG_HOME", tmp())
        .env_remove("UHC_IMPORT_DIR")
        .env_remove("NO_COLOR");
    cmd
}

fn error_of(out: &assert_cmd::assert::Assert) -> Value {
    serde_json::from_slice(&out.get_output().stdout).expect("one JSON error document on stdout")
}

#[test]
fn every_command_renders_help() {
    for args in [
        vec!["--help"],
        vec!["auth", "--help"],
        vec!["auth", "login", "--help"],
        vec!["auth", "status", "--help"],
        vec!["auth", "logout", "--help"],
        vec!["config", "--help"],
        vec!["config", "path", "--help"],
        vec!["config", "show", "--help"],
        vec!["config", "set", "--help"],
        vec!["config", "unset", "--help"],
        vec!["patients", "--help"],
        vec!["patients", "list", "--help"],
        vec!["records", "--help"],
        vec!["records", "export", "--help"],
        vec!["claims", "--help"],
        vec!["claims", "list", "--help"],
        vec!["claims", "get", "--help"],
        vec!["accumulators", "--help"],
        vec!["documents", "--help"],
        vec!["documents", "list", "--help"],
        vec!["documents", "download", "--help"],
        vec!["import", "--help"],
        vec!["import", "check", "--help"],
        vec!["self-update", "--help"],
        vec!["completions", "--help"],
        vec!["info", "--help"],
    ] {
        uhc().args(&args).assert().success();
    }
}

#[test]
fn help_says_unofficial_and_read_only() {
    uhc()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("Unofficial read-only"));
}

#[test]
fn version_prints() {
    uhc()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::starts_with("uhc "));
}

#[test]
fn info_declares_both_profiles_and_the_browser_sign_in() {
    let out = uhc().arg("info").assert().success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "cli-info/v1");
    assert_eq!(v["name"], "uhc");
    assert_eq!(v["spec"], "piekstra-cli/1");
    assert_eq!(v["profiles"], serde_json::json!(["health-records/v1", "documents/v1"]));
    // The files source needs no sign-in; the member site's is a browser one.
    assert_eq!(v["auth"]["required"], false);
    assert_eq!(v["auth"]["method"], "browser-session");
    assert_eq!(v["auth"]["login_hint"], "uhc auth login --browser");
}

#[test]
fn auth_status_when_nothing_is_stored() {
    let out = uhc().args(["auth", "status", "--json"]).assert().success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "auth-status/v1");
    assert_eq!(v["required"], false);
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["method"], "browser-session");
    assert_eq!(v["credential_in_keychain"], false);
    assert!(v.get("captured_at").is_none(), "{v}");
    assert!(v.get("verified").is_none(), "{v}");
    // Under the test service the real profile is not reported.
    assert!(v.get("browser_profile").is_none(), "{v}");
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("UHC_KEYCHAIN_SERVICE is set"), "{stderr}");
    uhc()
        .args(["auth", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Method:        browser-session"))
        .stdout(predicate::str::contains("Keychain:      empty"));
}

#[test]
fn auth_logout_when_nothing_is_stored() {
    let out = uhc().args(["auth", "logout", "--json"]).assert().success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "auth-logout/v1");
    assert_eq!(v["session_removed"], false);
    // The kept profile is left alone under the test service.
    assert!(v.get("browser_profile_removed").is_none(), "{v}");
    uhc()
        .args(["auth", "logout"])
        .assert()
        .success()
        .stderr(predicate::str::contains("no session was stored"))
        .stderr(predicate::str::contains("left alone"));
}

/// The usage rules of DESIGN.md §2.2: one credential source, decided before
/// any keychain or network work.
#[test]
fn auth_login_takes_exactly_one_source() {
    for args in [
        vec!["--browser", "--stdin"],
        vec!["--browser", "--from-env", "UHC_TEST_COOKIE"],
        vec!["--stdin", "--from-env", "UHC_TEST_COOKIE"],
        // No source at all.
        vec![],
    ] {
        let out = uhc().args(["auth", "login", "--json"]).args(&args).assert().code(2);
        assert_eq!(error_of(&out)["error"]["code"], "usage", "{args:?}");
    }
    for args in [
        vec!["--browser", "--op", "op://Example/Login/cookie"],
        vec!["--fresh-profile"],
        vec!["--fresh-profile", "--stdin"],
    ] {
        uhc().args(["auth", "login"]).args(&args).assert().code(2);
    }
}

/// A `--stdin` import checks what it read before anything is stored or
/// sent. `--no-verify` gets it past the unmapped check; nothing it reads
/// here is a header, so nothing is stored.
#[test]
fn auth_login_stdin_refuses_what_is_not_a_cookie_header() {
    for input in ["", "\n", "not a cookie header", "dummy_a=1\ndummy_b=2"] {
        let out = uhc()
            .args(["auth", "login", "--stdin", "--no-verify", "--json"])
            .write_stdin(input)
            .assert()
            .code(2);
        let e = error_of(&out);
        assert_eq!(e["error"]["code"], "usage", "{input:?}");
        assert!(!e.to_string().contains("dummy"), "{e}");
    }
}

/// While the member site's session check is not mapped, a checked sign-in
/// is refused before the keychain, stdin or Chrome. Once it is mapped this
/// would send a request, so it stops here.
#[test]
fn auth_login_refuses_a_check_that_is_not_mapped() {
    if uhc::site::SESSION_CHECK_URL.is_some() {
        return;
    }
    for source in ["--browser", "--stdin"] {
        let out = uhc()
            .args(["auth", "login", "--json", source])
            .write_stdin("dummy_a=1")
            .assert()
            .code(1);
        let message = error_of(&out)["error"]["message"].as_str().unwrap().to_string();
        assert!(message.contains(uhc::site::NOT_MAPPED), "{message}");
        assert!(message.contains("--no-verify"), "{message}");
    }
}

#[test]
fn a_command_without_an_import_folder_says_how_to_set_one() {
    for args in [
        vec!["patients", "list"],
        vec!["claims", "list"],
        vec!["accumulators"],
        vec!["documents", "list"],
        vec!["import", "check"],
        // -o, not --ndjson: --json with the stream is refused first.
        vec!["records", "export", "--patient", "x", "-o", "never-created"],
    ] {
        let out = uhc().args(&args).arg("--json").assert().code(2);
        let e = error_of(&out);
        assert!(
            e["error"]["message"]
                .as_str()
                .unwrap()
                .contains("config set import_dir"),
            "{args:?}: {e}"
        );
    }
}

#[test]
fn a_folder_that_is_not_there_is_not_found() {
    uhc()
        .args(["--dir", "/nonexistent/uhc-statements", "patients", "list"])
        .assert()
        .code(4);
    let file = tmp().join("file.txt");
    std::fs::write(&file, "x").unwrap();
    uhc()
        .arg("--dir")
        .arg(&file)
        .args(["patients", "list"])
        .assert()
        .code(2);
}

#[test]
fn the_import_folder_comes_from_the_environment_too() {
    let dir = tmp();
    let out = uhc()
        .env("UHC_IMPORT_DIR", &dir)
        .args(["patients", "list", "--json"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "patient-list/v1");
}

#[test]
fn config_set_show_unset() {
    let cfg = tmp();
    let dir = tmp();
    let run = |args: &[&str]| {
        let mut c = Command::cargo_bin("uhc").unwrap();
        hermetic(&mut c)
            .env("XDG_CONFIG_HOME", &cfg)
            .env_remove("UHC_IMPORT_DIR")
            .args(args);
        c
    };
    run(&["config", "set", "import_dir", dir.to_str().unwrap()])
        .assert()
        .success();
    let out = run(&["config", "show", "--json"]).assert().success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["import_dir"], dir.to_str().unwrap());
    run(&["patients", "list", "--json"]).assert().success();
    run(&["config", "unset", "import_dir"]).assert().success();
    run(&["patients", "list"]).assert().code(2);
    run(&["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("uhc"));
}

#[test]
fn config_refuses_unknown_keys_and_missing_folders() {
    uhc().args(["config", "set", "password", "x"]).assert().code(2);
    uhc().args(["config", "unset", "password"]).assert().code(2);
    uhc()
        .args(["config", "set", "import_dir", "/nonexistent/uhc-statements"])
        .assert()
        .code(2);
}

#[test]
fn records_export_checks_its_arguments_first() {
    let dir = tmp();
    // No destination.
    let out = uhc()
        .arg("--dir")
        .arg(&dir)
        .args(["records", "export", "--patient", "x", "--json"])
        .assert()
        .code(2);
    assert!(error_of(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .contains("--ndjson"));
    // A bad date, before the folder is read.
    uhc()
        .args([
            "records",
            "export",
            "--patient",
            "x",
            "--since",
            "03/01/2026",
            "--ndjson",
        ])
        .assert()
        .code(2);
    // Both destinations.
    uhc()
        .args(["records", "export", "--patient", "x", "--ndjson", "-o", "/tmp/x"])
        .assert()
        .code(2);
    // No patient.
    uhc().args(["records", "export", "--ndjson"]).assert().code(2);
}

#[test]
fn range_flags_are_checked() {
    uhc().args(["claims", "list", "--since", "yesterday"]).assert().code(2);
    uhc()
        .args(["claims", "list", "--since", "2026-05-01", "--until", "2026-04-01"])
        .assert()
        .code(2);
    uhc()
        .args(["documents", "list", "--until", "05/01/2026"])
        .assert()
        .code(2);
    uhc().args(["accumulators", "--year", "twenty"]).assert().code(2);
}

#[test]
fn documents_download_needs_an_id_or_all() {
    uhc().args(["documents", "download"]).assert().code(2);
    uhc()
        .args(["documents", "download", "abcdef", "--all"])
        .assert()
        .code(2);
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    uhc().arg("frobnicate").assert().code(2);
    uhc().args(["claims", "delete"]).assert().code(2);
}

#[test]
fn no_color_is_accepted_as_any_value() {
    uhc().env("NO_COLOR", "1").args(["auth", "status"]).assert().success();
    uhc().args(["--no-color", "info"]).assert().success();
}

#[test]
fn completions_render() {
    for shell in ["bash", "zsh", "fish"] {
        uhc()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(predicate::str::contains("uhc"));
    }
}

#[test]
fn there_is_no_write_command() {
    let out = uhc().arg("--help").assert().success();
    let help = String::from_utf8(out.get_output().stdout.clone())
        .unwrap()
        .to_lowercase();
    for verb in ["submit", "appeal", "pay ", "upload", "mcp"] {
        assert!(!help.contains(verb), "help mentions {verb}");
    }
}

#[test]
fn the_stream_is_the_output_so_json_with_ndjson_is_a_usage_error() {
    let out = uhc()
        .args(["--json", "records", "export", "--patient", "x", "--ndjson"])
        .assert()
        .code(2);
    assert_eq!(error_of(&out)["error"]["code"], "usage");
}
