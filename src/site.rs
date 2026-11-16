//! The member site, member.uhc.com: what `auth login --browser` reads out of
//! the Chrome window, and the one read that confirms a session before it is
//! stored.
//!
//! The site sets visitor cookies (`AWSALBTG`, `AWSALBTGCORS`,
//! `X-DashCafe-Canary`) before anyone signs in, so a cookie in the jar proves
//! nothing. The capture waits for a page that offers a way to sign out
//! ([`SIGNED_IN`]), then [`confirm_session`] makes one read with the jar.
//!
//! Which read, and what in its answer shows a signed-in member, are still
//! being mapped (docs/api.md). [`SESSION_CHECK_URL`] and [`judge`] are the two
//! places to fill in. Until then every confirmation answers `NotYet`, and
//! `auth login` refuses a checked sign-in before Chrome opens; `--no-verify`
//! stores a session unchecked.

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

/// Signed in: the page offers a way to sign out. A signed-out page offers
/// only a sign-in.
pub const SIGNED_IN: &str = r#"(() => {
  const out = [...document.querySelectorAll('a, button')].some(e =>
    /^\s*(sign|log)\s?out\s*$/i.test(e.textContent || '') ||
    /(logout|signout|sign-out|log-out)/i.test(e.getAttribute('href') || ''));
  return out ? 'signed-in' : null;
})()"#;

/// The member-site read that shows whether a cookie jar is a signed-in
/// member's: an `https` URL on a uhc.com host, read with `GET`. `None` until
/// the API is mapped (docs/api.md). Fill it in together with [`judge`].
pub const SESSION_CHECK_URL: Option<&str> = None;

/// The reason every confirmation gives until [`SESSION_CHECK_URL`] and
/// [`judge`] are filled in.
pub const NOT_MAPPED: &str = "the member site's session check is not mapped yet";

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
        .expression(SIGNED_IN)
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
/// session captured from one; without it the request names `uhc`.
pub fn confirm_session(cookie_header: &str, user_agent: Option<&str>) -> Confirm {
    match SESSION_CHECK_URL {
        Some(url) => confirm_at(url, cookie_header, user_agent),
        None => Confirm::NotYet(NOT_MAPPED.into()),
    }
}

/// How the answer from [`SESSION_CHECK_URL`] shows a signed-in member.
///
/// `Ready` for what only a signed-in answer has (the member's own record,
/// not just a 200). `NotYet` for the answer a visitor gets: a 401, or a
/// parseable answer with no member in it. `Confirm::undecided` for a 2xx
/// that is empty, unparseable or an edge page. It sees only answers that
/// [`confirm_session`] has not already decided: no 429, 5xx, 403 with an
/// HTML page or redirect. `body` is at most 1 MiB.
///
/// Not mapped yet: every answer is `NotYet`.
pub fn judge(status: u16, content_type: Option<&str>, body: &[u8]) -> Confirm {
    let _ = (status, content_type, body);
    Confirm::NotYet(NOT_MAPPED.into())
}

/// [`confirm_session`]'s read of `url`, apart from the constant so the tests
/// can point it at a local server.
fn confirm_at(url: &str, cookie_header: &str, user_agent: Option<&str>) -> Confirm {
    let what = format!("GET {}", path_of(url));
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
        .get(url)
        .header(COOKIE, cookie)
        .header(ACCEPT, "application/json")
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
fn path_of(url: &str) -> &str {
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

    /// Whoever fills in the session check keeps it on the member site's own
    /// hosts, over https.
    #[test]
    fn the_session_check_is_an_https_read_on_uhc_com() {
        if let Some(url) = SESSION_CHECK_URL {
            let host = url
                .strip_prefix("https://")
                .unwrap()
                .split(['/', '?', ':'])
                .next()
                .unwrap();
            assert!(host == DOMAIN || host.ends_with(".uhc.com"), "{url}");
        }
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

    /// Until the check is mapped, no candidate is confirmed and no request is
    /// made: the hook answers before any client is built.
    #[test]
    fn the_confirm_hook_waits_until_the_check_is_mapped() {
        if SESSION_CHECK_URL.is_some() {
            return;
        }
        for jar in [true, false] {
            match confirm_candidate(&candidate(jar)) {
                Confirm::NotYet(reason) if jar => assert_eq!(reason, NOT_MAPPED),
                Confirm::NotYet(reason) => assert!(reason.contains("no uhc.com cookie"), "{reason}"),
                other => panic!("jar {jar}: {other:?}"),
            }
        }
        assert!(matches!(confirm_session("a=b", None), Confirm::NotYet(r) if r == NOT_MAPPED));
    }

    #[test]
    fn judge_is_not_mapped_yet() {
        for (status, mime, body) in [
            (200, Some("application/json"), &b"{}"[..]),
            (401, Some("application/json"), &b"{}"[..]),
            (200, None, &b""[..]),
        ] {
            assert!(matches!(judge(status, mime, body), Confirm::NotYet(r) if r == NOT_MAPPED));
        }
    }

    #[test]
    fn a_reason_names_the_path_without_its_query() {
        assert_eq!(path_of("https://member.uhc.com/api/x?member=1#y"), "/api/x");
        assert_eq!(path_of("https://member.uhc.com"), "/");
        assert_eq!(path_of("http://127.0.0.1:9/a/b"), "/a/b");
    }

    /// Serves one canned answer on loopback and hands back the request's
    /// head.
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
            stream.write_all(answer.as_bytes()).unwrap();
            String::from_utf8_lossy(&head).to_lowercase()
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
                    assert!(reason.starts_with("GET /api/check answered HTTP"), "{reason}");
                    assert!(!reason.contains("q=1"), "{reason}");
                    assert_eq!(retry_after, retry.map(Duration::from_secs));
                }
                other => panic!("{answer:?}: {other:?}"),
            }
            server.join().unwrap()
        };
        let head = undecided("HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\n\r\n", None);
        assert!(head.contains("\r\ncookie: a=b; c=d\r\n"), "{head}");
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

        for answer in [
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            "HTTP/1.1 401 Unauthorized\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
        ] {
            let (url, server) = serve_once(answer);
            assert!(matches!(confirm_at(&url, "a=b", None), Confirm::NotYet(r) if r == NOT_MAPPED));
            server.join().unwrap();
        }
    }

    #[test]
    fn no_answer_is_undecided() {
        let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        match confirm_at(&format!("http://127.0.0.1:{port}/api/check"), "a=b", None) {
            Confirm::Undecided { reason, .. } => assert_eq!(reason, "GET /api/check got no answer"),
            other => panic!("{other:?}"),
        }
    }
}
