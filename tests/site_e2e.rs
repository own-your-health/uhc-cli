//! Every member-site command end to end, in-process, against a loopback
//! server that answers with the synthetic fixtures in `tests/fixtures/site/`
//! (their README has the household). Each runs from a command line through
//! `--profile adult1`, as the binary does (`site_cmd::run_args`), with a
//! temp config configuring the profile and the session in an in-memory
//! vault (`Vault::new(MemoryKeys::new(), dir)`): no keychain item is read
//! or written on any platform, and nothing is contacted beyond 127.0.0.1.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clap::{CommandFactory, FromArgMatches};
use pk_cli_auth::vault::{MemoryKeys, Vault};
use pk_cli_config::profile::ProfileName;
use pk_cli_secrets::Secret;
use serde_json::{json, Value};
use uhc::cli::Cli;
use uhc::model::{patient_id, Book, StatementFile};
use uhc::session::{Session, SessionStore};
use uhc::site_cmd::{self, Env};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/site");
/// The session the tests store: dummy cookies, the documents service's
/// token among them.
const COOKIES: &str = "dummy_session=abc; FH_MU=dummyToken%21p2%21p3; AWSALBTG=dummy-visitor";
const AGENT: &str = "Mozilla/5.0 (synthetic test browser)";

fn tmp(tag: &str) -> PathBuf {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "uhc-site-e2e-{tag}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(format!("{FIXTURES}/{name}")).unwrap()
}

/// What the server answers every request with, instead of the fixtures.
#[derive(Clone)]
enum Fault {
    None,
    /// After this many requests answered from the fixtures, this answer.
    After(usize, u16, &'static str, String),
    /// This answer to each request whose target holds the text.
    On(&'static str, u16, &'static str, String),
}

/// One request the server saw.
#[derive(Debug, Clone)]
struct Seen {
    method: String,
    target: String,
    headers: HashMap<String, String>,
    body: String,
}

struct Server {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    fault: Arc<Mutex<Fault>>,
}

/// The synthetic PDF every document download answers.
fn pdf() -> Vec<u8> {
    uhc::synth::text_pdf("SYNTHETIC TEST DATA - NOT A REAL DOCUMENT")
}

impl Server {
    fn start() -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let fault = Arc::new(Mutex::new(Fault::None));
        let details: HashMap<String, String> = std::fs::read_dir(FIXTURES)
            .unwrap()
            .filter_map(|e| {
                let path = e.unwrap().path();
                let name = path.file_name()?.to_str()?.to_string();
                name.starts_with("claim-detail-").then(|| {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let v: Value = serde_json::from_str(&text).unwrap();
                    let key = v
                        .pointer("/data/claimDetailsAggregate/healthCareClaimDetails/claimKey")
                        .unwrap();
                    (key.as_str().unwrap().to_string(), text)
                })
            })
            .collect();
        let (s, f) = (seen.clone(), fault.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let (s, f, d) = (s.clone(), f.clone(), details.clone());
                std::thread::spawn(move || serve(stream, &s, &f, &d));
            }
        });
        Server { base, seen, fault }
    }

    fn fail_with(&self, status: u16, content_type: &'static str, body: &str) {
        self.fail_after(0, status, content_type, body);
    }

    fn fail_after(&self, n: usize, status: u16, content_type: &'static str, body: &str) {
        let before = self.seen().len();
        *self.fault.lock().unwrap() = Fault::After(before + n, status, content_type, body.to_string());
    }

    fn fail_on(&self, target: &'static str, status: u16, content_type: &'static str, body: &str) {
        *self.fault.lock().unwrap() = Fault::On(target, status, content_type, body.to_string());
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().unwrap().clone()
    }

    fn count(&self, needle: &str) -> usize {
        self.seen().iter().filter(|r| r.target.contains(needle)).count()
    }
}

fn serve(mut stream: TcpStream, seen: &Mutex<Vec<Seen>>, fault: &Mutex<Fault>, details: &HashMap<String, String>) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        if stream.read(&mut byte).unwrap_or(0) != 1 {
            return;
        }
        head.push(byte[0]);
    }
    let head = String::from_utf8_lossy(&head).to_string();
    let mut lines = head.lines();
    let mut first = lines.next().unwrap_or("").split(' ');
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("").to_string();
    let headers: HashMap<String, String> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length = headers.get("content-length").and_then(|n| n.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; length];
    stream.read_exact(&mut body).unwrap();
    let body = String::from_utf8_lossy(&body).to_string();
    let n = {
        let mut seen = seen.lock().unwrap();
        seen.push(Seen {
            method: method.clone(),
            target: target.clone(),
            headers: headers.clone(),
            body: body.clone(),
        });
        seen.len()
    };
    let fault = fault.lock().unwrap().clone();
    let (status, content_type, payload): (u16, &str, Vec<u8>) = match fault {
        Fault::After(after, status, mime, body) if n > after => (status, mime, body.into_bytes()),
        Fault::On(needle, status, mime, body) if target.contains(needle) => (status, mime, body.into_bytes()),
        _ => route(&method, &target, &headers, &body, details),
    };
    let reason = match status {
        200 => "OK",
        401 => "Unauthorized",
        403 => "Forbidden",
        _ => "Other",
    };
    let mut answer = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n",
        payload.len()
    );
    if target.contains("q=claims") {
        // The load balancer's cookie, rotated, as the site does.
        answer.push_str("Set-Cookie: AWSALBTG=dummy-rotated; Path=/; Secure\r\n");
    }
    answer.push_str("\r\n");
    let _ = stream.write_all(answer.as_bytes());
    let _ = stream.write_all(&payload);
}

fn route(
    method: &str,
    target: &str,
    headers: &HashMap<String, String>,
    body: &str,
    details: &HashMap<String, String>,
) -> (u16, &'static str, Vec<u8>) {
    const JSON: &str = "application/json";
    if method == "GET" && target.starts_with("/oneportal/view/documents-aggregate-api/v1/v2/doc360/content/id/") {
        return (200, "application/pdf", pdf());
    }
    if method != "POST" {
        return (404, JSON, b"{}".to_vec());
    }
    let request: Value = serde_json::from_str(body).unwrap();
    assert!(
        request["query"].as_str().unwrap().trim_start().starts_with("query "),
        "a query, never a mutation"
    );
    let text = if target.starts_with("/ce-claims-aggregate-api/graphql?q=claims") {
        fixture("claims.json")
    } else if target.starts_with("/ce-claims-aggregate-api/graphql?q=claimDetails") {
        let key = request["variables"]["claimKey"].as_str().unwrap();
        match details.get(key) {
            Some(text) => text.clone(),
            None => panic!("no detail fixture for {key}"),
        }
    } else if target.starts_with("/ce-claims-aggregate-api/graphql?q=spending") {
        fixture("spending.json")
    } else if target == "/rest/oneportal/web/documents-aggregate-api/v1/graphql" {
        // The documents service checks the token the site's pages send.
        if headers.get("xsrf-token").map(String::as_str) != Some("dummyToken")
            || headers.get("rp-token-suffix").map(String::as_str) != Some("MU")
        {
            return (403, JSON, br#"{"status":403,"error":"Forbidden"}"#.to_vec());
        }
        fixture("documents.json")
    } else {
        return (404, JSON, b"{}".to_vec());
    };
    (200, JSON, text.into_bytes())
}

/// A household's command line: a temp config with profile `adult1`, its
/// session in an in-memory vault, the loopback server unpaced.
struct Household {
    server: Server,
    env: Env,
    config: PathBuf,
}

fn adult1() -> ProfileName {
    ProfileName::parse("adult1").unwrap()
}

impl Household {
    fn new() -> Household {
        Self::with_session(true)
    }

    fn with_session(stored: bool) -> Household {
        Self::on("2026-11-17", stored)
    }

    /// A household whose reads are as of `today`.
    fn on(today: &str, stored: bool) -> Household {
        let dir = tmp("house");
        let config = dir.join("config.json");
        std::fs::write(&config, r#"{"profiles":{"adult1":{}}}"#).unwrap();
        let sessions = SessionStore::with_vault(Vault::new(MemoryKeys::new(), dir.join("vault")));
        if stored {
            sessions
                .put(
                    &adult1(),
                    &Session {
                        cookie_header: Secret::new(COOKIES),
                        user_agent: Some(AGENT.into()),
                        captured_at: "2026-01-02T03:04:05Z".into(),
                    },
                )
                .unwrap();
        }
        let server = Server::start();
        let env = Env {
            sessions,
            base: server.base.clone(),
            pace: Duration::ZERO,
            today: Some(today.into()),
        };
        Household { server, env, config }
    }

    /// `uhc --config <tmp> --profile adult1 <args>`: the exit code and what
    /// went to stdout.
    fn run(&self, args: &[&str]) -> Result<(i32, Vec<u8>), pk_cli_core::CliError> {
        let mut argv = vec!["uhc", "--config", self.config.to_str().unwrap(), "--profile", "adult1"];
        argv.extend_from_slice(args);
        let matches = Cli::command().try_get_matches_from(&argv).unwrap();
        let mut cli = Cli::from_arg_matches(&matches).unwrap();
        cli.given = uhc::source::Given::of(&matches);
        let mut out = Vec::new();
        let code = site_cmd::run_args(&cli, &self.env, &mut out)?;
        Ok((code, out))
    }

    fn json(&self, args: &[&str]) -> Value {
        let mut args = args.to_vec();
        args.push("--json");
        let (code, out) = self.run(&args).unwrap();
        assert_eq!(code, 0);
        serde_json::from_slice(&out).unwrap()
    }

    fn stream(&self, args: &[&str]) -> (i32, Vec<Value>) {
        let (code, out) = self.run(args).unwrap();
        let lines = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (code, lines)
    }

    fn stored(&self) -> Session {
        self.env.sessions.get(&adult1()).unwrap().unwrap()
    }
}

fn id(first: &str) -> String {
    patient_id("SYNM000001", first)
}

fn ids(items: &Value) -> Vec<String> {
    items["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

fn of_type<'a>(stream: &'a [Value], kind: &str) -> Vec<&'a Value> {
    stream.iter().filter(|l| l["resourceType"] == kind).collect()
}

#[test]
fn the_answers_cookies_are_written_back_once() {
    let h = Household::new();
    h.json(&["claims", "list"]);
    let s = h.stored();
    assert!(s.cookie_header.expose().contains("AWSALBTG=dummy-rotated"), "updated");
    assert!(s.cookie_header.expose().contains("FH_MU=dummyToken"), "the rest kept");
    assert_eq!(s.user_agent.as_deref(), Some(AGENT));
    assert_eq!(s.captured_at, "2026-01-02T03:04:05Z");
}

#[test]
fn a_refused_session_is_exit_3_and_is_not_rewritten() {
    let h = Household::new();
    h.server
        .fail_with(401, "application/json", &fixture("unauthorized.json"));
    let e = h.run(&["claims", "list", "--json"]).unwrap_err();
    assert_eq!(e.exit_code(), 3);
    assert_eq!(
        e.remedy(),
        Some("uhc auth login --browser --profile adult1 --overwrite")
    );
    assert_eq!(h.stored().cookie_header.expose(), COOKIES);
    assert_eq!(h.server.seen().len(), 1, "no read after a refusal");
}

/// A refusal partway through an export ends the stream incomplete, its
/// error in the trailer and its exit code the command's; the session is
/// not written back.
#[test]
fn a_refusal_partway_ends_the_stream_incomplete() {
    let h = Household::new();
    // The claims, the documents and the plan balances answer; the first
    // detail is refused.
    h.server
        .fail_after(3, 401, "application/json", &fixture("unauthorized.json"));
    let (code, stream) = h.stream(&["records", "export", "--patient", &id("SAM"), "--ndjson"]);
    assert_eq!(code, 3);
    let trailer = stream.last().unwrap();
    assert_eq!(trailer["complete"], false);
    assert_eq!(trailer["error"]["exit_code"], 3, "{trailer}");
    assert!(of_type(&stream, "ExplanationOfBenefit").is_empty());
    assert_eq!(h.stored().cookie_header.expose(), COOKIES);
}

#[test]
fn the_edge_page_and_graphql_errors_are_exit_5() {
    let h = Household::new();
    h.server.fail_with(403, "text/html", &fixture("edge-403.html"));
    let e = h.run(&["accumulators"]).unwrap_err();
    assert_eq!(e.exit_code(), 5);
    assert!(e.remedy().unwrap().contains("--overwrite"));
    h.server
        .fail_with(200, "application/json", &fixture("graphql-errors.json"));
    assert_eq!(h.run(&["patients", "list"]).unwrap_err().exit_code(), 5);
    h.server.fail_with(503, "text/html", "<p>down</p>");
    assert_eq!(h.run(&["claims", "list"]).unwrap_err().exit_code(), 5);
}

#[test]
fn no_session_stored_is_exit_3_before_any_request() {
    let h = Household::with_session(false);
    let e = h.run(&["claims", "list"]).unwrap_err();
    assert_eq!(e.exit_code(), 3);
    assert_eq!(e.remedy(), Some("uhc auth login --browser --profile adult1"));
    assert!(h.server.seen().is_empty());
}

/// From the site, a claim, the member, the plan and the payer carry the
/// read's time (the header's `pulled_at`) as `meta.lastUpdated`, and a
/// document and its bytes the document's creation date at 00:00:00Z. Read
/// twice, at two times, a document is the same bytes both times.
#[test]
fn site_resources_are_dated_by_the_read_and_documents_by_their_date() {
    let h = Household::new();
    let read = || {
        let (code, stream) = h.stream(&["records", "export", "--patient", &id("SAM"), "--ndjson"]);
        assert_eq!(code, 0);
        stream
    };
    let first = read();
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let second = read();
    assert_ne!(first[0]["pulled_at"], second[0]["pulled_at"], "two reads at two times");
    let documents = |stream: &[Value]| -> Vec<String> {
        stream
            .iter()
            .filter(|r| matches!(r["resourceType"].as_str(), Some("DocumentReference" | "Binary")))
            .map(|r| r.to_string())
            .collect()
    };
    assert!(!documents(&first).is_empty());
    assert_eq!(documents(&first), documents(&second));
    for stream in [&first, &second] {
        let pulled_at = stream[0]["pulled_at"].as_str().unwrap();
        let mut kinds = std::collections::BTreeSet::new();
        for r in stream.iter().filter(|r| r.get("resourceType").is_some()) {
            let kind = r["resourceType"].as_str().unwrap();
            kinds.insert(kind);
            let at = r["meta"]["lastUpdated"]
                .as_str()
                .unwrap_or_else(|| panic!("undated: {r}"));
            assert!(pk_med_records::Timestamp::parse(at).is_some(), "{at}");
            match kind {
                "DocumentReference" | "Binary" => {
                    assert!(at.ends_with("T00:00:00Z") && at.len() == 20, "{at}");
                    assert_ne!(at, pulled_at);
                }
                _ => assert_eq!(at, pulled_at, "{r}"),
            }
        }
        for k in [
            "Patient",
            "Coverage",
            "Organization",
            "ExplanationOfBenefit",
            "DocumentReference",
            "Binary",
        ] {
            assert!(kinds.contains(k), "{k} in {kinds:?}");
        }
    }
}

/// From the site, --patient takes a member's id or first name (the label
/// its grant shows), and without it a household of three is exit 2 naming
/// no member, by id or by name (DESIGN §1.3). The id is the files source's,
/// so a store merges the two.
#[test]
fn records_export_takes_an_id_or_a_label() {
    let h = Household::new();
    h.json(&["patients", "list"]);
    assert_eq!(
        h.json(&["patients", "label", "--patient", &id("SAM"), "Robin"])["items"][0]["label"],
        "Robin"
    );
    let (code, by_label) = h.stream(&["records", "export", "--patient", "robin", "--ndjson"]);
    assert_eq!(code, 0);
    let (code, by_id) = h.stream(&["records", "export", "--patient", &id("SAM"), "--ndjson"]);
    assert_eq!(code, 0);
    assert_eq!(by_label[0]["patient"]["id"], id("SAM"));
    assert_eq!(by_id[0]["patient"]["id"], id("SAM"));
    assert_eq!(by_id[0]["patient"]["label"], "Robin", "the household's label");
    assert_eq!(by_id[0]["patient"]["organization"], "uhc");
    assert_eq!(by_id[0]["organization"]["id"], "uhc");
    // The first name the site shows is not a label (medical-common DESIGN
    // §1.3), and the error repeats neither it nor any member.
    let e = h
        .run(&["records", "export", "--patient", "Sam", "--ndjson"])
        .unwrap_err();
    assert_eq!(e.exit_code(), 4);
    assert!(e.remedy().is_some_and(|r| r.contains("patients label")), "{e}");
    let msg = e.to_string();
    assert!(msg.contains("patients label"), "{msg}");
    for who in ["SAM", "ALEX", "JORDAN", "ROBIN"] {
        assert!(!msg.contains(&id(who)) && !msg.to_uppercase().contains(who), "{msg}");
    }
    let e = h.run(&["records", "export", "--ndjson"]).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    let msg = e.to_string();
    assert!(msg.contains("`uhc --profile <NAME> patients list`"), "{msg}");
    for who in ["SAM", "ALEX", "JORDAN", "ROBIN"] {
        assert!(!msg.contains(&id(who)) && !msg.to_uppercase().contains(who), "{msg}");
    }
}

/// `patients label` (medical-common DESIGN §1): the household's own name
/// for one member, kept in the profile's grant for it. It makes no request
/// and reads no secret.
#[test]
fn patients_label_names_one_member_of_the_profile() {
    let h = Household::new();
    // Before any read, the profile knows no member.
    let e = h
        .run(&["patients", "label", "--patient", &id("SAM"), "Robin"])
        .unwrap_err();
    assert_eq!(e.exit_code(), 4);
    assert!(e.to_string().contains("patients list"), "{e}");
    h.json(&["patients", "list"]);
    let reads = h.server.seen().len();

    let v = h.json(&["patients", "label", "--patient", &id("SAM"), "  Robin "]);
    assert_eq!(v["schema"], "patient-list/v1");
    assert_eq!(
        v["items"],
        json!([{"id": id("SAM"), "organization": "uhc", "label": "Robin", "kind": "member"}])
    );
    let grants = |h: &Household| -> Value {
        let c: Value = serde_json::from_str(&std::fs::read_to_string(&h.config).unwrap()).unwrap();
        c["profiles"]["adult1"]["grants"].clone()
    };
    assert_eq!(grants(&h).as_array().unwrap().len(), 3, "one grant per member read");
    let list = h.json(&["patients", "list"]);
    let sam = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == id("SAM"))
        .unwrap()
        .clone();
    assert_eq!(sam["label"], "Robin");
    assert_eq!(sam["name"], "Sam Example", "the site's name stays the name");
    let reads = reads + 2;
    assert_eq!(h.server.seen().len(), reads, "only patients list read the site");

    // By its label, in any case, a new label replaces the old.
    assert_eq!(
        h.json(&["patients", "label", "--patient", "ROBIN", "Kit"])["items"][0]["label"],
        "Kit"
    );
    assert_eq!(
        h.json(&["claims", "list", "--patient", "kit"])["items"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    let reads = reads + 1;

    // The same label, or clearing an unset one, changes nothing.
    let before = std::fs::read(&h.config).unwrap();
    assert_eq!(
        h.json(&["patients", "label", "--patient", "Kit", "Kit"])["items"][0]["label"],
        "Kit"
    );
    assert_eq!(
        h.json(&["patients", "label", "--patient", &id("ALEX"), "--clear"])["items"][0].get("label"),
        None
    );
    assert_eq!(std::fs::read(&h.config).unwrap(), before, "the config is not rewritten");

    // Collisions and empty labels are exit 2, and change nothing.
    let (alex, jordan) = (id("ALEX"), id("JORDAN"));
    for (who, why) in [
        ("kit", "another member's label"),
        (jordan.as_str(), "a member's id"),
        (" ", "only whitespace"),
        ("", "empty"),
    ] {
        let argv = ["patients", "label", "--patient", alex.as_str(), who];
        let e = h.run(&argv).unwrap_err();
        assert_eq!(e.exit_code(), 2, "{why}: {e}");
        for who in ["SAM", "ALEX", "JORDAN"] {
            assert!(!e.to_string().contains(&id(who)), "{why}: {e}");
        }
    }
    assert_eq!(std::fs::read(&h.config).unwrap(), before);

    // A member the profile does not have is exit 4, and the error does not
    // repeat --patient.
    for q in ["Sam", "nobody", "m000000000000"] {
        let e = h.run(&["patients", "label", "--patient", q, "Lee"]).unwrap_err();
        assert_eq!(e.exit_code(), 4, "{q}");
        assert!(!e.to_string().to_lowercase().contains(&q.to_lowercase()), "{q}: {e}");
    }

    // Human output is one line, and --clear removes the label.
    let (code, out) = h.run(&["patients", "label", "--patient", "kit", "--clear"]).unwrap();
    assert_eq!(code, 0);
    assert_eq!(String::from_utf8(out).unwrap().lines().count(), 1);
    assert!(
        grants(&h).as_array().unwrap().iter().all(|g| g.get("label").is_none()),
        "{}",
        grants(&h)
    );
    assert_eq!(h.server.seen().len(), reads, "patients label read nothing");
}

/// A read of the site does not fail on settings it cannot read: it warns,
/// and `--patient` takes ids only. `patients label`, whose subject they are,
/// is an error.
#[test]
fn unreadable_settings_do_not_stop_a_read() {
    let h = Household::new();
    let mut c: Value = serde_json::from_str(&std::fs::read_to_string(&h.config).unwrap()).unwrap();
    c["profiles"]["adult1"] = json!({"grants": "not a list"});
    std::fs::write(&h.config, c.to_string()).unwrap();
    let before = std::fs::read(&h.config).unwrap();
    let v = h.json(&["patients", "list"]);
    assert_eq!(ids(&v), [id("ALEX"), id("JORDAN"), id("SAM")]);
    assert_eq!(
        h.json(&["claims", "list", "--patient", &id("SAM")])["items"]
            .as_array()
            .unwrap()
            .len(),
        6
    );
    assert_eq!(std::fs::read(&h.config).unwrap(), before, "left as it was");
    assert!(h.run(&["patients", "label", "--patient", &id("SAM"), "Robin"]).is_err());
}

#[test]
fn patients_label_reads_no_secret() {
    // No session stored: every site read is exit 3, and a label is still set.
    let h = Household::with_session(false);
    let mut c: Value = serde_json::from_str(&std::fs::read_to_string(&h.config).unwrap()).unwrap();
    c["profiles"]["adult1"] = json!({"grants": [{"organization": "uhc", "patient": id("SAM")}]});
    std::fs::write(&h.config, c.to_string()).unwrap();
    assert_eq!(
        h.json(&["patients", "label", "--patient", &id("SAM"), "Robin"])["items"][0]["label"],
        "Robin"
    );
    assert!(h.server.seen().is_empty());
    assert_eq!(h.run(&["claims", "list"]).unwrap_err().exit_code(), 3);
}

#[test]
fn records_export_to_a_file_is_refused_before_any_request() {
    let h = Household::new();
    let dir = tmp("out");
    let e = h
        .run(&[
            "records",
            "export",
            "--patient",
            &id("SAM"),
            "-o",
            dir.to_str().unwrap(),
        ])
        .unwrap_err();
    assert_eq!(e.exit_code(), 2);
    assert!(h.server.seen().is_empty());
}

/// `api` sends one checked read with the session and the browser's user
/// agent, prints the JSON answer as it came, and sends nothing for a
/// request that is not a read.
#[test]
fn api_reads_the_site_and_refuses_anything_else() {
    let h = Household::new();
    let query = r#"{"query":"query spending { spendingOverviews { __typename } }","variables":{}}"#;
    let (code, out) = h
        .run(&[
            "api",
            "POST",
            "/ce-claims-aggregate-api/graphql?q=spending",
            "--data",
            query,
        ])
        .unwrap();
    assert_eq!(code, 0);
    let doc: Value = serde_json::from_slice(&out).unwrap();
    assert!(doc["data"]["spendingOverviews"].is_object(), "{doc}");
    {
        let seen = h.server.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].method, "POST");
        assert_eq!(seen[0].headers["user-agent"], AGENT);
        assert!(seen[0].headers["cookie"].contains("dummy_session=abc"));
        assert_eq!(
            serde_json::from_str::<Value>(&seen[0].body).unwrap(),
            serde_json::from_str::<Value>(query).unwrap()
        );
    }

    // The documents service's token goes where the site's pages send it.
    let documents = r#"{"query":"query GetAllDocumentTypes { multiDocTypeListSummary { __typename } }"}"#;
    let (code, out) = h
        .run(&[
            "api",
            "POST",
            "/rest/oneportal/web/documents-aggregate-api/v1/graphql",
            "--data",
            documents,
        ])
        .unwrap();
    assert_eq!(code, 0);
    assert!(serde_json::from_slice::<Value>(&out).unwrap()["data"].is_object());

    let pdf = "/oneportal/view/documents-aggregate-api/v1/v2/doc360/content/id/0000-aaaa?documentClass=u_mber_eob";
    let e = h.run(&["api", "GET", pdf]).err().unwrap();
    assert_eq!(e.exit_code(), 5);
    assert!(e.to_string().contains("documents download"), "{e}");

    let sent = h.server.seen.lock().unwrap().len();
    for args in [
        vec![
            "api",
            "POST",
            "/ce-claims-aggregate-api/graphql",
            "--data",
            r#"{"query":"mutation { a }"}"#,
        ],
        vec!["api", "GET", "/content/myuhc/logout"],
        vec!["api", "GET", "https://example.com/x"],
    ] {
        let e = h.run(&args).err().unwrap();
        assert_eq!(e.exit_code(), 2, "{args:?}");
    }
    assert_eq!(
        h.server.seen.lock().unwrap().len(),
        sent,
        "a refused request sends nothing"
    );
}

/// A write-back that fails fails nothing: the export finished, so it exits
/// 0 with its whole stream, and the stored session is the one that worked.
/// The warning goes to stderr, which an in-process run does not capture.
/// The vault's write lock (`.key.lock`, which `put` opens and `get` never
/// does) is replaced by a folder, so only the write-back fails; were the
/// file renamed, the write would land and this test would fail.
#[test]
fn a_failed_write_back_is_only_a_warning() {
    let h = Household::new();
    let lock = h.config.parent().unwrap().join("vault").join(".key.lock");
    std::fs::remove_file(&lock).unwrap();
    std::fs::create_dir(&lock).unwrap();
    let (code, stream) = h.stream(&["records", "export", "--patient", &id("SAM"), "--ndjson"]);
    assert_eq!(code, 0);
    assert_eq!(stream.last().unwrap()["complete"], true);
    assert!(stream.len() > 2, "the whole stream");
    assert_eq!(h.stored().cookie_header.expose(), COOKIES, "nothing written");
    // With the lock back, the next command writes the rotated cookie.
    std::fs::remove_dir(&lock).unwrap();
    h.json(&["claims", "list"]);
    assert!(h.stored().cookie_header.expose().contains("AWSALBTG=dummy-rotated"));
}

/// A sign-in made while a command runs is never overwritten by that
/// command's cookie write-back.
#[test]
fn a_newer_sign_in_is_not_overwritten() {
    let h = Household::new();
    let site = uhc::portal::Site::open(&h.env.sessions, &adult1(), &h.env.base, Duration::ZERO).unwrap();
    site.household().unwrap();
    let newer = Session {
        cookie_header: Secret::new("dummy_session=newer"),
        user_agent: Some(AGENT.into()),
        captured_at: "2026-02-03T04:05:06Z".into(),
    };
    h.env.sessions.put(&adult1(), &newer).unwrap();
    assert!(!site.finish().unwrap(), "nothing written over the newer sign-in");
    assert_eq!(h.stored().cookie_header.expose(), "dummy_session=newer");

    // With the session unchanged, the rotated cookie is written back.
    let site = uhc::portal::Site::open(&h.env.sessions, &adult1(), &h.env.base, Duration::ZERO).unwrap();
    site.household().unwrap();
    assert!(site.finish().unwrap());
    assert!(h.stored().cookie_header.expose().contains("AWSALBTG=dummy-rotated"));
}

// ------------------------------------------------- the fixtures' household

/// A site document's `documents list` id: the export's `DocumentReference`.
fn doc(tail: &str, first: &str) -> String {
    format!("doc-00000000-0000-4000-8000-0000000000{tail}-{}", id(first))
}

fn named<'a>(stream: &'a [Value], kind: &str, id: &str) -> &'a Value {
    let found = of_type(stream, kind).into_iter().find(|r| r["id"] == id);
    found.unwrap_or_else(|| panic!("no {kind}/{id} in the stream"))
}

fn ids_of(stream: &[Value], kind: &str) -> Vec<String> {
    of_type(stream, kind)
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect()
}

/// The amount of one category of a line's or a claim's adjudication.
fn amount(entries: &Value, code: &str) -> Value {
    let entries = entries.as_array().unwrap();
    let found = entries.iter().find(|e| e["category"]["coding"][0]["code"] == code);
    found.map(|e| e["amount"]["value"].clone()).unwrap_or_default()
}

fn variables(r: &Seen) -> Value {
    serde_json::from_str::<Value>(&r.body).unwrap()["variables"].clone()
}

fn export(h: &Household, first: &str, more: &[&str]) -> Vec<Value> {
    let who = id(first);
    let mut args = vec!["records", "export", "--patient", &who, "--ndjson"];
    args.extend_from_slice(more);
    let (code, stream) = h.stream(&args);
    assert_eq!(code, 0);
    assert_eq!(stream.last().unwrap()["complete"], true);
    stream
}

// ---------------------------------------------------------------- patients

/// The profile's members, the subscriber first, with their ids, coverage,
/// claims and EOBs. The two reads carry the session, the browser's user
/// agent and dates.
#[test]
fn patients_list_is_the_profiles_members() {
    let h = Household::new();
    let v = h.json(&["patients", "list"]);
    assert_eq!(v["schema"], "patient-list/v1");
    assert_eq!(ids(&v), [id("ALEX"), id("JORDAN"), id("SAM")]);
    let expected = [
        ("Alex Example", "subscriber", "self", 4, 5, "2026-08-08"),
        ("Jordan Example", "dependent", "child", 2, 2, "2026-10-09"),
        ("Sam Example", "dependent", "spouse", 6, 2, "2026-07-02"),
    ];
    for (m, (name, role, relationship, claims, statements, latest)) in
        v["items"].as_array().unwrap().iter().zip(expected)
    {
        assert_eq!(m["name"], name);
        assert_eq!(m["organization"], "uhc");
        assert_eq!(m["kind"], "member");
        assert_eq!(m["member_role"], role, "{name}");
        assert_eq!(m["coverage_relationship"], relationship, "{name}");
        // The sign-in is the subscriber's: only their record is their own.
        assert_eq!(m.get("relationship").is_some(), role == "subscriber", "{name}");
        assert_eq!(m["member_id"], "SYNM000001");
        assert_eq!(m["group_number"], "SYNG0001");
        assert!(m.get("group_name").is_none(), "{name}");
        assert!(m.get("label").is_none(), "a first name is no label");
        assert_eq!(m["claims"], claims, "{name}");
        assert_eq!(m["statements"], statements, "{name}");
        assert_eq!(m["latest_statement"], latest, "{name}");
    }

    let seen = h.server.seen();
    assert_eq!(seen.len(), 2, "the claims and the documents");
    for r in &seen {
        assert_eq!(r.method, "POST");
        assert_eq!(r.headers["user-agent"], AGENT);
        assert!(r.headers["cookie"].contains("dummy_session=abc"));
        assert_eq!(r.headers["accept"], "application/json");
    }
    assert_eq!(
        variables(&seen[0]),
        json!({"startDate": "2016-01-01", "stopDate": "2026-11-17"})
    );
    assert_eq!(
        variables(&seen[1]),
        json!({"processedStartDate": "2016-01-01", "processedStopDate": "2026-11-17"})
    );
    // The documents service's token, for it alone.
    assert!(!seen[0].headers.contains_key("xsrf-token"));
    assert_eq!(seen[1].headers["xsrf-token"], "dummyToken");
}

// ------------------------------------------------------------------ claims

#[test]
fn claims_list_is_each_claim_once_and_reads_no_detail() {
    let h = Household::new();
    let v = h.json(&["claims", "list"]);
    assert_eq!(v["schema"], "claim-list/v1");
    assert_eq!(
        ids(&v),
        [
            "cSYNC555248",
            "c555040000318",
            "cSYNC55531742",
            "cSYNC55523128",
            "cSYNC55512785",
            "cSYNC55521940",
            "cSYNC55514009",
            "cSYNC55526407",
            "cSYNC55520613",
            "rx55500000000000237",
            "rx55500000000000481",
            "cSYNC55511862",
        ],
        "newest service first"
    );
    assert_eq!(h.server.seen().len(), 1, "one read, no detail");
    let of = |id: &str| -> Value {
        let items = v["items"].as_array().unwrap();
        items.iter().find(|c| c["id"] == id).unwrap().clone()
    };
    let c = of("cSYNC55511862");
    assert_eq!(c["claim_number"], "SYNC55511862");
    assert_eq!(c["patient"], id("ALEX"));
    assert_eq!(c["first_name"], "Alex");
    assert_eq!((&c["versions"], &c["processed"]), (&json!(2), &json!("2026-03-19")));
    assert_eq!((&c["type"], &c["network"]), (&json!("medical"), &json!("in")));
    assert_eq!(c["you_owe"], json!({"amount": "93.66", "currency": "USD"}));
    assert!(c.get("lines").is_none(), "not read: {c}");
    let reversed = of("rx55500000000000481");
    assert_eq!(
        (&reversed["reversed"], &reversed["versions"]),
        (&json!(true), &json!(2))
    );
    assert_eq!(reversed["plan_paid"]["amount"], "176.45");
    assert_eq!(reversed["lines"], 1);
    assert_eq!(of("c555040000318")["network"], "out");
    assert_eq!(of("c555040000318")["type"], "oral");
    assert_eq!(of("cSYNC55523128")["status"], "provider_denied");
    let waiting = of("cSYNC555248");
    assert_eq!(waiting["status"], "in_process");
    assert!(waiting.get("processed").is_none() && waiting.get("network").is_none());

    // One member's, and those served since a day.
    let jordan = h.json(&["claims", "list", "--patient", &id("JORDAN")]);
    assert_eq!(ids(&jordan), ["c555040000318", "cSYNC55531742"]);
    let since = h.json(&["claims", "list", "--since", "2026-08-11"]);
    assert_eq!(
        ids(&since),
        ["cSYNC555248", "c555040000318", "cSYNC55531742", "cSYNC55523128"],
        "the day itself counts"
    );
    let both = h.json(&["claims", "list", "--patient", &id("SAM"), "--since", "2026-06-17"]);
    assert_eq!(ids(&both), ["cSYNC555248", "cSYNC55523128"]);
    let e = h.run(&["claims", "list", "--patient", "nobody"]).unwrap_err();
    assert_eq!(e.exit_code(), 4);
    assert_eq!(h.server.count("q=claimDetails"), 0);
}

#[test]
fn claims_get_reads_the_latest_versions_detail() {
    let h = Household::new();
    let v = h.json(&["claims", "get", "cSYNC55511862"]);
    assert_eq!(v["schema"], "claim/v1");
    assert_eq!((&v["id"], &v["versions"]), (&json!("cSYNC55511862"), &json!(2)));
    assert_eq!(v["type"], "professional");
    assert_eq!(
        v["document"], "00000000-0000-4000-8000-0000000000a2",
        "the latest EOB listing any version"
    );
    assert_eq!(v["coinsurance"]["amount"], "93.66");
    assert_eq!(v["lines"].as_array().unwrap().len(), 2);
    let mri = &v["lines"][0];
    assert_eq!(mri["description"], "KNEE MRI");
    assert_eq!(
        mri["procedure"],
        json!({"code": "73721", "display": "MRI OF A LEG JOINT, NO CONTRAST"})
    );
    assert_eq!(mri["codes"], json!(["K4", "T3"]));
    assert_eq!(mri["counted"], true);
    assert_eq!(v["codes"].as_object().unwrap().len(), 2);
    let seen = h.server.seen();
    assert_eq!(seen.len(), 3, "the claims, the documents and one detail");
    assert_eq!(
        variables(&seen[2]),
        json!({"claimKey": "SYNK-018", "claimType": "MEDICAL"})
    );

    // By its claim number too; a line with a split code, and one the totals
    // leave out.
    let v = h.json(&["claims", "get", "SYNC55512785"]);
    assert_eq!(v["type"], "institutional");
    assert_eq!(
        v["lines"][1]["procedure"],
        json!({
            "code": "71046",
            "display": "X-RAY OF THE CHEST, 2 VIEWS",
            "suffix": "R",
            "site_code": "71046R",
        })
    );
    assert_eq!(v["lines"][1]["revenue"]["code"], "0324");
    let v = h.json(&["claims", "get", "cSYNC55514009"]);
    assert_eq!(v["lines"][1]["counted"], false);
    assert_eq!(v["lines"][1]["you_owe"]["amount"], "28.00");
    assert_eq!(v["you_owe"]["amount"], "173.27");

    // A pharmacy claim has no detail to read.
    let details = h.server.count("q=claimDetails");
    assert_eq!(details, 3);
    let v = h.json(&["claims", "get", "rx55500000000000237"]);
    assert_eq!(v["type"], "pharmacy");
    assert_eq!(v["lines"][0]["description"], "FLUTICASONE SPR 50MCG");
    // A claim the site does not list is exit 4.
    let e = h.run(&["claims", "get", "cSYNC00000000"]).unwrap_err();
    assert_eq!(e.exit_code(), 4);
    assert_eq!(h.server.count("q=claimDetails"), details, "no detail read");
}

// ------------------------------------------------------------ accumulators

#[test]
fn accumulators_are_each_members_and_the_familys() {
    let h = Household::new();
    let v = h.json(&["accumulators"]);
    assert_eq!(v["schema"], "accumulator-list/v1");
    let rows = v["items"].as_array().unwrap();
    assert_eq!(rows.len(), 16, "three members and the family, four each");
    assert_eq!(h.server.seen().len(), 1, "the plan balances");
    assert_eq!(variables(&h.server.seen()[0]), json!({}));
    for r in rows {
        assert_eq!(r["plan_year"], 2026);
        assert_eq!(r["as_of"], "2026-11-17");
        assert_eq!(r["period"], json!({"start": "2026-01-01", "end": "2026-12-31"}));
    }
    // The family's rows name no one.
    let family: Vec<&Value> = rows.iter().filter(|r| r["family"] == true).collect();
    assert_eq!(family.len(), 4);
    for r in &family {
        assert_eq!(r["first_name"], Value::Null);
        assert!(r["patient"].as_str().unwrap().starts_with("family-"));
    }
    assert_eq!(family[0]["maximum"]["amount"], "2300.00");
    assert_eq!(family[0]["used"]["amount"], "1577.35");
    let met: Vec<&Value> = rows.iter().filter(|r| r["met"] == true).collect();
    assert_eq!(met.len(), 1);
    assert_eq!(
        (&met[0]["first_name"], &met[0]["network"], &met[0]["limit"]),
        (&json!("Alex"), &json!("in"), &json!("deductible"))
    );
    assert_eq!(met[0]["remaining"]["amount"], "0.00");

    // One member's rows, and the family's with them.
    let v = h.json(&["accumulators", "--patient", &id("JORDAN")]);
    let names: Vec<Value> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["first_name"].clone())
        .collect();
    let (jordan, none) = (json!("Jordan"), Value::Null);
    assert_eq!(
        names,
        [&jordan, &jordan, &jordan, &jordan, &none, &none, &none, &none].map(Value::clone)
    );
    // The plan year is the calendar year, not the year a coverage began.
    let of_year = |h: &Household, y: &str| ids_len(&h.json(&["accumulators", "--year", y]));
    assert_eq!(of_year(&h, "2026"), 16);
    assert_eq!(of_year(&h, "2023"), 0);
    let next = Household::on("2027-01-04", true);
    assert_eq!(of_year(&next, "2026"), 0);
    let v = next.json(&["accumulators", "--year", "2027"]);
    assert_eq!(ids_len(&v), 16);
    assert_eq!(
        v["items"][0]["period"],
        json!({"start": "2027-01-01", "end": "2027-12-31"})
    );
}

fn ids_len(list: &Value) -> usize {
    list["items"].as_array().unwrap().len()
}

// --------------------------------------------------------------- documents

#[test]
fn documents_list_is_one_row_per_document_and_member() {
    let h = Household::new();
    let v = h.json(&["documents", "list"]);
    assert_eq!(v["schema"], "document-list/v1");
    assert_eq!(
        ids(&v),
        [
            doc("a4", "JORDAN"),
            doc("d4", "JORDAN"),
            doc("b2", "ALEX"),
            doc("a3", "ALEX"),
            doc("b6", "SAM"),
            doc("a9", "SAM"),
            doc("a6", "ALEX"),
            doc("a5", "SAM"),
            doc("a8", "ALEX"),
            doc("a2", "ALEX"),
            doc("a1", "ALEX"),
        ],
        "newest first, without the plan's general form"
    );
    let rows = v["items"].as_array().unwrap();
    assert_eq!(rows[7]["name"], "UnitedHealthcare Claim EOB 2026-05-07, Sam");
    assert_eq!(rows[7]["category"], "eob");
    assert_eq!(
        rows[7]["file"],
        "2026-05-07_uhc-eob_00000000-0000-4000-8000-0000000000a5.pdf"
    );
    assert_eq!(rows[1]["category"], "eob", "a dental EOB is one too");
    assert_eq!(rows[2]["category"], "health-statement");
    assert_eq!(rows[4]["category"], "prior-authorization-letters");
    let v = h.json(&["documents", "list", "--since", "2026-08-08", "--until", "2026-10-07"]);
    assert_eq!(ids(&v), [doc("d4", "JORDAN"), doc("b2", "ALEX"), doc("a3", "ALEX")]);
    let v = h.json(&["documents", "list", "--limit", "2"]);
    assert_eq!(ids(&v), [doc("a4", "JORDAN"), doc("d4", "JORDAN")]);
    assert_eq!(h.server.count("doc360"), 0, "a list downloads nothing");
}

#[test]
fn documents_download_saves_one_and_never_onto_a_file() {
    let h = Household::new();
    let dir = tmp("one");
    let dental = doc("d4", "JORDAN");
    let into = dir.to_str().unwrap();
    let v = h.json(&["documents", "download", &dental, "-o", into]);
    assert_eq!(v["schema"], "document-download/v1");
    assert_eq!(v["id"], dental.as_str());
    let path = PathBuf::from(v["path"].as_str().unwrap());
    assert_eq!(
        path.file_name().unwrap(),
        "2026-10-07_uhc-eob_00000000-0000-4000-8000-0000000000d4.pdf"
    );
    assert_eq!(std::fs::read(&path).unwrap(), pdf());
    assert_eq!(v["bytes"], pdf().len());
    let get = h.server.seen().into_iter().find(|r| r.method == "GET").unwrap();
    assert!(
        get.target
            .ends_with("/00000000-0000-4000-8000-0000000000d4?documentClass=u_sb_dental_elec"),
        "{}",
        get.target
    );
    assert_eq!(get.headers["user-agent"], AGENT);
    assert!(get.headers["cookie"].contains("dummy_session=abc"));

    // Onto a file that is there: exit 2, and not read again.
    std::fs::write(&path, b"kept").unwrap();
    for target in [into, path.to_str().unwrap()] {
        let e = h.run(&["documents", "download", &dental, "-o", target]).unwrap_err();
        assert_eq!(e.exit_code(), 2);
        assert!(e.to_string().contains("--overwrite"), "{e}");
    }
    assert_eq!(std::fs::read(&path).unwrap(), b"kept");
    assert_eq!(h.server.count("doc360"), 1);
    h.json(&["documents", "download", &dental, "--overwrite", "-o", into]);
    assert_eq!(std::fs::read(&path).unwrap(), pdf());

    // By the site's own document id, to stdout.
    let (code, out) = h
        .run(&[
            "documents",
            "download",
            "00000000-0000-4000-8000-0000000000b2",
            "-o",
            "-",
        ])
        .unwrap();
    assert_eq!((code, out), (0, pdf()));
}

#[test]
fn a_prefix_that_names_no_one_document_downloads_nothing() {
    let h = Household::new();
    // Too short to name one is a usage error.
    let e = h.run(&["documents", "download", "0000"]).unwrap_err();
    assert_eq!(e.exit_code(), 2);
    // One several documents share, or none has, names no document.
    for q in ["00000000-0000", "doc-00000000", "ffffffff"] {
        let e = h.run(&["documents", "download", q]).unwrap_err();
        assert_eq!(e.exit_code(), 4, "{q}: {e}");
    }
    let e = h.run(&["documents", "download", "00000000-0000"]).unwrap_err();
    assert!(e.to_string().contains("matches 11 documents"), "{e}");
    for who in ["ALEX", "JORDAN", "SAM"] {
        assert!(!e.to_string().contains(&id(who)), "{e}");
    }
    assert_eq!(h.server.count("doc360"), 0);
}

#[test]
fn documents_download_all_skips_what_is_saved() {
    let h = Household::new();
    let dir = tmp("all").join("archive");
    let all = ["documents", "download", "--all", "-o", dir.to_str().unwrap()];
    let v = h.json(&all);
    assert_eq!(v["schema"], "document-download-batch/v1");
    assert_eq!(v["count"], 11);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 11);
    assert_eq!(h.server.count("doc360"), 11);

    // The site's bytes differ on each download, so a file of a document's
    // name is that document, whatever it holds.
    let kept = dir.join("2026-09-01_uhc-health-statement_00000000-0000-4000-8000-0000000000b2.pdf");
    std::fs::write(&kept, b"kept").unwrap();
    std::fs::remove_file(dir.join("2026-05-07_uhc-eob_00000000-0000-4000-8000-0000000000a5.pdf")).unwrap();
    let v = h.json(&all);
    assert_eq!(v["count"], 1);
    assert_eq!(v["items"][0]["id"], doc("a5", "SAM"));
    let skipped = v["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 10);
    assert!(skipped.iter().all(|s| s["code"] == "exists"), "{v}");
    assert_eq!(h.server.count("doc360"), 12, "only the missing one is read");
    assert_eq!(std::fs::read(&kept).unwrap(), b"kept");

    let mut overwrite = all.to_vec();
    overwrite.push("--overwrite");
    assert_eq!(h.json(&overwrite)["count"], 11);
    assert_eq!(std::fs::read(&kept).unwrap(), pdf());
}

// ----------------------------------------------------------------- records

/// The whole of one member's record: the profile's Patient and Coverage,
/// each claim with its detail's codes, notes and network, and each document
/// with the claims it lists.
#[test]
fn records_export_carries_the_claims_codes_and_the_members_documents() {
    let h = Household::new();
    let stream = export(&h, "ALEX", &[]);
    let alex = id("ALEX");
    assert_eq!(stream[0]["schema"], "records-stream/v1");
    assert_eq!(stream[0]["method"], "portal");
    assert_eq!(stream[0]["organization"]["fhir_base"], "https://member.uhc.com/myuhc");
    assert_eq!(stream[0]["patient"]["member_role"], "subscriber");
    let trailer = stream.last().unwrap();
    assert_eq!(
        trailer["counts"],
        json!({
            "Binary": 6, "Coverage": 1, "DocumentReference": 6, "ExplanationOfBenefit": 4,
            "Organization": 1, "Patient": 1,
        })
    );
    // What was read: one detail for each claim that has one (a pharmacy's
    // has none) and one PDF for each document, the rest once.
    for (target, n) in [
        ("q=claims&", 1),
        ("documents-aggregate-api/v1/graphql", 1),
        ("q=spending", 1),
        ("q=claimDetails", 3),
        ("doc360", 6),
    ] {
        assert_eq!(h.server.count(target), n, "{target}");
    }
    assert_eq!(h.server.seen().len(), 12);

    for r in &stream[1..stream.len() - 1] {
        assert_eq!(r["meta"]["source"], "uhc:member-site");
        assert_eq!(r["meta"]["tag"][0]["code"], "mapped-from-member-site");
    }
    let patient = named(&stream, "Patient", &alex);
    assert_eq!(patient["identifier"][0]["value"], "SYNM000001");
    assert_eq!(
        patient["name"],
        json!([{"text": "Alex Example", "family": "Example", "given": ["Alex"]}])
    );
    assert_eq!(patient["birthDate"], "1984-01-01");
    assert_eq!(patient["gender"], "female");
    let coverage = of_type(&stream, "Coverage")[0];
    assert_eq!(coverage["status"], "active");
    assert_eq!(coverage["subscriberId"], "SYNM000001");
    assert_eq!(coverage["relationship"]["coding"][0]["code"], "self");
    assert_eq!(
        coverage["period"],
        json!({"start": "2023-04-01", "end": "9999-12-31"}),
        "the medical coverage's, though dental is listed first"
    );
    assert_eq!(coverage["class"][0]["value"], "SYNG0001");
    assert_eq!(coverage["payor"][0]["reference"], "Organization/uhc");

    assert_eq!(
        ids_of(&stream, "ExplanationOfBenefit"),
        ["cSYNC55512785", "cSYNC55514009", "rx55500000000000237", "cSYNC55511862"]
    );
    let eob = named(&stream, "ExplanationOfBenefit", "cSYNC55512785");
    assert_eq!(eob["status"], "active");
    assert_eq!(eob["patient"]["reference"], format!("Patient/{alex}"));
    assert_eq!(
        eob["insurance"][0]["coverage"]["reference"],
        format!("Coverage/{}", coverage["id"].as_str().unwrap())
    );
    assert_eq!(eob["identifier"][0]["value"], "SYNC55512785");
    assert_eq!(eob["type"]["coding"][0]["code"], "institutional");
    assert_eq!(eob["created"], "2026-08-06");
    assert_eq!(
        eob["billablePeriod"],
        json!({"start": "2026-07-18", "end": "2026-07-19"})
    );
    assert_eq!(eob["provider"]["display"], "EXAMPLE COMMUNITY HOSPITAL");
    assert_eq!(
        eob["adjudication"][0]["category"]["coding"][0]["code"],
        "benefitpaymentstatus"
    );
    assert_eq!(eob["adjudication"][0]["reason"]["coding"][0]["code"], "innetwork");
    const CPT: &str = "http://www.ama-assn.org/go/cpt";
    const HCPCS: &str = "https://www.cms.gov/Medicare/Coding/HCPCSReleaseCodeSets";
    let items = eob["item"].as_array().unwrap();
    let coded: Vec<(&str, &str, &str)> = items
        .iter()
        .map(|i| {
            let p = &i["productOrService"]["coding"][0];
            (
                i["revenue"]["coding"][0]["code"].as_str().unwrap(),
                p["system"].as_str().unwrap(),
                p["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        coded,
        [
            ("0450", CPT, "99284"),
            ("0324", CPT, "71046"),
            ("0305", CPT, "85025"),
            ("0636", HCPCS, "J1885"),
            ("0258", HCPCS, "J7030"),
        ]
    );
    assert_eq!(
        items[0]["productOrService"],
        json!({
            "coding": [{"system": CPT, "code": "99284", "display": "EMERGENCY VISIT, MODERATE"}],
            "text": "EMERGENCY ROOM VISIT",
        })
    );
    assert_eq!(
        items[1]["productOrService"]["coding"][1],
        json!({
            "system": "https://github.com/own-your-health/uhc-cli/fhir/site-procedure-code",
            "code": "71046R",
        })
    );
    assert_eq!(items[0]["net"], json!({"value": 1386.0, "currency": "USD"}));
    assert_eq!(
        eob["processNote"],
        json!([
            {"number": 1, "type": "print",
             "text": "K4 -- THE AMOUNT SAVED IS THE DISCOUNT THIS NETWORK PROVIDER GIVES YOUR PLAN."},
            {"number": 2, "type": "print",
             "text": "Q9 -- THIS SERVICE IS PART OF ANOTHER SERVICE FROM THE SAME DAY AND IS NOT PAID SEPARATELY."},
        ])
    );
    assert_eq!(items[0]["noteNumber"], json!([1]));
    assert_eq!(amount(&eob["total"], "submitted"), 1893.0);
    assert_eq!(amount(&eob["total"], "memberliability"), 476.65);
    assert!(
        eob.get("benefitBalance").is_none(),
        "the site shows today's balances only"
    );

    // Each claim points at the latest EOB listing it, and each document at
    // the exported claims its rows list.
    let info = |id: &str| named(&stream, "ExplanationOfBenefit", id)["supportingInfo"].clone();
    let points_at = |id: &str, tail: &str| {
        assert_eq!(
            info(id),
            json!([{
                "sequence": 1,
                "category": {"coding": [{
                    "system": "http://terminology.hl7.org/CodeSystem/claiminformationcategory",
                    "code": "info",
                }]},
                "valueReference": {"reference": format!("DocumentReference/{}", doc(tail, "ALEX"))},
            }]),
            "{id}"
        );
    };
    points_at("cSYNC55512785", "a3");
    points_at("cSYNC55514009", "a6");
    points_at("cSYNC55511862", "a2");
    points_at("rx55500000000000237", "a8");
    assert_eq!(
        ids_of(&stream, "DocumentReference"),
        ["b2", "a3", "a6", "a8", "a2", "a1"].map(|t| doc(t, "ALEX"))
    );
    for (tail, claim) in [
        ("a3", "cSYNC55512785"),
        ("a6", "cSYNC55514009"),
        ("a8", "rx55500000000000237"),
        ("a2", "cSYNC55511862"),
        // The first version's statement is a document of the claim too.
        ("a1", "cSYNC55511862"),
    ] {
        let d = named(&stream, "DocumentReference", &doc(tail, "ALEX"));
        assert_eq!(
            d["context"]["related"],
            json!([{"reference": format!("ExplanationOfBenefit/{claim}")}]),
            "{tail}"
        );
        assert_eq!(d["category"], json!([{"text": "EOB"}]));
        assert_eq!(d["type"]["text"], "Explanation of Benefits");
        assert_eq!(d["subject"]["reference"], format!("Patient/{alex}"));
    }
    let statement = named(&stream, "DocumentReference", &doc("b2", "ALEX"));
    assert_eq!(statement["type"]["text"], "Health Statement");
    assert_eq!(statement["category"], json!([{"text": "Health Statement"}]));
    assert_eq!(statement["date"], "2026-09-01T00:00:00Z");
    assert!(statement.get("context").is_none(), "it lists no claim");
    let attachment = &statement["content"][0]["attachment"];
    let binary = attachment["url"].as_str().unwrap().strip_prefix("Binary/").unwrap();
    assert_eq!(attachment["size"], pdf().len());
    assert_eq!(named(&stream, "Binary", binary)["contentType"], "application/pdf");
}

/// A dental claim out of network, with its CDT codes and teeth.
#[test]
fn an_oral_claim_carries_cdt_codes_and_its_teeth() {
    let h = Household::new();
    let stream = export(&h, "JORDAN", &[]);
    let eob = named(&stream, "ExplanationOfBenefit", "c555040000318");
    assert_eq!(
        eob["type"],
        json!({
            "coding": [{
                "system": "http://terminology.hl7.org/CodeSystem/claim-type",
                "code": "oral",
                "display": "Oral",
            }],
            "text": "Dental claim",
        })
    );
    assert_eq!(eob["identifier"][0]["value"], "555040000318");
    assert_eq!(eob["adjudication"][0]["reason"]["coding"][0]["code"], "outofnetwork");
    let rows: Vec<(Value, Value, Value, Value)> = eob["item"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let coding = &i["productOrService"]["coding"][0];
            (
                coding["system"].clone(),
                coding["code"].clone(),
                i["bodySite"]["text"].clone(),
                amount(&i["adjudication"], "eligible"),
            )
        })
        .collect();
    let cdt = json!("http://ada.org/cdt");
    assert_eq!(
        rows,
        [
            (cdt.clone(), json!("D0120"), Value::Null, json!(52.0)),
            (cdt.clone(), json!("D1120"), json!("Tooth 01-32"), json!(74.5)),
            (cdt.clone(), json!("D1351"), json!("Tooth 19"), json!(49.0)),
            (cdt, json!("D1351"), json!("Tooth 30"), json!(49.0)),
        ],
        "the site's allowed amount, not the charge less a write-off"
    );
    assert_eq!(amount(&eob["total"], "noncovered"), 78.5);

    let dental = named(&stream, "DocumentReference", &doc("d4", "JORDAN"));
    assert_eq!(
        dental["context"]["related"],
        json!([{"reference": "ExplanationOfBenefit/c555040000318"}])
    );
    assert_eq!(h.server.count("q=claimDetails"), 2);
    assert_eq!(h.server.count("doc360"), 2);
}

/// An EOB of two claims is one document, of both.
#[test]
fn an_eob_of_two_claims_is_one_document() {
    let h = Household::new();
    let stream = export(&h, "SAM", &[]);
    let claims = ["cSYNC55526407", "cSYNC55520613"];
    let shared = named(&stream, "DocumentReference", &doc("a5", "SAM"));
    let related = claims.map(|c| json!({"reference": format!("ExplanationOfBenefit/{c}")}));
    assert_eq!(shared["context"]["related"], json!(related));
    for (c, network) in claims.iter().zip(["outofnetwork", "innetwork"]) {
        let eob = named(&stream, "ExplanationOfBenefit", c);
        assert_eq!(
            eob["supportingInfo"][0]["valueReference"]["reference"],
            format!("DocumentReference/{}", doc("a5", "SAM"))
        );
        assert_eq!(eob["adjudication"][0]["reason"]["coding"][0]["code"], network);
    }
    let shared_id = doc("a5", "SAM");
    let held = ids_of(&stream, "DocumentReference");
    assert_eq!(held.iter().filter(|d| **d == shared_id).count(), 1);
    assert_eq!(h.server.count("doc360"), 3, "its PDF once, with the other two");
}

/// A reversed pharmacy claim is `cancelled`, with the original's amounts. A
/// claim in process is left out, and the trailer says so.
#[test]
fn a_reversed_claim_is_cancelled_and_one_in_process_is_skipped() {
    let h = Household::new();
    let stream = export(&h, "SAM", &[]);
    assert_eq!(
        ids_of(&stream, "ExplanationOfBenefit"),
        [
            "cSYNC55523128",
            "cSYNC55521940",
            "cSYNC55526407",
            "cSYNC55520613",
            "rx55500000000000481"
        ]
    );
    for eob in of_type(&stream, "ExplanationOfBenefit") {
        let reversed = eob["id"] == "rx55500000000000481";
        let status = if reversed { "cancelled" } else { "active" };
        assert_eq!(eob["status"], status, "{}", eob["id"]);
    }
    let reversed = named(&stream, "ExplanationOfBenefit", "rx55500000000000481");
    assert_eq!(reversed["type"]["coding"][0]["code"], "pharmacy");
    assert_eq!(amount(&reversed["total"], "submitted"), 318.6);
    assert_eq!(amount(&reversed["total"], "memberliability"), 45.0);
    assert_eq!(
        reversed["item"][0]["productOrService"],
        json!({"text": "Prescription drug"}),
        "the site masks its name"
    );
    assert_eq!(reversed["supportingInfo"], json!([]), "no document lists it");

    let trailer = stream.last().unwrap();
    let skipped = trailer["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1, "{trailer}");
    assert_eq!(skipped[0]["code"], "in_process");
    assert_eq!(skipped[0]["resource_type"], "ExplanationOfBenefit");
    assert_eq!(skipped[0]["id"], "cSYNC555248");
    assert_eq!(h.server.count("q=claimDetails"), 4, "none for the one in process");

    // Lines the totals leave out are marked, in a stream as in a claim.
    let lab = named(&stream, "ExplanationOfBenefit", "cSYNC55521940");
    let marked: Vec<bool> = lab["item"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            let entries = i["adjudication"].as_array().unwrap();
            entries
                .iter()
                .any(|e| e["category"]["coding"][0]["code"] == "not-counted")
        })
        .collect();
    assert_eq!(marked, [false, false, false, true, true]);
    assert_eq!(amount(&lab["total"], "discount"), 267.85);
    assert_eq!(amount(&lab["total"], "eligible"), 61.15);
    // A denied claim the site gives no network for has no network status.
    let denied = named(&stream, "ExplanationOfBenefit", "cSYNC55523128");
    assert!(denied.get("adjudication").is_none());
    assert_eq!(amount(&denied["total"], "memberliability"), 0.0);
}

/// A claim's benefit period is the calendar year it was served in, whenever
/// it is read.
#[test]
fn a_claims_benefit_period_is_its_year_of_service() {
    for today in ["2026-11-17", "2027-02-03"] {
        let h = Household::on(today, true);
        let stream = export(&h, "ALEX", &[]);
        let periods: Vec<(&str, &Value)> = of_type(&stream, "ExplanationOfBenefit")
            .iter()
            .map(|e| (e["id"].as_str().unwrap(), &e["benefitPeriod"]))
            .collect();
        let (last, this) = (
            json!({"start": "2025-01-01", "end": "2025-12-31"}),
            json!({"start": "2026-01-01", "end": "2026-12-31"}),
        );
        assert_eq!(
            periods,
            [
                ("cSYNC55512785", &this),
                ("cSYNC55514009", &this),
                ("rx55500000000000237", &this),
                // Served in December 2025, processed in 2026.
                ("cSYNC55511862", &last),
            ],
            "{today}"
        );
    }
}

/// The plan balances are read for the benefit period only: without them the
/// claims go out without one, and the trailer says so.
#[test]
fn claims_go_out_without_a_benefit_period_when_the_balances_fail() {
    let h = Household::new();
    h.server.fail_on("q=spending", 503, "text/html", "<p>down</p>");
    let stream = export(&h, "ALEX", &[]);
    let eobs = of_type(&stream, "ExplanationOfBenefit");
    assert_eq!(eobs.len(), 4);
    for e in &eobs {
        assert!(e.get("benefitPeriod").is_none(), "{}", e["id"]);
    }
    assert_eq!(of_type(&stream, "DocumentReference").len(), 6, "the rest as ever");
    let skipped = stream.last().unwrap()["skipped"].as_array().unwrap();
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0]["code"], "upstream");
    let reason = skipped[0]["reason"].as_str().unwrap();
    assert!(reason.contains("benefitPeriod"), "{reason}");
}

#[test]
fn a_session_refused_at_the_balances_is_exit_3() {
    let h = Household::new();
    h.server
        .fail_on("q=spending", 401, "application/json", &fixture("unauthorized.json"));
    let e = h
        .run(&["records", "export", "--patient", &id("ALEX"), "--ndjson"])
        .unwrap_err();
    assert_eq!(e.exit_code(), 3);
    assert_eq!(h.server.seen().len(), 3, "nothing read after it");
    assert_eq!(h.stored().cookie_header.expose(), COOKIES, "not written back");
}

/// `--since` reads the details of the claims processed since and the PDFs
/// created since, and no others.
#[test]
fn since_reads_only_what_changed() {
    let h = Household::new();
    let stream = export(&h, "ALEX", &["--since", "2026-08-06"]);
    assert_eq!(stream[0]["since"], "2026-08-06");
    assert_eq!(
        stream[0]["since_unbounded"],
        json!(["Coverage", "Organization", "Patient"])
    );
    assert_eq!(
        ids_of(&stream, "ExplanationOfBenefit"),
        ["cSYNC55512785"],
        "processed that day"
    );
    assert_eq!(
        ids_of(&stream, "DocumentReference"),
        ["b2", "a3"].map(|t| doc(t, "ALEX"))
    );
    assert_eq!(of_type(&stream, "Patient").len(), 1);
    assert_eq!(of_type(&stream, "Coverage").len(), 1);
    assert_eq!(h.server.count("q=claimDetails"), 1);
    assert_eq!(h.server.count("doc360"), 2);

    // The day after: the statement is still new, its claim is not.
    let h = Household::new();
    let stream = export(&h, "ALEX", &["--since", "2026-08-07"]);
    assert!(of_type(&stream, "ExplanationOfBenefit").is_empty());
    assert_eq!(h.server.count("q=claimDetails"), 0);
    let d = named(&stream, "DocumentReference", &doc("a3", "ALEX"));
    assert!(d.get("context").is_none(), "only claims the stream carries: {d}");

    // A claim served long before counts by when it was processed; its first
    // version's statement is older, so not read.
    let h = Household::new();
    let stream = export(&h, "ALEX", &["--since", "2026-03-19"]);
    let reprocessed = named(&stream, "ExplanationOfBenefit", "cSYNC55511862");
    assert_eq!(
        reprocessed["supportingInfo"][0]["valueReference"]["reference"],
        format!("DocumentReference/{}", doc("a2", "ALEX"))
    );
    assert_eq!(
        ids_of(&stream, "DocumentReference"),
        ["b2", "a3", "a6", "a8", "a2"].map(|t| doc(t, "ALEX"))
    );
    assert_eq!(h.server.count("doc360"), 5);

    // A claim points at no statement the stream does not carry.
    let h = Household::new();
    let earlier = fixture("documents.json").replace("2026-03-23", "2026-03-18");
    h.server
        .fail_on("documents-aggregate-api/v1/graphql", 200, "application/json", &earlier);
    let stream = export(&h, "ALEX", &["--since", "2026-03-19"]);
    assert_eq!(
        named(&stream, "ExplanationOfBenefit", "cSYNC55511862")["supportingInfo"],
        json!([])
    );
    assert_eq!(h.server.count("doc360"), 4);

    // A claim in process is skipped whatever the day.
    let h = Household::new();
    let stream = export(&h, "SAM", &["--since", "2026-11-10"]);
    assert_eq!(stream.last().unwrap()["skipped"][0]["code"], "in_process");
    assert!(of_type(&stream, "ExplanationOfBenefit").is_empty());
    assert!(of_type(&stream, "DocumentReference").is_empty());
}

/// A member, a coverage and a claim have one id from either source, so a
/// store holds one record of each.
#[test]
fn the_sites_ids_are_the_statements() {
    let h = Household::new();
    let site = ids(&h.json(&["patients", "list"]));
    for (day, first, shared, only) in [
        // A suffix beside the claim id, and one in it too.
        (
            "2026-05-07",
            "SAM",
            vec!["cSYNC55526407", "cSYNC55520613"],
            vec!["cSYNC55527315"],
        ),
        // A pharmacy's 17 digits.
        ("2026-04-15", "ALEX", vec!["rx55500000000000237"], vec!["cSYNC55511903"]),
        // No suffix.
        ("2026-10-09", "JORDAN", vec!["cSYNC55531742"], vec![]),
    ] {
        let path = format!("{}/tests/fixtures/eob/{day}-statement.txt", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).unwrap();
        let file = StatementFile::new(PathBuf::from("s.pdf"), text.as_bytes(), uhc::eob::parse(&text));
        let book = Book::new(vec![file]);
        let members = book.members();
        assert_eq!(members.len(), 1, "{day}");
        let m = &members[0];
        assert_eq!(m.id, id(first));
        assert!(site.contains(&m.id), "{site:?}");
        let stream = export(&h, first, &[]);
        assert_eq!(ids_of(&stream, "Coverage"), [uhc::fhir::coverage_id(m)]);
        assert_eq!(
            named(&stream, "Patient", &m.id)["identifier"],
            uhc::fhir::patient(m, None)["identifier"]
        );

        let on_site = ids_of(&stream, "ExplanationOfBenefit");
        let (both, printed_only): (Vec<_>, Vec<_>) = book.claims().into_iter().partition(|c| on_site.contains(&c.id));
        let both_ids: Vec<&str> = both.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(both_ids, shared, "{day}");
        // The rest are the statement's alone: the site does not list them.
        let rest: Vec<&str> = printed_only.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(rest, only, "{day}");
        for c in &both {
            let eob = named(&stream, "ExplanationOfBenefit", &c.id);
            assert_eq!(eob["patient"]["reference"], format!("Patient/{}", c.patient));
            assert_eq!(eob["item"].as_array().unwrap().len(), c.claim.lines.len(), "{}", c.id);
            let printed = uhc::fhir::explanation_of_benefit(c, "cov-synthetic", &[], None);
            for field in ["total", "billablePeriod", "adjudication"] {
                assert_eq!(eob[field], printed[field], "{} {field}", c.id);
            }
        }
    }
}
