//! Black-box tests of the binary's surface. None reads a statement or the
//! member site: each renders help or info, or is refused by argument checks
//! that run before a source is read. The config directory is a temp dir,
//! never the developer's, and `UHC_FILES`, `UHC_IMPORT_DIR` and `UHC_PROFILE` are
//! cleared.
//!
//! None reaches the real keychain or browser profile: `UHC_KEYCHAIN_SERVICE`
//! names a service that holds nothing (a missing item needs no approval),
//! and under it the binary never touches the kept profile. `XDG_DATA_HOME`,
//! where Linux keeps that profile, is a temp dir besides. `HOME` stays: on
//! macOS the keychain is found through it. No test stores a session, so no
//! test writes the vault's key or reads one: the vault reads its key only
//! when an entry exists (cli-common DESIGN.md §2.5).
//!
//! The temp config of [`uhc`] configures one profile, `adult1`.

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

/// A config directory whose config configures `profiles`.
fn config_with(profiles: &[&str]) -> std::path::PathBuf {
    let base = tmp();
    std::fs::create_dir_all(base.join("uhc")).unwrap();
    let names: serde_json::Map<String, Value> = profiles
        .iter()
        .map(|p| (p.to_string(), serde_json::json!({})))
        .collect();
    std::fs::write(
        base.join("uhc").join("config.json"),
        serde_json::json!({ "profiles": names }).to_string(),
    )
    .unwrap();
    base
}

fn uhc() -> Command {
    let mut cmd = Command::cargo_bin("uhc").unwrap();
    hermetic(&mut cmd)
        .env("XDG_CONFIG_HOME", config_with(&["adult1"]))
        .env_remove("UHC_IMPORT_DIR")
        .env_remove("UHC_FILES")
        .env_remove("UHC_PROFILE")
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
        vec!["auth", "reset", "--help"],
        vec!["profiles", "--help"],
        vec!["profiles", "list", "--help"],
        vec!["profiles", "add", "--help"],
        vec!["profiles", "remove", "--help"],
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
    // medrec passes --profile to a provider listing login-profiles among its
    // capabilities (medical-common DESIGN.md §1.1), and a sign-in is
    // required: without --files, every record command reads the site.
    let capabilities = v["capabilities"].as_array().unwrap();
    assert!(capabilities.contains(&serde_json::json!("login-profiles")), "{v}");
    assert!(capabilities.contains(&serde_json::json!("api")), "{v}");
    // A records store passes --files only to a provider listing this.
    assert!(capabilities.contains(&serde_json::json!("records-files")), "{v}");
    // A store can label one member of a profile (medical-common DESIGN.md §1).
    assert!(capabilities.contains(&serde_json::json!("patient-labels")), "{v}");
    assert_eq!(v["auth"]["required"], true);
    assert_eq!(v["auth"]["method"], "browser-session");
    assert_eq!(v["auth"]["login_hint"], "uhc auth login --browser --profile <NAME>");
}

#[test]
fn auth_status_when_nothing_is_stored() {
    let out = uhc()
        .args(["auth", "status", "--profile", "adult1", "--json"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "auth-status/v1");
    assert_eq!(v["required"], true);
    assert_eq!(v["authenticated"], false);
    assert_eq!(v["method"], "browser-session");
    assert_eq!(v["credential_in_keychain"], false);
    assert_eq!(v["account"], "adult1");
    assert!(v.get("captured_at").is_none(), "{v}");
    assert!(v.get("verified").is_none(), "{v}");
    // Under the test service the real profile is not reported.
    assert!(v.get("browser_profile").is_none(), "{v}");
    let stderr = String::from_utf8_lossy(&out.get_output().stderr);
    assert!(stderr.contains("UHC_KEYCHAIN_SERVICE is set"), "{stderr}");
    uhc()
        .env("UHC_PROFILE", "adult1")
        .args(["auth", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Method:        browser-session"))
        .stdout(predicate::str::contains("Keychain:      empty"));
}

/// A profile must be configured: an unknown one is exit 2 naming the
/// configured ones, none configured names `profiles add`, and a lone
/// profile is never picked for a command that names none.
#[test]
fn a_profile_must_be_configured() {
    let out = uhc()
        .args(["auth", "status", "--profile", "adult2", "--json"])
        .assert()
        .code(2);
    let e = error_of(&out);
    assert!(e["error"]["message"].as_str().unwrap().contains("adult1"), "{e}");
    let out = uhc().args(["auth", "status", "--json"]).assert().code(2);
    assert!(error_of(&out)["error"]["message"]
        .as_str()
        .unwrap()
        .contains("configured: adult1"));
    let out = uhc()
        .env("XDG_CONFIG_HOME", tmp())
        .args(["claims", "list", "--profile", "adult1", "--json"])
        .assert()
        .code(2);
    let e = error_of(&out);
    assert!(e.to_string().contains("uhc profiles add adult1"), "{e}");
}

#[test]
fn profiles_add_list_remove() {
    let cfg = tmp();
    let run = |args: &[&str]| {
        let mut c = uhc();
        c.env("XDG_CONFIG_HOME", &cfg).args(args);
        c
    };
    let list =
        |c: &mut Command| -> Value { serde_json::from_slice(&c.assert().success().get_output().stdout).unwrap() };
    assert_eq!(
        list(&mut run(&["profiles", "list", "--json"])),
        serde_json::json!({ "schema": "profile-list/v1", "profiles": [] })
    );
    for name in ["adult2", "adult1"] {
        let v = list(&mut run(&["profiles", "add", name, "--json"]));
        assert_eq!(
            v,
            serde_json::json!({ "schema": "profile-add/v1", "profile": name, "added": true })
        );
    }
    let v = list(&mut run(&["profiles", "add", "adult1", "--json"]));
    assert_eq!(v["added"], false);
    let v = list(&mut run(&["profiles", "list", "--json"]));
    assert_eq!(
        v["profiles"],
        serde_json::json!([{ "name": "adult1" }, { "name": "adult2" }])
    );
    run(&["profiles", "list"]).assert().success().stdout("adult1\nadult2\n");
    for bad in ["Adult1", "a b", "-x"] {
        run(&["profiles", "add", bad]).assert().code(2);
    }
    // Destructive: without a terminal it needs --force, decided first.
    run(&["profiles", "remove", "adult2"]).assert().code(6);
    run(&["profiles", "remove", "adult3", "--force"]).assert().code(4);
    let v = list(&mut run(&["profiles", "remove", "adult2", "--force", "--json"]));
    assert_eq!(v["schema"], "profile-remove/v1");
    assert_eq!(v["profile"], "adult2");
    assert_eq!(v["session_removed"], false);
    // The kept browser profile is left alone under the test service.
    assert!(v.get("browser_profile_removed").is_none(), "{v}");
    let v = list(&mut run(&["profiles", "list", "--json"]));
    assert_eq!(v["profiles"], serde_json::json!([{ "name": "adult1" }]));
    run(&["--profile", "adult1", "profiles", "list"]).assert().code(2);
}

/// `auth reset` clears every profile's session: without a terminal it
/// needs --force (exit 6), before the vault is opened, and it is no one
/// profile's.
#[test]
fn auth_reset_needs_force_without_a_terminal() {
    let out = uhc().args(["auth", "reset", "--json"]).assert().code(6);
    assert_eq!(error_of(&out)["error"]["code"], "confirmation_required");
    uhc()
        .args(["auth", "reset", "--force", "--profile", "adult1"])
        .assert()
        .code(2);
}

#[test]
fn auth_logout_when_nothing_is_stored() {
    let out = uhc()
        .args(["--profile", "adult1", "auth", "logout", "--json"])
        .assert()
        .success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["schema"], "auth-logout/v1");
    assert_eq!(v["session_removed"], false);
    // The kept profile is left alone under the test service.
    assert!(v.get("browser_profile_removed").is_none(), "{v}");
    uhc()
        .args(["auth", "logout", "--profile", "adult1"])
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
        let out = uhc()
            .args(["auth", "login", "--profile", "adult1", "--json"])
            .args(&args)
            .assert()
            .code(2);
        assert_eq!(error_of(&out)["error"]["code"], "usage", "{args:?}");
    }
    for args in [
        vec!["--browser", "--op", "op://Example/Login/cookie"],
        vec!["--fresh-profile"],
        vec!["--fresh-profile", "--stdin"],
    ] {
        uhc()
            .args(["auth", "login", "--profile", "adult1"])
            .args(&args)
            .assert()
            .code(2);
    }
}

/// The sign-in is one profile's: none named, a malformed name, or the
/// files source beside it is exit 2, before the keychain is read.
#[test]
fn auth_commands_take_one_well_formed_profile() {
    let too_long = "a".repeat(33);
    for args in [
        vec!["auth", "status"],
        vec!["auth", "logout"],
        vec!["auth", "login", "--stdin"],
        vec!["auth", "status", "--profile", "Adult1"],
        vec!["auth", "status", "--profile=-x"],
        vec!["auth", "status", "--profile", "a.b"],
        vec!["auth", "status", "--profile", &too_long],
        vec!["--files", "/tmp", "auth", "status", "--profile", "adult1"],
    ] {
        let out = uhc().args(&args).arg("--json").assert().code(2);
        assert_eq!(error_of(&out)["error"]["code"], "usage", "{args:?}");
    }
    uhc()
        .env("UHC_PROFILE", "Not Valid")
        .args(["auth", "status"])
        .assert()
        .code(2);
}

/// A `--stdin` import checks what it read before anything is stored or
/// sent. `--no-verify` gets it past the unmapped check; nothing it reads
/// here is a header, so nothing is stored.
#[test]
fn auth_login_stdin_refuses_what_is_not_a_cookie_header() {
    for input in ["", "\n", "not a cookie header", "dummy_a=1\ndummy_b=2"] {
        let out = uhc()
            .args([
                "auth",
                "login",
                "--profile",
                "adult1",
                "--stdin",
                "--no-verify",
                "--json",
            ])
            .write_stdin(input)
            .assert()
            .code(2);
        let e = error_of(&out);
        assert_eq!(e["error"]["code"], "usage", "{input:?}");
        assert!(!e.to_string().contains("dummy"), "{e}");
    }
}

/// `--user-agent` names the browser a pasted Cookie header came from, so
/// it goes with --stdin or --from-env, and they need it: the site's edge
/// refuses a request without a browser's.
#[test]
fn auth_login_user_agent_goes_with_a_pasted_session() {
    for args in [
        vec!["--browser", "--user-agent", "Mozilla/5.0 (test)"],
        vec!["--user-agent", "Mozilla/5.0 (test)"],
        vec!["--stdin"],
        vec!["--from-env", "UHC_TEST_COOKIE"],
    ] {
        uhc()
            .args(["auth", "login", "--profile", "adult1"])
            .args(&args)
            .assert()
            .code(2);
    }
}

/// `api` reads: another method is a usage error when the arguments parse,
/// before a profile or the vault is read; GET and POST take any case.
#[test]
fn api_takes_get_and_post_only() {
    for m in ["PUT", "DELETE", "PATCH", "HEAD"] {
        uhc().args(["--profile", "adult1", "api", m, "/x"]).assert().code(2);
    }
    let help = uhc().args(["api", "--help"]).assert().success();
    let text = String::from_utf8_lossy(&help.get_output().stdout).to_string();
    assert!(text.contains("[possible values: GET, POST]"), "{text}");
}

const EITHER_SOURCE: [&[&str]; 7] = [
    &["patients", "list"],
    &["claims", "list"],
    &["claims", "get", "cSYNC00000001"],
    &["accumulators"],
    &["documents", "list"],
    &["documents", "download", "--all", "-o", "never-created"],
    // -o, not --ndjson: --json with the stream is refused first.
    &["records", "export", "--patient", "x", "-o", "never-created"],
];

/// No flag never means the files, and a configured `import_dir` does not
/// either: a command that reads either source names one or is exit 2.
#[test]
fn a_command_that_reads_either_source_needs_one_named() {
    let cfg = tmp();
    let dir = tmp();
    let run = || {
        let mut c = uhc();
        c.env("XDG_CONFIG_HOME", &cfg);
        c
    };
    run()
        .args(["config", "set", "import_dir", dir.to_str().unwrap()])
        .assert()
        .success();
    for args in EITHER_SOURCE {
        let out = run().args(args).arg("--json").assert().code(2);
        let message = error_of(&out)["error"]["message"].as_str().unwrap().to_string();
        assert!(
            message.contains("--profile <NAME>") && message.contains("--files <DIR>"),
            "{args:?}: {message}"
        );
    }
}

/// Both sources named on the command line, or both through their
/// variables with neither flag, is no guess; a flag wins over the other
/// source's variable.
#[test]
fn one_source_at_a_time() {
    let dir = tmp();
    for args in EITHER_SOURCE {
        uhc()
            .args(["--profile", "adult1", "--files"])
            .arg(&dir)
            .args(args)
            .assert()
            .code(2);
        uhc()
            .env("UHC_PROFILE", "adult1")
            .env("UHC_IMPORT_DIR", &dir)
            .args(args)
            .assert()
            .code(2);
    }
    uhc()
        .env("UHC_PROFILE", "adult1")
        .arg("--files")
        .arg(&dir)
        .args(["patients", "list", "--json"])
        .assert()
        .success();
    uhc().args(["--profile", "Adult 1", "claims", "list"]).assert().code(2);
}

/// The commands that need no sign-in take no --profile; UHC_PROFILE in the
/// environment does not stop them.
#[test]
fn commands_without_a_sign_in_take_no_profile() {
    for args in [
        vec!["info"],
        vec!["import", "check"],
        vec!["config", "path"],
        vec!["completions", "zsh"],
        vec!["self-update", "--check"],
    ] {
        uhc().args(&args).args(["--profile", "adult1"]).assert().code(2);
    }
    uhc().env("UHC_PROFILE", "adult1").arg("info").assert().success();
}

#[test]
fn import_check_without_a_folder_says_how_to_set_one() {
    let out = uhc().args(["import", "check", "--json"]).assert().code(2);
    let e = error_of(&out);
    assert!(
        e["error"]["message"]
            .as_str()
            .unwrap()
            .contains("config set import_dir"),
        "{e}"
    );
}

#[test]
fn a_folder_that_is_not_there_is_not_found() {
    uhc()
        .args(["--dir", "/nonexistent/uhc-statements", "patients", "list"])
        .assert()
        .code(4);
    uhc()
        .args(["patients", "list", "--files", "/nonexistent/uhc-statements"])
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

/// UHC_FILES names the folder, and UHC_IMPORT_DIR, its older name, still
/// does; the two naming different folders is exit 2.
#[test]
fn the_import_folder_comes_from_the_environment_too() {
    let dir = tmp();
    for var in ["UHC_FILES", "UHC_IMPORT_DIR"] {
        let out = uhc()
            .env(var, &dir)
            .args(["patients", "list", "--json"])
            .assert()
            .success();
        let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
        assert_eq!(v["schema"], "patient-list/v1", "{var}");
    }
    uhc()
        .env("UHC_FILES", &dir)
        .env("UHC_IMPORT_DIR", &dir)
        .args(["patients", "list", "--json"])
        .assert()
        .success();
    uhc()
        .env("UHC_FILES", &dir)
        .env("UHC_IMPORT_DIR", tmp())
        .args(["patients", "list", "--json"])
        .assert()
        .code(2);
    uhc()
        .env("UHC_PROFILE", "adult1")
        .env("UHC_FILES", &dir)
        .args(["patients", "list", "--json"])
        .assert()
        .code(2);
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
            .env_remove("UHC_FILES")
            .env_remove("UHC_PROFILE")
            .args(args);
        c
    };
    run(&["config", "set", "import_dir", dir.to_str().unwrap()])
        .assert()
        .success();
    let out = run(&["config", "show", "--json"]).assert().success();
    let v: Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["import_dir"], dir.to_str().unwrap());
    run(&["import", "check", "--json"]).assert().success();
    run(&["config", "unset", "import_dir"]).assert().success();
    run(&["import", "check"]).assert().code(2);
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
        .arg("--files")
        .arg(&dir)
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
        .arg("--files")
        .arg(&dir)
        .args(["records", "export", "--patient", "x", "--ndjson", "-o", "/tmp/x"])
        .assert()
        .code(2);
    // No patient.
    uhc().args(["records", "export", "--ndjson"]).assert().code(2);
}

#[test]
fn range_flags_are_checked() {
    let dir = tmp();
    let files = || {
        let mut c = uhc();
        c.arg("--files").arg(&dir);
        c
    };
    for args in [
        vec!["claims", "list", "--since", "yesterday"],
        vec!["claims", "list", "--since", "2026-05-01", "--until", "2026-04-01"],
        vec!["documents", "list", "--until", "05/01/2026"],
    ] {
        let out = files().args(&args).arg("--json").assert().code(2);
        let message = error_of(&out)["error"]["message"].as_str().unwrap().to_string();
        assert!(!message.contains("--profile <NAME>"), "{args:?}: {message}");
    }
    // clap's own check.
    files().args(["accumulators", "--year", "twenty"]).assert().code(2);
}

#[test]
fn documents_download_needs_an_id_or_all() {
    let dir = tmp();
    uhc()
        .arg("--files")
        .arg(&dir)
        .args(["documents", "download"])
        .assert()
        .code(2);
    uhc()
        .arg("--files")
        .arg(&dir)
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
    uhc()
        .env("NO_COLOR", "1")
        .args(["auth", "status", "--profile", "adult1"])
        .assert()
        .success();
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

/// `patients label` (medical-common DESIGN.md §1) takes a label or
/// `--clear`, exactly one, and `--profile`, never `--files`: all exit 2,
/// before any request or secret.
#[test]
fn patients_label_takes_a_label_or_clear_and_a_profile() {
    for args in [
        &["--profile", "adult1", "patients", "label", "--patient", "x"][..],
        &[
            "--profile",
            "adult1",
            "patients",
            "label",
            "--patient",
            "x",
            "Robin",
            "--clear",
        ][..],
        &["patients", "label", "--patient", "x", "Robin"][..],
        &["--files", "never-read", "patients", "label", "--patient", "x", "Robin"][..],
    ] {
        let out = uhc().args(args).assert().code(2);
        let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
        assert!(!stderr.is_empty(), "{args:?}");
    }
    uhc().args(["patients", "label", "--help"]).assert().success();
}

/// A `--json` error carries the next step as `remedy`, apart from the
/// message (cli-common v0.33.4), and repeats no `--patient` value.
#[test]
fn a_json_error_carries_its_remedy() {
    let out = uhc()
        .args([
            "--json",
            "--profile",
            "adult1",
            "patients",
            "label",
            "--patient",
            "mSYNTHETIC01",
            "Robin",
        ])
        .assert()
        .code(4);
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();
    assert_eq!(v["error"]["code"], "not_found", "{v}");
    assert_eq!(
        v["error"]["remedy"], "`uhc --profile adult1 patients list` reads them from the site",
        "{v}"
    );
    assert!(!v.to_string().contains("mSYNTHETIC01"), "{v}");
}
