//! The repository carries no personal data; runtime output may.
//!
//! Walks every tracked file (`git ls-files`) for the *shapes* personal data
//! takes (phone numbers, long id numbers, street addresses, email
//! addresses) rather than a list of real values, which would publish what
//! it exists to keep out. Then checks the fixtures positively: every
//! identity-bearing field a statement prints holds one of the dummy shapes
//! `tests/fixtures/README.md` lists.

use std::process::Command;

fn root() -> &'static str {
    env!("CARGO_MANIFEST_DIR")
}

fn tracked_files() -> Vec<String> {
    let out = Command::new("git")
        .args(["ls-files"])
        .current_dir(root())
        .output()
        .expect("git ls-files");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect()
}

fn digits_run(s: &str, min: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for c in s.chars().chain(std::iter::once(' ')) {
        if c.is_ascii_digit() {
            cur.push(c);
        } else {
            if cur.len() >= min {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    out
}

/// A digit run is a dummy when it is all one digit, zeros then one digit
/// (`00000001`), or a 555 phone number: `555…`, or any area code with the
/// 555-0100 to 555-0199 lines reserved for fiction.
fn is_dummy_number(n: &str) -> bool {
    let first = n.chars().next().unwrap();
    let head = &n[..n.len() - 1];
    let local = n.strip_prefix('1').filter(|r| r.len() == 10).unwrap_or(n);
    let fictional = local.len() == 10 && &local[3..8] == "55501";
    n.chars().all(|c| c == first)
        || head.chars().all(|c| c == '0')
        || n.starts_with("555")
        || n.starts_with("1555")
        || fictional
}

const PUBLIC_EMAIL_DOMAINS: &[&str] = &["example.com", "uhc.com", "optum.com", "flex.optum.com"];

/// The line with every hex digest blanked: a run of 32 or more lowercase hex
/// digits with a letter among them, such as a pinned SHA-256. Its digit runs
/// are not id numbers. A shorter run, or one of digits only, stays.
fn blank_digests(line: &str) -> String {
    fn flush(run: &mut String, out: &mut String) {
        if run.len() >= 32 && run.bytes().any(|b| b.is_ascii_lowercase()) {
            out.extend(std::iter::repeat_n(' ', run.len()));
        } else {
            out.push_str(run);
        }
        run.clear();
    }
    let mut out = String::with_capacity(line.len());
    let mut run = String::new();
    for c in line.chars() {
        if c.is_ascii_digit() || ('a'..='f').contains(&c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// The personal-data shapes on one line, each as `kind value`.
fn scan_line(line: &str) -> Vec<String> {
    let mut findings = Vec::new();
    let numbers = blank_digests(line);
    let phoneish = numbers.replace(['(', ')', '-', '.', ' '], "");
    for n in digits_run(&numbers, 7)
        .into_iter()
        .chain(digits_run(&phoneish, 10).into_iter().filter(|n| n.len() <= 11))
    {
        if !is_dummy_number(&n) {
            findings.push(format!("number {n}"));
        }
    }
    for word in line.split(|c: char| c.is_whitespace() || "\"'<>(),;`".contains(c)) {
        if let Some((local, domain)) = word.split_once('@') {
            let domain = domain.trim_end_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-'));
            let ok = local.is_empty()
                || !domain.contains('.')
                || PUBLIC_EMAIL_DOMAINS.contains(&domain)
                || domain.ends_with(".example")
                || domain.ends_with("users.noreply.github.com");
            if !ok {
                findings.push(format!("email {word}"));
            }
        }
    }
    let lower = line.to_ascii_lowercase();
    for suffix in [" way", " st", " ave", " blvd", " dr", " ln", " ct", " rd"] {
        if let Some(pos) = lower.find(suffix) {
            let has_number = lower[..pos]
                .split_whitespace()
                .rev()
                .take(4)
                .any(|w| w.len() >= 3 && w.chars().all(|c| c.is_ascii_digit()));
            if has_number && !lower.contains("example st") {
                findings.push(format!("address-like `{}`", line.trim()));
            }
        }
    }
    findings
}

/// `<commit>:<path>:<rule>:<line>`: a 40-character hex commit, a path with
/// no space, a lowercase rule id and a line number.
fn is_fingerprint(line: &str) -> bool {
    let parts: Vec<&str> = line.split(':').collect();
    let [commit, path, rule, number] = parts.as_slice() else {
        return false;
    };
    commit.len() == 40
        && commit.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && !path.is_empty()
        && !path.contains(char::is_whitespace)
        && !rule.is_empty()
        && rule.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
}

#[test]
fn a_fingerprint_is_only_a_fingerprint() {
    assert!(is_fingerprint(
        "865517eb837b9fb4fc7d1983f6cc1de7376015f8:src/portal.rs:generic-api-key:1205"
    ));
    assert!(!is_fingerprint(
        "865517eb837b9fb4fc7d1983f6cc1de7376015f8:src/portal.rs:generic-api-key:1205 note"
    ));
    assert!(!is_fingerprint("call 5551234567"));
    assert!(!is_fingerprint("865517e:src/portal.rs:generic-api-key:1205"));
}

#[test]
fn tracked_files_carry_no_personal_data() {
    let files = tracked_files();
    if files.is_empty() {
        // Not a git checkout (a packaged crate): nothing to scan.
        return;
    }
    let mut findings = Vec::new();
    for file in files {
        if file.ends_with(".lock") || file.starts_with("LICENSE") || file == "tests/no_personal_data.rs" {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(format!("{}/{file}", root())) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            // A gitleaks fingerprint's commit hash can hold a phone-length
            // digit run. Such a line is checked for that shape instead, so
            // nothing else can ride along in the file uncommented.
            if file == ".gitleaksignore" && !line.starts_with('#') {
                if !is_fingerprint(line) {
                    findings.push(format!("{file}:{}: not a gitleaks fingerprint", i + 1));
                }
                continue;
            }
            for f in scan_line(line) {
                findings.push(format!("{file}:{}: {f}", i + 1));
            }
        }
    }
    assert!(
        findings.is_empty(),
        "personal-data shapes in tracked files:\n{}",
        findings.join("\n")
    );
}

#[test]
fn no_tracked_file_is_a_raw_capture_or_a_statement() {
    for file in tracked_files() {
        let lower = file.to_ascii_lowercase();
        for ext in [".pdf", ".har", ".csv", ".xlsx", ".raw.json", ".raw.html", ".ndjson"] {
            assert!(
                !lower.ends_with(ext),
                "{file} is tracked; real statements and captures never are"
            );
        }
    }
}

const PEOPLE: &[&str] = &["ALEX", "SAM", "JORDAN"];

fn is_dummy_name(full: &str) -> bool {
    match full.split_whitespace().collect::<Vec<_>>().as_slice() {
        [first, "EXAMPLE"] | [first] => PEOPLE.contains(first),
        _ => false,
    }
}

fn is_dummy_org(name: &str) -> bool {
    // A statement prints a pharmacy benefit's claim as "Provider: Pharmacy".
    name == "Pharmacy"
        || ["EXAMPLE", "SAMPLE", "TEST"]
            .iter()
            .any(|w| name.split_whitespace().any(|x| x == *w))
}

/// `prefix` then exactly `digits` digits.
fn shaped(value: &str, prefix: &str, digits: usize) -> bool {
    value
        .strip_prefix(prefix)
        .is_some_and(|d| d.len() == digits && d.chars().all(|c| c.is_ascii_digit()))
}

/// The value printed after `label` on a line, up to a two-space gap.
fn after<'a>(line: &'a str, label: &str) -> Option<&'a str> {
    let i = line.find(label)?;
    let rest = line[i + label.len()..].trim_start();
    Some(rest.split("  ").next().unwrap_or("").trim())
}

#[test]
fn identity_fields_in_fixtures_use_dummy_values() {
    let dir = format!("{}/tests/fixtures/eob", root());
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.display().to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.trim_start()
                .starts_with("SYNTHETIC TEST DATA - NOT A REAL STATEMENT"),
            "{name} does not open with the synthetic marker"
        );
        for line in text.lines() {
            let mut check = |label: &str, ok: &dyn Fn(&str) -> bool| {
                if let Some(v) = after(line, label) {
                    if !v.is_empty() {
                        assert!(ok(v), "{name}: `{label}` holds `{v}`, not a dummy");
                        checked += 1;
                    }
                }
            };
            check("Member/Patient:", &is_dummy_name);
            check("Member ID:", &|v| shaped(v, "SYNM", 6));
            check("Group number:", &|v| shaped(v, "SYNG", 4));
            check("Group name:", &is_dummy_org);
            check("Claim number:", &|v| shaped(v, "SYNC", 8));
            // A pharmacy claim's account number is its pharmacy claim number.
            check("Patient account number:", &|v| {
                shaped(v, "SYNA", 4) || (v.bytes().all(|b| b.is_ascii_digit()) && is_dummy_number(v))
            });
            check("Provider:", &is_dummy_org);
            check("Claim detail for", &|v| {
                PEOPLE.contains(&v.trim_end_matches(" (continued)").trim_end_matches(" continued"))
            });
            check("Balances for", &|v| PEOPLE.contains(&v) || v == "FAMILY");
            check("Hi,", &|v| PEOPLE.contains(&v.trim_end_matches('.')));
        }
        for n in digits_run(&text.replace(['(', ')', '-', ' '], ""), 10) {
            assert!(n.len() > 11 || is_dummy_number(&n), "{name}: phone-like {n}");
        }
    }
    assert!(
        checked >= 30,
        "only {checked} identity fields found; did the fixture layout change?"
    );
}

/// Every identity field the member-site fixtures carry, by key, wherever it
/// is nested: names are the household's dummies, ids the dummy shapes, the
/// dates of birth January firsts, the providers fictional.
#[test]
fn identity_fields_in_site_fixtures_use_dummy_values() {
    fn walk(name: &str, key: &str, v: &serde_json::Value, checked: &mut usize) {
        match v {
            serde_json::Value::Object(map) => {
                for (k, v) in map {
                    walk(name, k, v, checked);
                }
            }
            serde_json::Value::Array(items) => items.iter().for_each(|v| walk(name, key, v, checked)),
            serde_json::Value::String(s) if !s.is_empty() => {
                let first_name = |s: &str| PEOPLE.contains(&s.to_ascii_uppercase().as_str());
                let ok = match key {
                    "firstName" => first_name(s),
                    "lastName" => s == "Example",
                    "memberName" => is_dummy_name(s),
                    "dob" => s.ends_with("-01-01"),
                    "id" | "subscriberId" => shaped(s, "SYNM", 6),
                    "groupNumber" => shaped(s, "SYNG", 4),
                    "groupName" | "providerName" => is_dummy_org(s),
                    "claimId" => {
                        let base = s.split('-').next().unwrap();
                        base.starts_with("SYNC") || (base.bytes().all(|b| b.is_ascii_digit()) && is_dummy_number(base))
                    }
                    "prescriptionNumber" => s.bytes().all(|b| b.is_ascii_digit()) && is_dummy_number(s),
                    "documentId" => s.starts_with("00000000-0000-4000-8000-0000000000"),
                    "claimKey" => s.starts_with("SYNK-"),
                    "correlationId" => s.starts_with("example-"),
                    _ => return,
                };
                assert!(ok, "{name}: `{key}` holds `{s}`, not a dummy");
                *checked += 1;
            }
            _ => {}
        }
    }
    let dir = format!("{}/tests/fixtures/site", root());
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.display().to_string();
        let text = std::fs::read_to_string(&path).unwrap();
        if name.ends_with(".json") {
            let v: serde_json::Value = serde_json::from_str(&text).unwrap();
            walk(&name, "", &v, &mut checked);
        } else {
            assert!(
                text.contains("EXAMPLE-REQUEST-ID"),
                "{name}: an edge page carries a dummy request id"
            );
        }
    }
    assert!(
        checked >= 60,
        "only {checked} identity fields found; did the fixtures change?"
    );
}

#[test]
fn the_scanner_flags_real_shapes_and_passes_dummies() {
    for real in [
        "call (772) 404-1234",
        "call 888-555-0200",
        "member id 9234567812",
        "mail someone@gmail.com",
        "lives at 1234 NW Oak St, Somewhere",
        "claim 7342918",
        "bad1234567",
    ] {
        assert!(!scan_line(real).is_empty(), "missed: {real}");
    }
    for dummy in [
        "call (555) 010-0001",
        "customer service 888-555-0100",
        "SYNM000001 SYNC00000003",
        "000000000000000-A-S-PS-E",
        "pat@example.com",
        "100 Example St, Exampleton",
        "$16,000.00",
        "GITLEAKS_SHA256: 551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb",
    ] {
        assert!(
            scan_line(dummy).is_empty(),
            "false positive: {dummy}: {:?}",
            scan_line(dummy)
        );
    }
    assert!(is_dummy_name("ALEX EXAMPLE"));
    assert!(!is_dummy_name("ALEX SMITH"));
    assert!(shaped("SYNC00000004", "SYNC", 8));
    assert!(!shaped("SYNC0000004", "SYNC", 8));
}
