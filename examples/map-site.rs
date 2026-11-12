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
//! The session stays in memory and is dropped at exit; nothing is written.
//! The kept profile (`~/Library/Application Support/piekstra.uhc/browser-profile`)
//! keeps the site's own cookies, as every family CLI's browser sign-in does.
//!
//! ```sh
//! cargo run -q --example map-site -- --minutes 4
//! ```

use std::collections::HashSet;
use std::time::Duration;

use pk_cli_browser::{Capture, HostRule, Launch, RequestKind, DEFAULT_TIMEOUT};
use pk_cli_core::CliError;
use uhc::shape::{redact_path, shape_lines};

const HOST: &str = "member.uhc.com";
const DOMAIN: &str = "uhc.com";
const START: &str = "https://member.uhc.com/myuhc";
/// Re-reads at most, and the pause between two.
const MAX_READS: usize = 60;
const PAUSE: Duration = Duration::from_millis(1500);

/// Signed in: the page offers a way to sign out. A signed-out page offers
/// only a sign-in.
const SIGNED_IN: &str = r#"(() => {
  const out = [...document.querySelectorAll('a, button')].some(e =>
    /^\s*(sign|log)\s?out\s*$/i.test(e.textContent || '') ||
    /(logout|signout|sign-out|log-out)/i.test(e.getAttribute('href') || ''));
  return out ? 'signed-in' : null;
})()"#;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {}", e.message());
        std::process::exit(e.exit_code());
    }
}

fn minutes() -> Result<u64, CliError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.as_slice() {
        [] => Ok(4),
        [flag, n] if flag == "--minutes" => n
            .parse()
            .ok()
            .filter(|n| (1..=15).contains(n))
            .ok_or_else(|| CliError::Usage("--minutes takes 1 to 15".into())),
        _ => Err(CliError::Usage("usage: map-site [--minutes N]".into())),
    }
}

fn run() -> Result<(), CliError> {
    let minutes = minutes()?;
    let capture = Capture::new("UnitedHealthcare", HostRule::exact(HOST)?)
        .expression(SIGNED_IN)
        .cookie_jar(DOMAIN)
        .with_user_agent()
        .request_log(DOMAIN, Duration::from_secs(minutes * 60))
        .hint(format!(
            "your HealthSafe ID password and code; then, for {minutes} minutes, open Claims, one claim's \
             details, Benefits, Documents and your ID card until the window closes"
        ));
    let captured = pk_cli_browser::capture(Launch::new("uhc").url(START), &capture, DEFAULT_TIMEOUT)?;

    let mut rows: Vec<(String, String, String, &'static str, String, String)> = captured
        .request_log()
        .iter()
        .map(|e| {
            (
                e.host().to_string(),
                redact_path(e.path()),
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

    let Some(cookies) = captured.cookie_header() else {
        println!("\n# no uhc.com cookie was captured; nothing re-read");
        return Ok(());
    };
    let agent = captured.user_agent().unwrap_or("Mozilla/5.0").to_string();
    let client = pk_cli_http::builder("uhc", env!("CARGO_PKG_VERSION"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| CliError::Other(format!("building the client: {}", e.without_url())))?;

    let mut read = HashSet::new();
    for e in captured.request_log() {
        let json = e.mime().is_some_and(|m| m.contains("json"));
        let api = matches!(e.kind(), RequestKind::Xhr | RequestKind::Fetch);
        let on_site = e.host() == DOMAIN || e.host().ends_with(".uhc.com");
        if e.method() != "GET" || !api || !json || e.status() != Some(200) || !on_site {
            continue;
        }
        let shown = format!("{} {}", e.host(), redact_path(e.path()));
        if !read.insert(shown.clone()) {
            continue;
        }
        if read.len() > MAX_READS {
            println!("\n# stopped at {MAX_READS} re-reads");
            break;
        }
        std::thread::sleep(PAUSE);
        let answer = client
            .get(format!("https://{}{}", e.host(), e.path()))
            .header(reqwest::header::COOKIE, cookies.expose())
            .header(reqwest::header::USER_AGENT, &agent)
            .header(reqwest::header::ACCEPT, "application/json")
            .send();
        let answer = match answer {
            Ok(a) => a,
            Err(err) => {
                let why = if err.is_timeout() { "timed out" } else { "no answer" };
                println!("\n## GET {shown}: {why}");
                continue;
            }
        };
        let status = answer.status().as_u16();
        let mime = answer
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .split(';')
            .next()
            .unwrap_or("-")
            .to_string();
        println!("\n## GET {shown}: HTTP {status} {mime}");
        if status != 200 || !mime.contains("json") {
            continue;
        }
        match answer.json::<serde_json::Value>() {
            Ok(doc) => {
                for line in shape_lines(&doc) {
                    println!("  {line}");
                }
            }
            Err(_) => println!("  (not JSON)"),
        }
    }
    Ok(())
}
