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

#[test]
fn patients_list() {
    let h = Household::new();
    let v = h.json(&["patients", "list"]);
    assert_eq!(v["schema"], "patient-list/v1");
    assert_eq!(ids(&v), [id("ALEX"), id("JORDAN"), id("SAM")]);
    let sam = &v["items"][2];
    assert_eq!(sam["label"], "Sam");
    assert_eq!(sam["name"], "Sam Example");
    assert_eq!(sam["member_role"], "dependent");
    assert_eq!(sam["coverage_relationship"], "spouse");
    assert_eq!(sam["member_id"], "SYNM000001");
    assert_eq!(sam["claims"], 4);
    assert_eq!(sam["statements"], 2);
    assert_eq!(sam["latest_statement"], "2026-08-16");
    assert_eq!(v["items"][0]["relationship"], "self");
    // Each request carries the session and the browser's user agent.
    for r in h.server.seen() {
        assert_eq!(r.headers["user-agent"], AGENT, "{}", r.target);
        assert!(r.headers["cookie"].contains("dummy_session=abc"), "{}", r.target);
        assert_eq!(r.method, "POST");
    }
    assert_eq!(h.server.seen().len(), 2, "the claims and the documents");
    // The variables are dates only: no member or plan id goes out.
    for r in h.server.seen() {
        let body: Value = serde_json::from_str(&r.body).unwrap();
        for key in body["variables"].as_object().unwrap().keys() {
            assert!(key.ends_with("tartDate") || key.ends_with("topDate"), "{key}");
        }
    }
}

#[test]
fn claims_list_and_get() {
    let h = Household::new();
    let v = h.json(&["claims", "list"]);
    assert_eq!(v["schema"], "claim-list/v1");
    let all = ids(&v);
    assert_eq!(all.len(), 9, "{all:?}");
    // The statement fixture's claims, under the statement's ids.
    for statement_id in ["cSYNC00000006", "cSYNC00000008", "rx00000000000000000"] {
        assert!(all.contains(&statement_id.to_string()), "{statement_id}: {all:?}");
    }
    assert_eq!(all[0], "cSYNC11111111", "newest service first");
    let rx = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "rx55500000000000000")
        .unwrap();
    assert_eq!(rx["reversed"], true);
    assert_eq!(rx["type"], "pharmacy");
    assert_eq!(rx["lines"], 1);
    let office = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "cSYNC00000006")
        .unwrap();
    assert_eq!(office["claim_number"], "SYNC00000006");
    assert_eq!(office["billed"]["amount"], "200.00");
    assert_eq!(office["you_owe"]["amount"], "35.00");
    assert_eq!(office["network"], "in");
    assert_eq!(office["processed"], "2026-08-15");
    assert!(office.get("lines").is_none(), "a summary has no lines: {office}");

    let sam = h.json(&["claims", "list", "--patient", "Sam", "--since", "2026-08-10"]);
    assert_eq!(ids(&sam), ["cSYNC33333333", "cSYNC00000008"]);
    assert_eq!(h.server.count("claimDetails"), 0, "claims list reads no detail");

    let c = h.json(&["claims", "get", "cSYNC00000008"]);
    assert_eq!(c["schema"], "claim/v1");
    assert_eq!(c["type"], "professional");
    assert_eq!(c["document"], "00000000-0000-4000-8000-000000000001");
    let lines = c["lines"].as_array().unwrap();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0]["procedure"]["code"], "70450");
    assert_eq!(lines[0]["codes"], json!(["D1"]));
    assert_eq!(c["allowed"]["amount"], "270.00");
    assert!(c["codes"]["UG"].is_string());
    assert_eq!(h.server.count("claimDetails"), 1);

    let e = h.run(&["claims", "get", "cNOSUCHCLAIM"]).unwrap_err();
    assert_eq!(e.exit_code(), 4);
}

#[test]
fn accumulators() {
    let h = Household::new();
    let v = h.json(&["accumulators"]);
    assert_eq!(v["schema"], "accumulator-list/v1");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 16);
    let alex = h.json(&["accumulators", "--patient", "Alex", "--year", "2026"]);
    let rows = alex["items"].as_array().unwrap();
    assert_eq!(rows.len(), 8, "Alex's and the family's");
    let ded = rows
        .iter()
        .find(|r| r["label"] == "Alex" && r["network"] == "in" && r["limit"] == "deductible")
        .unwrap();
    assert_eq!(ded["maximum"]["amount"], "1250.00");
    assert_eq!(ded["used"]["amount"], "50.00");
    assert_eq!(ded["remaining"]["amount"], "1200.00");
    assert_eq!(ded["met"], false);
    assert!(ded.get("document").is_none());
    assert_eq!(h.json(&["accumulators", "--year", "2025"])["items"], json!([]));
}

#[test]
fn documents_list_and_download() {
    let h = Household::new();
    let v = h.json(&["documents", "list"]);
    assert_eq!(v["schema"], "document-list/v1");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let eob = items
        .iter()
        .find(|d| d["id"] == format!("doc-00000000-0000-4000-8000-000000000001-{}", id("SAM")))
        .unwrap();
    assert_eq!(eob["category"], "eob");
    assert_eq!(eob["date"], "2026-08-16");
    assert_eq!(
        eob["file"],
        "2026-08-16_uhc-eob_00000000-0000-4000-8000-000000000001.pdf"
    );
    assert_eq!(eob["name"], "UnitedHealthcare Explanation of Benefits 2026-08-16, Sam");

    let dir = tmp("downloads");
    let one = h.json(&[
        "documents",
        "download",
        "00000000-0000-4000-8000-000000000005",
        "-o",
        dir.to_str().unwrap(),
    ]);
    assert_eq!(one["schema"], "document-download/v1");
    let saved = PathBuf::from(one["path"].as_str().unwrap());
    assert_eq!(std::fs::read(&saved).unwrap(), pdf());
    let pdf_gets = h
        .server
        .seen()
        .into_iter()
        .filter(|r| r.method == "GET")
        .collect::<Vec<_>>();
    assert_eq!(pdf_gets.len(), 1);
    assert!(
        pdf_gets[0].target.ends_with("?documentClass=u_prior_auth_doc"),
        "{}",
        pdf_gets[0].target
    );
    assert!(pdf_gets[0].headers["accept"].contains("application/pdf"));
    // Onto the file it saved: refused before any download.
    let again = h.run(&[
        "documents",
        "download",
        "00000000-0000-4000-8000-000000000005",
        "-o",
        saved.to_str().unwrap(),
    ]);
    assert_eq!(again.unwrap_err().exit_code(), 2);

    let all = h.json(&["documents", "download", "--all", "-o", dir.to_str().unwrap()]);
    assert_eq!(all["schema"], "document-download-batch/v1");
    assert_eq!(all["count"], 4);
    assert_eq!(all["skipped"].as_array().unwrap().len(), 1, "the one saved before");
    let gets = || h.server.seen().into_iter().filter(|r| r.method == "GET").count();
    assert_eq!(gets(), 5);
    // A re-run reads no document again.
    let rerun = h.json(&["documents", "download", "--all", "-o", dir.to_str().unwrap()]);
    assert_eq!(rerun["count"], 0);
    assert_eq!(rerun["skipped"].as_array().unwrap().len(), 5);
    assert!(rerun["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["code"] == "exists"));
    assert_eq!(gets(), 5);
    assert_eq!(h.run(&["documents", "download", "00000"]).unwrap_err().exit_code(), 2);
}

fn of_type<'a>(stream: &'a [Value], kind: &str) -> Vec<&'a Value> {
    stream.iter().filter(|l| l["resourceType"] == kind).collect()
}

#[test]
fn records_export_carries_the_sites_codes() {
    let h = Household::new();
    let (code, stream) = h.stream(&["records", "export", "--patient", "Sam", "--ndjson"]);
    assert_eq!(code, 0);
    let header = &stream[0];
    assert_eq!(header["schema"], "records-stream/v1");
    assert_eq!(header["method"], "portal");
    assert_eq!(header["patient"]["id"], id("SAM"));
    let trailer = stream.last().unwrap();
    assert_eq!(trailer["complete"], true, "{trailer}");

    let patient = &of_type(&stream, "Patient")[0];
    assert_eq!(patient["birthDate"], "1982-01-01");
    assert_eq!(patient["gender"], "female");
    assert_eq!(patient["name"][0]["family"], "Example");
    assert_eq!(patient["meta"]["source"], "uhc:member-site");
    let coverage = &of_type(&stream, "Coverage")[0];
    assert_eq!(
        coverage["period"],
        json!({ "start": "2026-01-01", "end": "2026-12-31" })
    );
    assert_eq!(coverage["relationship"]["coding"][0]["code"], "spouse");

    let eobs = of_type(&stream, "ExplanationOfBenefit");
    let eob_ids: Vec<&str> = eobs.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(
        eob_ids,
        ["cSYNC33333333", "cSYNC00000008", "rx00000000000000000", "cSYNC00000006"]
    );
    let radiology = eobs.iter().find(|e| e["id"] == "cSYNC00000008").unwrap();
    assert_eq!(radiology["type"]["coding"][0]["code"], "professional");
    assert_eq!(radiology["created"], "2026-08-16");
    let first = &radiology["item"][0]["productOrService"]["coding"][0];
    assert_eq!(first["system"], "http://www.ama-assn.org/go/cpt");
    assert_eq!(first["code"], "70450");
    let second = &radiology["item"][1]["productOrService"]["coding"][0];
    assert_eq!(
        second["system"],
        "https://www.cms.gov/Medicare/Coding/HCPCSReleaseCodeSets"
    );
    let notes: Vec<&str> = radiology["processNote"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["text"].as_str().unwrap())
        .collect();
    assert!(notes.iter().any(|n| n.starts_with("D1 -- ")), "{notes:?}");
    assert_eq!(radiology["adjudication"][0]["reason"]["coding"][0]["code"], "innetwork");
    assert_eq!(
        radiology["supportingInfo"][0]["valueReference"]["reference"],
        format!(
            "DocumentReference/doc-00000000-0000-4000-8000-000000000001-{}",
            id("SAM")
        )
    );
    assert!(radiology.get("benefitBalance").is_none());
    let rx = eobs.iter().find(|e| e["id"] == "rx00000000000000000").unwrap();
    assert_eq!(rx["type"]["coding"][0]["code"], "pharmacy");
    assert_eq!(rx["status"], "active");

    let docs = of_type(&stream, "DocumentReference");
    assert_eq!(docs.len(), 3, "Sam's two EOBs and the health statement");
    let eob_doc = docs
        .iter()
        .find(|d| d["id"] == format!("doc-00000000-0000-4000-8000-000000000001-{}", id("SAM")))
        .unwrap();
    let related: Vec<&str> = eob_doc["context"]["related"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reference"].as_str().unwrap())
        .collect();
    assert_eq!(
        related,
        [
            "ExplanationOfBenefit/cSYNC00000008",
            "ExplanationOfBenefit/cSYNC00000006"
        ]
    );
    assert_eq!(of_type(&stream, "Binary").len(), 3);
    // Three claims' details (the pharmacy claim has none) and three PDFs.
    assert_eq!(h.server.count("claimDetails"), 3);
    assert_eq!(h.server.seen().iter().filter(|r| r.method == "GET").count(), 3);
}

/// One claim is one record whichever source it came from: the statement
/// fixture's claims and member read through the files source carry the
/// ids the site's export gives them.
#[test]
fn the_ids_are_the_statements() {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/eob/2026-08-20-statement.txt"
    ))
    .unwrap();
    let book = Book::new(vec![StatementFile::new(
        "statement.pdf".into(),
        text.as_bytes(),
        uhc::eob::parse(&text),
    )]);
    let statement_claims: Vec<String> = book.claims().into_iter().map(|c| c.id).collect();
    let statement_members: Vec<String> = book.members().into_iter().map(|m| m.id).collect();

    let h = Household::new();
    let (_, stream) = h.stream(&["records", "export", "--patient", "Sam", "--ndjson"]);
    let site_claims: Vec<String> = of_type(&stream, "ExplanationOfBenefit")
        .iter()
        .map(|e| e["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(statement_claims.len(), 3);
    for c in &statement_claims {
        assert!(site_claims.contains(c), "{c} is not among {site_claims:?}");
    }
    assert_eq!(statement_members, [id("SAM")]);
    assert_eq!(of_type(&stream, "Patient")[0]["id"], id("SAM"));
    assert_eq!(
        of_type(&stream, "Coverage")[0]["id"],
        uhc::fhir::coverage(&book.members()[0])["id"]
    );
}

#[test]
fn records_export_since_reads_only_what_changed() {
    let h = Household::new();
    let (code, stream) = h.stream(&[
        "records",
        "export",
        "--patient",
        "Sam",
        "--since",
        "2026-08-16",
        "--ndjson",
    ]);
    assert_eq!(code, 0);
    assert_eq!(stream[0]["since"], "2026-08-16");
    let eobs: Vec<&str> = of_type(&stream, "ExplanationOfBenefit")
        .iter()
        .map(|e| e["id"].as_str().unwrap())
        .collect();
    assert_eq!(eobs, ["cSYNC33333333", "cSYNC00000008"]);
    assert_eq!(h.server.count("claimDetails"), 2);
    // The EOB of 2026-08-16 and the health statement of 2026-08-20.
    assert_eq!(of_type(&stream, "DocumentReference").len(), 2);
}

#[test]
fn a_claim_in_process_is_skipped() {
    let h = Household::new();
    let (code, stream) = h.stream(&["records", "export", "--patient", "Jordan", "--ndjson"]);
    assert_eq!(code, 0);
    let trailer = stream.last().unwrap();
    let skipped = trailer["skipped"].as_array().unwrap();
    assert!(
        skipped
            .iter()
            .any(|s| s["code"] == "in_process" && s["id"] == "cSYNC11111111"),
        "{trailer}"
    );
    let eobs = of_type(&stream, "ExplanationOfBenefit");
    assert_eq!(eobs.len(), 1);
    assert_eq!(eobs[0]["type"]["coding"][0]["code"], "institutional");
    assert_eq!(
        eobs[0]["item"][0]["revenue"]["coding"][0]["system"],
        "https://www.nubc.org/CodeSystem/RevenueCodes"
    );
}

#[test]
fn the_dental_claim_is_oral() {
    let h = Household::new();
    let (_, stream) = h.stream(&["records", "export", "--patient", "Alex", "--ndjson"]);
    let eobs = of_type(&stream, "ExplanationOfBenefit");
    let dental = eobs.iter().find(|e| e["id"] == "c000000000003").unwrap();
    assert_eq!(dental["type"]["coding"][0]["code"], "oral");
    assert_eq!(
        dental["item"][0]["productOrService"]["coding"][0]["system"],
        "http://ada.org/cdt"
    );
    assert_eq!(dental["item"][0]["bodySite"]["text"], "Tooth 01-32");
    assert_eq!(dental["adjudication"][0]["reason"]["coding"][0]["code"], "outofnetwork");
    let rx = eobs.iter().find(|e| e["id"] == "rx55500000000000000").unwrap();
    assert_eq!(rx["status"], "cancelled");
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
    // The claims and the documents answer; the first detail is refused.
    h.server
        .fail_after(2, 401, "application/json", &fixture("unauthorized.json"));
    let (code, stream) = h.stream(&["records", "export", "--patient", "Sam", "--ndjson"]);
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

#[test]
fn records_export_to_a_file_is_refused_before_any_request() {
    let h = Household::new();
    let dir = tmp("out");
    let e = h
        .run(&["records", "export", "--patient", "Sam", "-o", dir.to_str().unwrap()])
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
