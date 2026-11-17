//! The member site, member.uhc.com: what `auth login --browser` reads out of
//! the Chrome window, and the one read that confirms a session before it is
//! stored.
//!
//! The site sets visitor cookies (`AWSALBTG`, `AWSALBTGCORS`,
//! `X-DashCafe-Canary`) before anyone signs in, so a cookie in the jar proves
//! nothing. The capture waits for [`SESSION_COOKIE`], which the site sets
//! only as the sign-in completes, then [`confirm_session`] makes one read
//! with the jar: a one-field GraphQL query that a signed-in member gets data
//! for and anyone else a 401 (docs/api.md).

use std::io::Read;
use std::time::Duration;

use pk_cli_browser::{BrowserError, Capture, Captured, Confirm, HostRule};
use pk_cli_core::CliError;
use reqwest::header::{HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE};

use crate::cli::BIN;

/// The provider's name in the browser crate's status lines and errors.
pub const LABEL: &str = "UnitedHealthcare";
/// The host whose pages the capture reads.
pub const HOST: &str = "member.uhc.com";
/// The domain whose whole cookie jar is the session.
pub const DOMAIN: &str = "uhc.com";
/// Where the Chrome window opens. A signed-out visitor is sent on to the
/// HealthSafe ID sign-in from here.
pub const START_URL: &str = "https://member.uhc.com/myuhc";
/// What signing in involves, for the opening status line.
pub const HINT: &str = "your HealthSafe ID password and code";

/// A cookie member.uhc.com sets only once a sign-in completes, so the
/// capture asks [`confirm_session`] once rather than on every poll. The
/// signed-in pages carry no sign-out link a page script can see: it is
/// inside the site header's web component.
pub const SESSION_COOKIE: &str = "rally-token";

/// The member-site read that shows whether a cookie jar is a signed-in
/// member's: an `https` URL on a uhc.com host, sent [`SESSION_CHECK_BODY`]
/// with `POST`.
pub const SESSION_CHECK_URL: &str = "https://member.uhc.com/ce-claims-aggregate-api/graphql?q=sessionCheck";

/// The GraphQL query [`SESSION_CHECK_URL`] is sent: the smallest read of the
/// member's own plan balances. It reads; nothing on the site changes.
pub const SESSION_CHECK_BODY: &str = r#"{"operationName":"sessionCheck","variables":{},"query":"query sessionCheck { spendingOverviews { __typename } }"}"#;

/// The bound on the whole read, connecting included. The browser crate's poll
/// (and Ctrl-C) waits for it.
const TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an answer's body read for [`judge`].
const MAX_BODY: u64 = 1024 * 1024;

/// What `auth login --browser` reads: a signed-in page on [`HOST`], the
/// whole [`DOMAIN`] jar and the browser's user agent. With `verify`, the tab
/// stays open until [`confirm_candidate`] says the jar is a signed-in one.
pub fn capture(verify: bool) -> Result<Capture, BrowserError> {
    let capture = Capture::new(LABEL, HostRule::exact(HOST)?)
        .cookie(START_URL, SESSION_COOKIE)
        .cookie_jar(DOMAIN)
        .with_user_agent()
        .hint(HINT);
    Ok(if verify {
        capture.confirm(confirm_candidate)
    } else {
        capture
    })
}

/// The capture's confirm hook: [`confirm_session`] on the candidate's jar.
pub fn confirm_candidate(candidate: &Captured) -> Confirm {
    match candidate.cookie_header() {
        Some(header) => confirm_session(header.expose(), candidate.user_agent()),
        None => Confirm::NotYet("the browser holds no uhc.com cookie yet".into()),
    }
}

/// One read of [`SESSION_CHECK_URL`] with `cookie_header`, judged by
/// [`judge`]. A 429, a 5xx, a 403 with an HTML page, no answer or an answer
/// that does not arrive whole is `Undecided`. A redirect is `NotYet`: the
/// site sends a visitor to its sign-in. `user_agent` is the browser's, for a
/// session captured from one. Without it the request names `uhc`, and the
/// site's edge answers 403 with an HTML page (docs/api.md).
pub fn confirm_session(cookie_header: &str, user_agent: Option<&str>) -> Confirm {
    confirm_at(SESSION_CHECK_URL, cookie_header, user_agent)
}

/// How the answer from [`SESSION_CHECK_URL`] shows a signed-in member.
///
/// `Ready` for a JSON answer whose `data.spendingOverviews` is an object:
/// only a signed-in member's query resolves. `NotYet` for a 401 (the site's
/// "Credentials missing.") or any other 4xx. `Confirm::undecided` for a 2xx
/// that is not JSON or holds no `data.spendingOverviews` (a GraphQL error).
/// It sees only answers that [`confirm_session`] has not already decided: no
/// 429, 5xx, 403 with an HTML page or redirect. `body` is at most 1 MiB.
pub fn judge(status: u16, content_type: Option<&str>, body: &[u8]) -> Confirm {
    let what = format!("POST {}", path_of(SESSION_CHECK_URL));
    if (400..500).contains(&status) {
        return Confirm::NotYet(format!("{what} answered HTTP {status}: not signed in"));
    }
    if !(200..300).contains(&status) {
        return Confirm::undecided(format!("{what} answered HTTP {status}"));
    }
    if !content_type.is_some_and(|t| t.contains("json")) {
        return Confirm::undecided(format!("{what} answered HTTP {status} without JSON"));
    }
    match serde_json::from_slice::<serde_json::Value>(body) {
        Ok(doc)
            if doc
                .pointer("/data/spendingOverviews")
                .is_some_and(serde_json::Value::is_object) =>
        {
            Confirm::Ready
        }
        Ok(_) => Confirm::undecided(format!("{what} answered without the plan balances")),
        Err(_) => Confirm::undecided(format!("{what} answered HTTP {status} with JSON that does not parse")),
    }
}

/// [`confirm_session`]'s read of `url`, apart from the constant so the tests
/// can point it at a local server.
fn confirm_at(url: &str, cookie_header: &str, user_agent: Option<&str>) -> Confirm {
    let what = format!("POST {}", path_of(url));
    let client = match client(user_agent) {
        Ok(client) => client,
        Err(e) => return Confirm::Fail(e),
    };
    let Ok(mut cookie) = HeaderValue::from_str(cookie_header) else {
        return Confirm::Fail(CliError::Other(
            "the session's Cookie header is not a valid HTTP header value".into(),
        ));
    };
    cookie.set_sensitive(true);
    let answer = match client
        .post(url)
        .header(COOKIE, cookie)
        .header(ACCEPT, "application/json")
        .header(CONTENT_TYPE, "application/json")
        .body(SESSION_CHECK_BODY)
        .send()
    {
        Ok(answer) => answer,
        Err(e) if e.is_timeout() => return Confirm::undecided(format!("{what} timed out")),
        Err(_) => return Confirm::undecided(format!("{what} got no answer")),
    };
    if let Some(said) = pk_cli_http::inconclusive(&answer) {
        return Confirm::Undecided {
            reason: format!("{what} answered HTTP {}", said.status),
            retry_after: said.retry_after,
        };
    }
    let status = answer.status().as_u16();
    if answer.status().is_redirection() {
        return Confirm::NotYet(format!(
            "{what} answered with a redirect (HTTP {status}): not signed in"
        ));
    }
    let content_type = answer
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut body = Vec::new();
    if answer.take(MAX_BODY + 1).read_to_end(&mut body).is_err() {
        return Confirm::undecided(format!("{what} answered HTTP {status} and its body did not arrive"));
    }
    if body.len() as u64 > MAX_BODY {
        return Confirm::undecided(format!("{what} answered with more than 1 MiB"));
    }
    judge(status, content_type.as_deref(), &body)
}

/// The confirm read's client: no redirects followed, bounded in time.
fn client(user_agent: Option<&str>) -> Result<reqwest::blocking::Client, CliError> {
    let mut builder = pk_cli_http::builder(BIN, env!("CARGO_PKG_VERSION"))
        .redirect(reqwest::redirect::Policy::none())
        .timeout(TIMEOUT)
        .connect_timeout(CONNECT_TIMEOUT);
    if let Some(agent) = user_agent {
        builder = builder.user_agent(agent);
    }
    builder
        .build()
        .map_err(|e| CliError::Other(format!("building the HTTP client: {}", e.without_url())))
}

/// A URL's path without its query, for a reason: reasons never carry a query.
pub(crate) fn path_of(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let path = rest.find('/').map_or("/", |i| &rest[i..]);
    path.split(['?', '#']).next().unwrap_or("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use pk_cli_browser::JarCookie;
    use std::io::Write;
    use std::net::TcpListener;

    /// The capture `pk-cli-browser` checks before Chrome starts, checked
    /// offline, with and without the confirm hook.
    #[test]
    fn the_browser_capture_is_well_formed() {
        for verify in [true, false] {
            let capture = capture(verify).unwrap();
            capture.validate().unwrap();
            assert_eq!(capture.label(), LABEL);
        }
        assert!(START_URL.starts_with(&format!("https://{HOST}/")));
    }

    /// The session check stays on the member site's own hosts, over https.
    #[test]
    fn the_session_check_is_an_https_read_on_uhc_com() {
        let url = SESSION_CHECK_URL;
        let host = url
            .strip_prefix("https://")
            .unwrap()
            .split(['/', '?', ':'])
            .next()
            .unwrap();
        assert!(host == DOMAIN || host.ends_with(".uhc.com"), "{url}");
    }

    fn candidate(jar: bool) -> Captured {
        let built = Captured::for_test(LABEL, HOST).user_agent("Mozilla/5.0 (test)");
        if jar {
            built.jar([
                JarCookie::for_test("AWSALBTG", "dummy-visitor", ".uhc.com", false, "/"),
                JarCookie::for_test("dummy_session", "dummy-value", "member.uhc.com", true, "/"),
            ])
        } else {
            built
        }
        .build()
    }

    /// A candidate without a jar is not read at all.
    #[test]
    fn no_jar_is_not_signed_in_yet() {
        match confirm_candidate(&candidate(false)) {
            Confirm::NotYet(reason) => assert!(reason.contains("no uhc.com cookie"), "{reason}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_check_query_reads_and_names_no_one() {
        let body: serde_json::Value = serde_json::from_str(SESSION_CHECK_BODY).unwrap();
        let query = body["query"].as_str().unwrap();
        assert!(query.starts_with("query "), "a query, never a mutation: {query}");
        assert_eq!(body["variables"], serde_json::json!({}));
    }

    /// What the site answers (docs/api.md), judged.
    #[test]
    fn judge_reads_the_sites_answers() {
        let json = Some("application/json");
        let signed_in = br#"{"data":{"spendingOverviews":{"__typename":"SpendingOverviewsAggregateResponse"}}}"#;
        assert!(matches!(judge(200, json, signed_in), Confirm::Ready));
        let visitor = br#"{"meta":{"success":false,"errors":[{"error":"Credentials missing."}]}}"#;
        assert!(matches!(judge(401, json, visitor), Confirm::NotYet(r) if r.contains("HTTP 401")));
        for (status, mime, body) in [
            (200, json, &br#"{"errors":[{"message":"x"}],"data":null}"#[..]),
            (200, json, &b"{"[..]),
            (200, Some("text/html"), &b"<p>edge</p>"[..]),
            (304, None, &b""[..]),
        ] {
            assert!(
                matches!(judge(status, mime, body), Confirm::Undecided { .. }),
                "{status} {:?}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn a_reason_names_the_path_without_its_query() {
        assert_eq!(path_of("https://member.uhc.com/api/x?member=1#y"), "/api/x");
        assert_eq!(path_of("https://member.uhc.com"), "/");
        assert_eq!(path_of("http://127.0.0.1:9/a/b"), "/a/b");
    }

    /// Serves one canned answer on loopback and hands back the request's head
    /// and body, read whole so the connection closes cleanly.
    fn serve_once(answer: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/api/check?q=1", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") && stream.read(&mut byte).unwrap() == 1 {
                head.push(byte[0]);
            }
            let head = String::from_utf8_lossy(&head).to_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .and_then(|n| n.trim().parse::<usize>().ok())
                .unwrap_or(0);
            let mut body = vec![0u8; length];
            stream.read_exact(&mut body).unwrap();
            stream.write_all(answer.as_bytes()).unwrap();
            format!("{head}{}", String::from_utf8_lossy(&body))
        });
        (url, handle)
    }

    /// The read's own decisions, before [`judge`]: what cannot decide is
    /// `Undecided` (with the provider's `Retry-After`), a redirect is
    /// `NotYet`, and the rest reaches `judge`. The request carries the jar and
    /// the browser's user agent, and follows no redirect.
    #[test]
    fn the_confirm_read_decides_what_says_nothing_about_the_session() {
        let undecided = |answer: &'static str, retry: Option<u64>| {
            let (url, server) = serve_once(answer);
            match confirm_at(&url, "a=b; c=d", Some("Mozilla/5.0 (test)")) {
                Confirm::Undecided { reason, retry_after } => {
                    assert!(reason.starts_with("POST /api/check answered HTTP"), "{reason}");
                    assert!(!reason.contains("q=1"), "{reason}");
                    assert_eq!(retry_after, retry.map(Duration::from_secs));
                }
                other => panic!("{answer:?}: {other:?}"),
            }
            server.join().unwrap()
        };
        let head = undecided("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n", None);
        assert!(head.starts_with("post /api/check?q=1 "), "{head}");
        assert!(head.contains("\r\ncookie: a=b; c=d\r\n"), "{head}");
        assert!(head.contains("\r\ncontent-type: application/json\r\n"), "{head}");
        assert!(head.ends_with(SESSION_CHECK_BODY), "{head}");
        assert!(head.contains("\r\nuser-agent: mozilla/5.0 (test)\r\n"), "{head}");
        undecided(
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 30\r\nContent-Length: 0\r\n\r\n",
            Some(30),
        );
        undecided(
            "HTTP/1.1 403 Forbidden\r\nContent-Type: text/html\r\nContent-Length: 9\r\n\r\n<p>no</p>",
            None,
        );

        let (url, server) =
            serve_once("HTTP/1.1 302 Found\r\nLocation: https://example.com/sign-in\r\nContent-Length: 0\r\n\r\n");
        match confirm_at(&url, "a=b", None) {
            Confirm::NotYet(reason) => assert!(reason.contains("redirect (HTTP 302)"), "{reason}"),
            other => panic!("{other:?}"),
        }
        let head = server.join().unwrap();
        assert!(head.contains(&format!("\r\nuser-agent: {BIN}/")), "{head}");

        let (url, server) =
            serve_once("HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}");
        assert!(matches!(confirm_at(&url, "a=b", None), Confirm::NotYet(r) if r.contains("HTTP 401")));
        server.join().unwrap();
        let (url, server) = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 40\r\n\r\n{\"data\":{\"spendingOverviews\":{\"a\":\"b\"}}}",
        );
        assert!(matches!(confirm_at(&url, "a=b", None), Confirm::Ready));
        server.join().unwrap();
    }

    #[test]
    fn no_answer_is_undecided() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        match confirm_at(&format!("http://127.0.0.1:{port}/api/check"), "a=b", None) {
            Confirm::Undecided { reason, .. } => assert_eq!(reason, "POST /api/check got no answer"),
            other => panic!("{other:?}"),
        }
    }
}
