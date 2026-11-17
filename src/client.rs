//! The member-site reads: GraphQL queries and the document PDF, made with a
//! profile's stored session (docs/api.md § Member site).
//!
//! - A request carries the session's `Cookie` header, the browser's user
//!   agent captured with it, and `Accept: application/json` (`application/pdf`
//!   beside it for a document). The site's edge refuses a request without a
//!   browser's user agent.
//! - It follows no redirect (the site sends a visitor to its sign-in) and is
//!   bounded in time: 30 s for the whole answer, 10 s to connect.
//! - One request at a time, at least [`PACE`] after the last answer.
//! - Only `query` operations and `GET`s: nothing writes to UHC.
//! - [`judge`] turns an answer into data or an error: a 401 or a redirect is
//!   a refused session (exit 3), a 403 page from the edge, a 429, a 5xx and a
//!   GraphQL answer of errors without data are exit 5.
//! - A `Set-Cookie` on an answer updates the client's jar; the caller writes
//!   the updated session back once the command has read what it needed
//!   ([`Client::updated_cookie_header`]).

use std::cell::{Cell, RefCell};
use std::io::Read as _;
use std::time::{Duration, Instant};

use pk_cli_config::profile::ProfileName;
use pk_cli_core::{CliError, Reason};
use pk_cli_secrets::Secret;
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, CONTENT_TYPE, COOKIE, RETRY_AFTER, SET_COOKIE, USER_AGENT};
use serde_json::{json, Value};

use crate::cli::BIN;
use crate::session::Session;

/// The member site's origin.
pub const BASE: &str = "https://member.uhc.com";
/// The least time between one answer and the next request.
pub const PACE: Duration = Duration::from_secs(1);
const TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of a GraphQL answer read (the claims of ten years are about
/// half a megabyte).
const MAX_JSON: u64 = 16 * 1024 * 1024;
/// The most of a document read.
const MAX_PDF: u64 = 64 * 1024 * 1024;

/// One GraphQL read: where it goes and the query it sends.
pub struct Query {
    pub path: &'static str,
    pub operation: &'static str,
    pub text: &'static str,
    /// The documents service checks a CSRF token taken from the `FH_MU`
    /// cookie ([`xsrf_token`]).
    pub xsrf: bool,
}

/// Members and every claim in a date window, in one answer.
pub const CLAIMS: Query = Query {
    path: "/ce-claims-aggregate-api/graphql?q=claims&cache=off",
    operation: "claims",
    text: include_str!("site/queries/claims.graphql"),
    xsrf: false,
};

/// One claim's lines, codes and remarks, by the `claimKey` and `claimType`
/// a claims answer gave.
pub const CLAIM_DETAILS: Query = Query {
    path: "/ce-claims-aggregate-api/graphql?q=claimDetails",
    operation: "claimDetails",
    text: include_str!("site/queries/claim_details.graphql"),
    xsrf: false,
};

/// Members, the plan's maximums and each member's progress.
pub const SPENDING: Query = Query {
    path: "/ce-claims-aggregate-api/graphql?q=spending",
    operation: "spending",
    text: include_str!("site/queries/spending.graphql"),
    xsrf: false,
};

/// The documents (EOBs, health statements, letters) in a window of
/// processing dates.
pub const DOCUMENTS: Query = Query {
    path: "/rest/oneportal/web/documents-aggregate-api/v1/graphql",
    operation: "documents",
    text: include_str!("site/queries/documents.graphql"),
    xsrf: true,
};

/// Every read uhc makes, for the tests that hold them to queries.
pub const QUERIES: [&Query; 4] = [&CLAIMS, &CLAIM_DETAILS, &SPENDING, &DOCUMENTS];

/// A session's cookies, in the order the header gave them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Jar {
    cookies: Vec<(String, String)>,
    changed: bool,
}

impl Jar {
    pub fn parse(header: &str) -> Jar {
        let cookies = header
            .split(';')
            .filter_map(|pair| {
                let (name, value) = pair.trim().split_once('=')?;
                (!name.trim().is_empty()).then(|| (name.trim().to_string(), value.trim().to_string()))
            })
            .collect();
        Jar {
            cookies,
            changed: false,
        }
    }

    pub fn header(&self) -> String {
        self.cookies
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.cookies.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }

    /// One `Set-Cookie` value: a new value replaces the cookie's, a new
    /// cookie is added, and an empty value or `Max-Age` of zero or less
    /// removes it. The other attributes (domain, path, expiry) are the
    /// browser's business; the site's are all on its own hosts.
    pub fn apply(&mut self, set_cookie: &str) {
        let mut parts = set_cookie.split(';');
        let Some((name, value)) = parts.next().and_then(|p| p.split_once('=')) else {
            return;
        };
        let (name, value) = (name.trim(), value.trim());
        if name.is_empty() {
            return;
        }
        let expired = parts.any(|a| {
            a.trim().split_once('=').is_some_and(|(k, v)| {
                k.trim().eq_ignore_ascii_case("max-age") && v.trim().parse::<i64>().is_ok_and(|n| n <= 0)
            })
        });
        let at = self.cookies.iter().position(|(n, _)| n == name);
        match (at, value.is_empty() || expired) {
            (Some(i), true) => {
                self.cookies.remove(i);
                self.changed = true;
            }
            (Some(i), false) if self.cookies[i].1 != value => {
                self.cookies[i].1 = value.to_string();
                self.changed = true;
            }
            (None, false) => {
                self.cookies.push((name.to_string(), value.to_string()));
                self.changed = true;
            }
            _ => {}
        }
    }

    pub fn changed(&self) -> bool {
        self.changed
    }
}

/// The documents service's CSRF token: the first `!`-separated part of the
/// URL-decoded `FH_MU` cookie, which the site's own pages send as
/// `XSRF-TOKEN`.
pub fn xsrf_token(jar: &Jar) -> Option<String> {
    let raw = jar.get("FH_MU")?;
    let decoded = percent_decode(raw);
    let first = decoded.split('!').next()?.trim();
    (!first.is_empty() && HeaderValue::from_str(first).is_ok()).then(|| first.to_string())
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            let hex = |c: u8| (c as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// What a read sends and to whom: the session's cookies and user agent.
pub struct Client {
    http: reqwest::blocking::Client,
    base: String,
    jar: RefCell<Jar>,
    pace: Duration,
    last: Cell<Option<Instant>>,
    profile: ProfileName,
}

impl Client {
    /// A client for `profile`'s `session` against `base` ([`BASE`], or a
    /// test's loopback server), `pace` apart.
    pub fn new(session: &Session, profile: &ProfileName, base: &str, pace: Duration) -> Result<Client, CliError> {
        let agent = session
            .user_agent
            .clone()
            .unwrap_or_else(|| format!("{BIN}/{}", env!("CARGO_PKG_VERSION")));
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_str(&agent)
                .map_err(|_| CliError::Other("the stored user agent is not a valid HTTP header value".into()))?,
        );
        // A plain client: no cookie store (the jar is ours), no redirects.
        let http = reqwest::blocking::Client::builder()
            .default_headers(headers)
            .redirect(reqwest::redirect::Policy::none())
            .timeout(TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| CliError::Other(format!("building the HTTP client: {}", e.without_url())))?;
        Ok(Client {
            http,
            base: base.trim_end_matches('/').to_string(),
            jar: RefCell::new(Jar::parse(session.cookie_header.expose())),
            pace,
            last: Cell::new(None),
            profile: profile.clone(),
        })
    }

    /// `query` with `variables`: its `data`.
    pub fn query(&self, query: &Query, variables: Value) -> Result<Value, CliError> {
        debug_assert!(query.text.trim_start().starts_with("query "));
        let body = json!({ "operationName": query.operation, "variables": variables, "query": query.text });
        let mut extra = HeaderMap::new();
        extra.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        extra.insert(ACCEPT, HeaderValue::from_static("application/json"));
        if query.xsrf {
            let token = xsrf_token(&self.jar.borrow()).ok_or_else(|| {
                CliError::Auth(format!(
                    "the session stored for profile `{}` lacks the FH_MU cookie the documents read needs",
                    self.profile
                ))
                .because(Reason::CredentialRefused)
                .with_remedy(login_remedy(&self.profile))
            })?;
            extra.insert(
                "XSRF-TOKEN",
                HeaderValue::from_str(&token).map_err(|_| CliError::Other("the FH_MU cookie is malformed".into()))?,
            );
            extra.insert("RP-Token-Suffix", HeaderValue::from_static("MU"));
        }
        let answer = self.send(
            "POST",
            query.path,
            self.http
                .post(self.url(query.path))
                .headers(extra)
                .body(body.to_string()),
            MAX_JSON,
        )?;
        judge(&answer, &self.profile)
    }

    /// The PDF of a site document.
    pub fn pdf(&self, document_id: &str, class: &str) -> Result<Vec<u8>, CliError> {
        let ok_id = !document_id.is_empty() && document_id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-');
        let ok_class = !class.is_empty() && class.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !ok_id || !ok_class {
            return Err(CliError::Upstream(
                "the member site listed a document whose id or class is not one uhc can ask for".into(),
            ));
        }
        let path = format!(
            "/oneportal/view/documents-aggregate-api/v1/v2/doc360/content/id/{document_id}?documentClass={class}"
        );
        let request = self
            .http
            .get(self.url(&path))
            .header(ACCEPT, "application/pdf,application/json");
        let answer = self.send("GET", &path, request, MAX_PDF)?;
        refuse(&answer, &self.profile)?;
        if !answer.content_type.as_deref().is_some_and(|t| t.contains("pdf")) {
            return Err(CliError::Upstream(format!(
                "{} answered HTTP {} without a PDF",
                answer.what, answer.status
            )));
        }
        Ok(answer.body)
    }

    /// The session's cookie header as the answers left it, when one changed it.
    pub fn updated_cookie_header(&self) -> Option<Secret> {
        let jar = self.jar.borrow();
        jar.changed().then(|| Secret::new(jar.header()))
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        request: reqwest::blocking::RequestBuilder,
        limit: u64,
    ) -> Result<Answer, CliError> {
        let what = format!("{method} {}", crate::site::path_of(path));
        if let Some(last) = self.last.get() {
            let since = last.elapsed();
            if since < self.pace {
                std::thread::sleep(self.pace - since);
            }
        }
        let mut cookie = HeaderValue::from_str(&self.jar.borrow().header())
            .map_err(|_| CliError::Other("the session's Cookie header is not a valid HTTP header value".into()))?;
        cookie.set_sensitive(true);
        let result = request.header(COOKIE, cookie).send();
        let answer = match result {
            Ok(a) => a,
            Err(e) => {
                self.last.set(Some(Instant::now()));
                return Err(if e.is_timeout() {
                    CliError::Upstream(format!("{what} timed out after {} s", TIMEOUT.as_secs()))
                } else {
                    CliError::Upstream(format!("{what} got no answer from the member site"))
                });
            }
        };
        let status = answer.status().as_u16();
        let header = |name| {
            answer
                .headers()
                .get(name)
                .and_then(|v: &HeaderValue| v.to_str().ok())
                .map(str::to_string)
        };
        let content_type = header(CONTENT_TYPE);
        let retry_after = header(RETRY_AFTER);
        {
            let mut jar = self.jar.borrow_mut();
            for v in answer.headers().get_all(SET_COOKIE) {
                if let Ok(v) = v.to_str() {
                    jar.apply(v);
                }
            }
        }
        let mut body = Vec::new();
        let read = answer.take(limit + 1).read_to_end(&mut body);
        self.last.set(Some(Instant::now()));
        if read.is_err() {
            return Err(CliError::Upstream(format!(
                "{what} answered HTTP {status} and its body did not arrive"
            )));
        }
        if body.len() as u64 > limit {
            return Err(CliError::Upstream(format!(
                "{what} answered with more than {} MiB",
                limit >> 20
            )));
        }
        Ok(Answer {
            what,
            status,
            content_type,
            retry_after,
            body,
        })
    }
}

/// An answer, read whole.
pub struct Answer {
    /// `POST /path`, no query.
    pub what: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub retry_after: Option<String>,
    pub body: Vec<u8>,
}

fn login_remedy(profile: &ProfileName) -> String {
    format!("{BIN} auth login --browser --profile {profile} --overwrite")
}

/// The answers that are no data whatever the read: a refused session, the
/// edge's page, too many requests, the site's own failures.
fn refuse(a: &Answer, profile: &ProfileName) -> Result<(), CliError> {
    let what = &a.what;
    let status = a.status;
    let html = a.content_type.as_deref().is_some_and(|t| t.contains("html"));
    match status {
        200..=299 => Ok(()),
        401 => Err(CliError::Auth(format!(
            "the member site refused profile `{profile}`'s session ({what}: HTTP 401): it has signed out or expired"
        ))
        .because(Reason::CredentialRefused)
        .with_remedy(login_remedy(profile))),
        300..=399 => Err(CliError::Auth(format!(
            "{what} answered with a redirect (HTTP {status}), as the site answers a visitor: profile `{profile}`'s \
             session has signed out"
        ))
        .because(Reason::CredentialRefused)
        .with_remedy(login_remedy(profile))),
        403 if html => Err(CliError::Upstream(format!(
            "the member site's edge refused {what} (HTTP 403 with its error page); it refuses a request without a \
             browser's user agent"
        ))
        .because(Reason::ClientBlocked)
        .with_remedy(format!(
            "sign in again with `{}`, which keeps the browser's user agent, or pass --user-agent with --stdin",
            login_remedy(profile)
        ))),
        403 => Err(CliError::Upstream(format!("the member site refused {what} (HTTP 403)")).because(Reason::Forbidden)),
        429 => {
            let mut e = CliError::Upstream(format!(
                "the member site answered {what} with HTTP 429: too many requests"
            ));
            e = match &a.retry_after {
                Some(after) => e.with_remedy(format!("try again after {} s", after.trim())),
                None => e.with_remedy("try again in a few minutes"),
            };
            Err(e)
        }
        500..=599 => Err(
            CliError::Upstream(format!("the member site answered {what} with HTTP {status}"))
                .with_remedy("try again later"),
        ),
        400 => Err(CliError::Upstream(format!(
            "the member site refused {what} as a bad request (HTTP 400); the site may have changed"
        ))
        .because(Reason::BadRequest)),
        _ => Err(CliError::Upstream(format!(
            "the member site answered {what} with HTTP {status}"
        ))),
    }
}

/// A GraphQL answer's `data`, or why there is none. An answer with `errors`
/// and data keeps the data (the site reports a part it could not fill, such
/// as one document type); errors without data are exit 5 quoting the first
/// message, which names the query's fields, never a member's.
pub fn judge(a: &Answer, profile: &ProfileName) -> Result<Value, CliError> {
    refuse(a, profile)?;
    let what = &a.what;
    if !a.content_type.as_deref().is_some_and(|t| t.contains("json")) {
        return Err(CliError::Upstream(format!(
            "{what} answered HTTP {} without JSON",
            a.status
        )));
    }
    let doc: Value = serde_json::from_slice(&a.body)
        .map_err(|_| CliError::Upstream(format!("{what} answered JSON that does not parse")))?;
    match doc.get("data") {
        Some(data) if !data.is_null() => Ok(data.clone()),
        _ => {
            let first = doc
                .pointer("/errors/0/message")
                .and_then(Value::as_str)
                .map(|m| m.chars().take(200).collect::<String>());
            Err(CliError::Upstream(match first {
                Some(m) => format!("{what} answered GraphQL errors and no data: {m}"),
                None => format!("{what} answered no data"),
            }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adult1() -> ProfileName {
        ProfileName::parse("adult1").unwrap()
    }

    fn answer(status: u16, content_type: &str, body: &str) -> Answer {
        Answer {
            what: "POST /ce-claims-aggregate-api/graphql".into(),
            status,
            content_type: Some(content_type.into()),
            retry_after: None,
            body: body.as_bytes().to_vec(),
        }
    }

    const JSON: &str = "application/json";

    #[test]
    fn every_read_is_a_query() {
        for q in QUERIES {
            let text = q.text.trim_start();
            assert!(text.starts_with(&format!("query {}", q.operation)), "{}", q.operation);
            assert!(!text.contains("mutation"), "{}", q.operation);
            assert!(q.path.starts_with('/'));
        }
    }

    #[test]
    fn the_queries_ask_for_no_member_or_plan_id() {
        for q in QUERIES {
            let head = q.text.lines().next().unwrap();
            for word in ["memberId", "planId", "groupNumber", "subscriber"] {
                assert!(!head.contains(word), "{}: {head}", q.operation);
            }
        }
    }

    /// The site's answers, as the fixtures hold them, judged.
    #[test]
    fn judge_reads_the_sites_answers() {
        let data = judge(&answer(200, JSON, r#"{"data":{"x":1}}"#), &adult1()).unwrap();
        assert_eq!(data, json!({"x": 1}));
        // Errors beside data keep the data.
        let data = judge(
            &answer(200, JSON, r#"{"data":{"x":1},"errors":[{"message":"part"}]}"#),
            &adult1(),
        )
        .unwrap();
        assert_eq!(data["x"], 1);

        let e = judge(
            &answer(401, JSON, include_str!("../tests/fixtures/site/unauthorized.json")),
            &adult1(),
        )
        .unwrap_err();
        assert_eq!(e.exit_code(), 3);
        assert_eq!(e.reason(), Some(Reason::CredentialRefused));
        assert_eq!(
            e.remedy(),
            Some("uhc auth login --browser --profile adult1 --overwrite")
        );

        let e = judge(&answer(302, "text/html", ""), &adult1()).unwrap_err();
        assert_eq!(e.exit_code(), 3);

        let e = judge(
            &answer(403, "text/html", include_str!("../tests/fixtures/site/edge-403.html")),
            &adult1(),
        )
        .unwrap_err();
        assert_eq!(e.exit_code(), 5);
        assert_eq!(e.reason(), Some(Reason::ClientBlocked));
        assert!(!e.message().contains("EXAMPLE-REQUEST-ID"), "{e}");

        let e = judge(
            &answer(200, JSON, include_str!("../tests/fixtures/site/graphql-errors.json")),
            &adult1(),
        )
        .unwrap_err();
        assert_eq!(e.exit_code(), 5);
        assert!(e.message().contains("FieldUndefined"), "{e}");

        let mut busy = answer(429, JSON, "{}");
        busy.retry_after = Some("30".into());
        let e = judge(&busy, &adult1()).unwrap_err();
        assert_eq!(e.exit_code(), 5);
        assert_eq!(e.remedy(), Some("try again after 30 s"));
        for (status, mime, body) in [
            (500, JSON, "{}"),
            (503, "text/html", "<p>down</p>"),
            (403, JSON, r#"{"status":403,"error":"Forbidden"}"#),
            (400, JSON, "{}"),
            (200, "text/html", "<p>x</p>"),
            (200, JSON, "{"),
            (200, JSON, r#"{"data":null}"#),
        ] {
            let e = judge(&answer(status, mime, body), &adult1()).unwrap_err();
            assert_eq!(e.exit_code(), 5, "{status} {body}");
        }
    }

    #[test]
    fn set_cookie_updates_the_jar() {
        let mut jar = Jar::parse("a=1; b=2;  c=3");
        assert_eq!(jar.header(), "a=1; b=2; c=3");
        jar.apply("b=2; Path=/; Secure");
        assert!(!jar.changed(), "the same value changes nothing");
        jar.apply("b=20; Path=/; HttpOnly");
        jar.apply("d=4; Domain=.uhc.com");
        jar.apply("a=; Max-Age=0");
        jar.apply("c=3; Max-Age=0");
        jar.apply("=junk");
        jar.apply("no pair");
        assert!(jar.changed());
        assert_eq!(jar.header(), "b=20; d=4");
    }

    #[test]
    fn the_documents_token_is_the_first_part_of_fh_mu() {
        let jar = Jar::parse("x=1; FH_MU=dummyToken%21part2%21part3");
        assert_eq!(xsrf_token(&jar).as_deref(), Some("dummyToken"));
        assert_eq!(xsrf_token(&Jar::parse("x=1")), None);
        assert_eq!(xsrf_token(&Jar::parse("FH_MU=%21x")), None);
        assert_eq!(percent_decode("a%2"), "a%2");
    }
}
