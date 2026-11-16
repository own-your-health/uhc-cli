//! Map the member site's own API, for docs/api.md. Not part of `uhc`.
//!
//! Opens Chrome in uhc's kept browser profile at member.uhc.com. The owner
//! signs in there (HealthSafe ID password and code) and then uses the site:
//! Claims, one claim's details, Benefits (deductible and out-of-pocket),
//! Documents and the ID card, until the window closes after `--minutes`.
//!
//! It prints, on stdout:
//!
//! - each endpoint the site's pages called on uhc.com hosts, once: method,
//!   host, path with every id as `{id}`, kind, status and content type (the
//!   request log holds no header, body or query);
//! - for each JSON GET that answered 200, one re-read of it with the
//!   session, one request at a time and 1.5 s apart, printed as its shape:
//!   keys and types only, never a value (`uhc::shape`).
//!
//! Nothing prints until every re-read is done. A key or path segment
//! holding one of the household's names, in any case, prints as `<key>` or
//! `{id}`. The names are gathered in memory and never printed (counts only):
//! from the name fields of every answer re-read (`Redactor::learn_names`),
//! from this Mac account's full name (`id -F`), and from
//! `~/.config/uhc/redact-words` when the owner wrote one (one per line,
//! mode 0600; a file others can read stops it, exit 2).
//!
//! The window waits up to `--wait` minutes (default 60) for the sign-in,
//! then stays open `--minutes` (default 4) for using the site.
//!
//! The session stays in memory and is dropped at exit; nothing is written.
//! The kept profile (`~/Library/Application Support/piekstra.uhc/browser-profile`)
//! keeps the site's own cookies, as every family CLI's browser sign-in does.
//!
//! ```sh
//! cargo run -q --example map-site -- --minutes 4 --wait 60
//! ```

use std::collections::HashSet;
use std::time::Duration;

use pk_cli_browser::{Capture, HostRule, Launch, RequestKind};
use pk_cli_core::CliError;
use uhc::shape::Redactor;
use uhc::site::{DOMAIN, HINT, HOST, LABEL, SIGNED_IN, START_URL};

/// Re-reads at most, and the pause between two.
const MAX_READS: usize = 60;
const PAUSE: Duration = Duration::from_millis(1500);

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {}", e.message());
        std::process::exit(e.exit_code());
    }
}

/// `--minutes` (using the site, 1 to 15) and `--wait` (for the sign-in, 1
/// to 120), in minutes.
fn args() -> Result<(u64, u64), CliError> {
    let usage = || CliError::Usage("usage: map-site [--minutes 1..15] [--wait 1..120]".into());
    let (mut minutes, mut wait) = (4, 60);
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let n: u64 = args.next().and_then(|n| n.parse().ok()).ok_or_else(usage)?;
        match flag.as_str() {
            "--minutes" if (1..=15).contains(&n) => minutes = n,
            "--wait" if (1..=120).contains(&n) => wait = n,
            _ => return Err(usage()),
        }
    }
    Ok((minutes, wait))
}

/// This Mac account's full name, for the deny words; never printed.
fn account_name() -> String {
    std::process::Command::new("id")
        .arg("-F")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// One re-read: where, and what came back.
struct Reread {
    host: String,
    path: String,
    answer: Result<(u16, String, Option<serde_json::Value>), &'static str>,
}

fn run() -> Result<(), CliError> {
    let (minutes, wait) = args()?;
    let path = Redactor::default_path().ok_or_else(|| CliError::Usage("no home directory".into()))?;
    let mut redact = Redactor::from_file_if_present(&path)?;
    let from_file = redact.len();
    let from_account = redact.add_names(&account_name());
    eprintln!(
        "{from_file} deny word(s) from {}, {from_account} from this account's name",
        path.display()
    );
    let capture = Capture::new(LABEL, HostRule::exact(HOST)?)
        .expression(SIGNED_IN)
        .cookie_jar(DOMAIN)
        .with_user_agent()
        .request_log(DOMAIN, Duration::from_secs(minutes * 60))
        .hint(format!(
            "{HINT}; then, for {minutes} minutes, open Claims, one claim's details, Benefits, Documents \
             and your ID card until the window closes"
        ));
    let timeout = Duration::from_secs((wait + minutes) * 60);
    let captured = pk_cli_browser::capture(Launch::new(uhc::cli::BIN).url(START_URL), &capture, timeout)?;

    let rereads = match captured.cookie_header() {
        Some(cookies) => reread(&captured, cookies.expose(), &redact)?,
        None => Vec::new(),
    };
    let learned: usize = rereads
        .iter()
        .filter_map(|r| r.answer.as_ref().ok().and_then(|(_, _, doc)| doc.as_ref()))
        .map(|doc| redact.learn_names(doc))
        .sum();
    eprintln!(
        "{learned} more deny word(s) from the answers' name fields; {} in all",
        redact.len()
    );

    let mut rows: Vec<(String, String, String, &'static str, String, String)> = captured
        .request_log()
        .iter()
        .map(|e| {
            (
                e.host().to_string(),
                redact.redact_path(e.path()),
                e.method().to_string(),
                e.kind().as_str(),
                e.status().map_or("-".into(), |s| s.to_string()),
                e.mime().unwrap_or("-").to_string(),
            )
        })
        .collect();
    rows.sort();
    rows.dedup();
    println!("# {} endpoints (method host path kind status type)", rows.len());
    for (host, path, method, kind, status, mime) in &rows {
        println!("{method} {host} {path} {kind} {status} {mime}");
    }
    if captured.cookie_header().is_none() {
        println!("\n# no uhc.com cookie was captured; nothing re-read");
    }
    for r in &rereads {
        let shown = format!("{} {}", r.host, redact.redact_path(&r.path));
        match &r.answer {
            Err(why) => println!("\n## GET {shown}: {why}"),
            Ok((status, mime, doc)) => {
                println!("\n## GET {shown}: HTTP {status} {mime}");
                match doc {
                    Some(doc) => redact.shape_lines(doc).iter().for_each(|l| println!("  {l}")),
                    None if *status == 200 && mime.contains("json") => println!("  (not JSON)"),
                    None => {}
                }
            }
        }
    }
    Ok(())
}

/// Re-reads each JSON GET the pages made on uhc.com hosts that answered
/// 200, once per path (as `redact` shows it), one at a time and `PAUSE`
/// apart, `MAX_READS` at most. The answers stay in memory.
fn reread(captured: &pk_cli_browser::Captured, cookies: &str, redact: &Redactor) -> Result<Vec<Reread>, CliError> {
    let agent = captured.user_agent().unwrap_or("Mozilla/5.0").to_string();
    let client = pk_cli_http::builder("uhc", env!("CARGO_PKG_VERSION"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| CliError::Other(format!("building the client: {}", e.without_url())))?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for e in captured.request_log() {
        let json = e.mime().is_some_and(|m| m.contains("json"));
        let api = matches!(e.kind(), RequestKind::Xhr | RequestKind::Fetch);
        let on_site = e.host() == DOMAIN || e.host().ends_with(".uhc.com");
        if e.method() != "GET" || !api || !json || e.status() != Some(200) || !on_site {
            continue;
        }
        if !seen.insert(format!("{} {}", e.host(), redact.redact_path(e.path()))) {
            continue;
        }
        if out.len() == MAX_READS {
            eprintln!("stopped at {MAX_READS} re-reads");
            break;
        }
        std::thread::sleep(PAUSE);
        let answer = client
            .get(format!("https://{}{}", e.host(), e.path()))
            .header(reqwest::header::COOKIE, cookies)
            .header(reqwest::header::USER_AGENT, &agent)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .map_err(|err| if err.is_timeout() { "timed out" } else { "no answer" })
            .map(|a| {
                let status = a.status().as_u16();
                let mime = a
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-")
                    .split(';')
                    .next()
                    .unwrap_or("-")
                    .trim()
                    .to_string();
                let doc = (status == 200 && mime.contains("json"))
                    .then(|| a.json::<serde_json::Value>().ok())
                    .flatten();
                (status, mime, doc)
            });
        out.push(Reread {
            host: e.host().to_string(),
            path: e.path().to_string(),
            answer,
        });
    }
    Ok(out)
}
