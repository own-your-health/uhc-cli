//! `auth login`, `auth status` and `auth logout`: a member-site session per
//! profile (DESIGN.md §1.7, §2.2), kept through [`SessionStore`], which alone
//! knows where.
//!
//! A `--browser` sign-in runs Chrome in the profile's own kept browser
//! profile (`Launch::profile_name`), so the site remembers each account's
//! device apart and one account's window never opens signed in as another;
//! `auth logout --profile <NAME>` removes the session, then that browser
//! profile. While `UHC_KEYCHAIN_SERVICE` points the store elsewhere
//! ([`session::hermetic`]), nothing touches the real browser profiles:
//! `auth status` does not report them, `auth logout` leaves them in place,
//! and `auth login --browser` signs in through a fresh one.

use pk_cli_auth::{AuthLogin, AuthMethod, AuthStatus};
use pk_cli_browser::{BrowserError, Confirm, Launch};
use pk_cli_core::{output, CliError, Reason};
use pk_cli_secrets::Secret;
use reqwest::header::HeaderValue;
use serde_json::{json, Value};

use crate::cli::{LoginCmd, BIN};
use crate::session::{self, Session, SessionStore, KEYCHAIN_SERVICE_ENV};
use crate::site;
use crate::source::Profile;

// ----------------------------------------------------------------- login

pub fn login(cmd: &LoginCmd, profile: &Profile, json: bool, quiet: bool) -> Result<(), CliError> {
    let LoginCmd {
        login,
        browser,
        user_agent,
    } = cmd;
    let manual = login.source.stdin || login.source.from_env.is_some();
    // Flags first: a refused combination reaches neither the keychain nor
    // the site.
    if browser.browser && manual {
        return Err(CliError::Usage(
            "--browser takes the session from the browser; drop --stdin and --from-env".into(),
        ));
    }
    if login.source.stdin && login.source.from_env.is_some() {
        return Err(CliError::Usage("pass one of --stdin and --from-env".into()));
    }
    if user_agent.is_some() && !manual {
        return Err(CliError::Usage(
            "--user-agent goes with --stdin or --from-env; --browser reads the browser's own".into(),
        ));
    }
    if !browser.browser && !manual {
        return Err(CliError::Usage(
            "pass --browser to sign in through a Chrome window, or --stdin with the Cookie header of a \
             member.uhc.com request copied from the browser's DevTools"
                .into(),
        ));
    }
    let verify = !login.no_verify;
    can_confirm(verify, site::SESSION_CHECK_URL)?;
    let store = SessionStore::open();
    // Before Chrome opens or stdin is read, so no sign-in is spent on a
    // session that would not be stored.
    if !login.overwrite {
        let stored = match store.get(profile) {
            Ok(found) => found.is_some(),
            Err(e) if e.reason() == Some(Reason::CredentialUnreadable) => true,
            Err(e) => return Err(e),
        };
        if stored {
            return Err(CliError::Usage(format!(
                "a member-site session is already stored for profile `{profile}`; pass --overwrite to replace it \
                 (`{BIN} auth logout --profile {profile}` removes it)"
            )));
        }
    }
    let session = if browser.browser {
        browser_session(profile, browser.fresh_profile, verify, quiet)?
    } else {
        manual_session(
            &login.source.read(None)?,
            user_agent.as_deref(),
            verify,
            profile,
            site::confirm_session,
        )?
    };
    store.put(profile, &session)?;
    if verify {
        if !quiet {
            eprintln!("member-site session stored for profile `{profile}`");
        }
    } else {
        // Even under -q: the session is stored with nothing to show it works.
        eprintln!(
            "warning: the session was stored unverified (--no-verify): no member-site read checked it. \
             `{BIN} auth login --browser --profile {profile} --overwrite` replaces it"
        );
    }
    if json {
        output::json(&AuthLogin::stored(verify).to_json());
    }
    Ok(())
}

/// Refuses a checked sign-in while the session check is not mapped
/// (`check` is [`site::SESSION_CHECK_URL`]): every confirmation would be
/// `NotYet`, so a `--browser` sign-in would be spent and then wait out its
/// timeout. Off once the check is filled in.
fn can_confirm(verify: bool, check: Option<&str>) -> Result<(), CliError> {
    if verify && check.is_none() {
        return Err(
            CliError::Other(format!("{}, so a session could not be confirmed", site::NOT_MAPPED))
                .with_remedy("--no-verify stores it unchecked (`verified: false`)"),
        );
    }
    Ok(())
}

/// The session from a Chrome window: the owner signs in on the site's own
/// page, and the capture hands over the `uhc.com` jar once a signed-in page
/// is open and, with `verify`, [`site::confirm_candidate`] accepts it.
fn browser_session(profile: &Profile, fresh: bool, verify: bool, quiet: bool) -> Result<Session, CliError> {
    let scratch = fresh || session::hermetic();
    if scratch && !fresh && !quiet {
        eprintln!(
            "{KEYCHAIN_SERVICE_ENV} is set, so this sign-in uses a fresh browser profile and leaves the kept one alone"
        );
    }
    let launch = Launch::new(BIN)
        .url(site::START_URL)
        .profile_name(profile.name())
        .fresh_profile(scratch)
        .login_hint(format!("{BIN} auth login --browser --profile {profile}"))
        .quiet(quiet);
    let captured = pk_cli_browser::capture(launch, &site::capture(verify)?, pk_cli_browser::DEFAULT_TIMEOUT).map_err(
        |e| match e {
            e @ BrowserError::ChromeNotFound(_) => CliError::from(e).with_remedy(format!(
                "without Chrome, copy the Cookie header of a member.uhc.com request from a signed-in browser's \
                 DevTools and pipe it to `{BIN} auth login --stdin`"
            )),
            e => e.into(),
        },
    )?;
    let cookie_header = captured
        .cookie_header()
        .ok_or_else(|| CliError::Other("the sign-in handed over no uhc.com cookie jar".into()))?;
    Ok(Session::new(cookie_header, captured.user_agent().map(str::to_string)))
}

/// A `--stdin` (or `--from-env`) import, checked with `confirm` unless
/// `verify` is off. Unlike a browser capture, any answer short of `Ready`
/// stores nothing, because the value can be piped again.
fn manual_session(
    raw: &Secret,
    user_agent: Option<&str>,
    verify: bool,
    profile: &Profile,
    confirm: impl FnOnce(&str, Option<&str>) -> Confirm,
) -> Result<Session, CliError> {
    let cookie_header = cookie_header_from(raw)?;
    if verify {
        match confirm(cookie_header.expose(), user_agent) {
            Confirm::Ready => {}
            Confirm::NotYet(reason) => {
                return Err(CliError::Auth(format!(
                    "the member site did not accept the session ({reason}); nothing was stored"
                ))
                .with_remedy(format!(
                    "sign in with `{BIN} auth login --browser --profile {profile}`; --no-verify stores the header \
                     unchecked"
                )))
            }
            Confirm::Undecided { reason, .. } => {
                return Err(CliError::Upstream(format!(
                    "the member site's answer did not show whether the session works ({reason}); nothing was stored"
                ))
                .with_remedy("pipe it again in a few minutes"))
            }
            Confirm::Fail(e) => return Err(e),
            _ => {
                return Err(CliError::Other(
                    "the session check answered in a way this uhc does not know; nothing was stored".into(),
                ))
            }
        }
    }
    Ok(Session::new(cookie_header, user_agent.map(str::to_string)))
}

/// The `Cookie` header value a `--stdin` import read: trimmed, a leading
/// `Cookie:` dropped, one line of `name=value` pairs. The errors never quote
/// it.
fn cookie_header_from(raw: &Secret) -> Result<Secret, CliError> {
    let text = raw.expose().trim();
    let text = match text.get(..7) {
        Some(name) if name.eq_ignore_ascii_case("cookie:") => text[7..].trim_start(),
        _ => text,
    };
    if text.is_empty() {
        return Err(CliError::Usage("the Cookie header read is empty".into()));
    }
    if !text.contains('=') || HeaderValue::from_str(text).is_err() {
        return Err(CliError::Usage(
            "what was read is not one Cookie header value (`name=value; name=value`, on one line); copy the \
             value of a member.uhc.com request's Cookie header from DevTools"
                .into(),
        ));
    }
    Ok(Secret::new(text))
}

// ---------------------------------------------------------------- status

/// `auth status`: local facts only, no request. `required` is false
/// because the files source needs no sign-in.
pub fn status(profile: &Profile, json: bool, quiet: bool) -> Result<(), CliError> {
    let store = SessionStore::open();
    let (stored, session) = match store.get(profile) {
        Ok(session) => (session.is_some(), session),
        Err(e) if e.reason() == Some(Reason::CredentialUnreadable) => {
            eprintln!("warning: {}", e.message());
            (true, None)
        }
        Err(e) => return Err(e),
    };
    let mut status = AuthStatus::new(false, session.is_some(), AuthMethod::BrowserSession);
    status.account = Some(profile.name().to_string());
    status.credential_in_keychain = Some(stored);
    let (kept, line) = browser_profile_status(session::hermetic(), || {
        pk_cli_browser::named_profile_exists(BIN, profile.name())
    });
    status.browser_profile = kept;
    if let (Some(line), false) = (line, quiet) {
        eprintln!("{line}");
    }
    let captured_at = session.as_ref().map(|s| s.captured_at.clone());
    if json {
        let mut v = status.to_json();
        if let Some(at) = &captured_at {
            v["captured_at"] = json!(at);
        }
        output::json(&v);
    } else {
        status.render();
        if let Some(at) = &captured_at {
            println!("Captured:      {at}");
        }
    }
    Ok(())
}

/// `auth status`'s browser field: whether a `--browser` sign-in keeps a
/// profile here, not whether it is signed in, and any line for stderr. A
/// profile that needs repair is a warning and leaves the field out, and so
/// does [`KEYCHAIN_SERVICE_ENV`], under which nothing reads the real profile.
/// The check is passed in: the binary binds it to `profile_exists(BIN)`, the
/// tests to `profile_exists_in` under a temp dir.
fn browser_profile_status(
    overridden: bool,
    exists: impl FnOnce() -> Result<bool, BrowserError>,
) -> (Option<bool>, Option<String>) {
    if overridden {
        return (
            None,
            Some(format!(
                "{KEYCHAIN_SERVICE_ENV} is set, so the kept browser profile is not reported"
            )),
        );
    }
    match exists() {
        Ok(kept) => (Some(kept), None),
        Err(e) => (None, Some(format!("warning: {e}"))),
    }
}

// ---------------------------------------------------------------- logout

pub fn logout(profile: &Profile, json: bool, quiet: bool) -> Result<(), CliError> {
    let store = SessionStore::open();
    let forget = (!session::hermetic()).then_some(|| pk_cli_browser::forget_named_profile(BIN, profile.name()));
    let done = run_logout(|| store.delete(profile), forget, profile)?;
    if json {
        output::json(&done.to_json());
    } else if !quiet {
        eprintln!("{}", done.line());
    }
    Ok(())
}

/// What `auth logout` removed (`auth-logout/v1`).
#[derive(Debug, PartialEq, Eq)]
struct LoggedOut {
    session_removed: bool,
    /// `None` when the profile was left alone ([`KEYCHAIN_SERVICE_ENV`]).
    browser_profile_removed: Option<bool>,
}

impl LoggedOut {
    fn to_json(&self) -> Value {
        let mut v = json!({ "schema": "auth-logout/v1", "session_removed": self.session_removed });
        if let Some(removed) = self.browser_profile_removed {
            v["browser_profile_removed"] = removed.into();
        }
        v
    }

    fn line(&self) -> String {
        format!(
            "logged out: {}; {}",
            session_said(self.session_removed),
            profile_said(self.browser_profile_removed)
        )
    }
}

fn session_said(removed: bool) -> &'static str {
    if removed {
        "the session was removed from the keychain"
    } else {
        "no session was stored"
    }
}

fn profile_said(removed: Option<bool>) -> String {
    match removed {
        Some(true) => "the browser profile was removed".into(),
        Some(false) => "no browser profile was kept".into(),
        None => format!("{KEYCHAIN_SERVICE_ENV} is set, so the kept browser profile was left alone"),
    }
}

/// `auth logout`'s two removals: the session first, then the browser
/// profile (`None` leaves it alone). Both are attempted whatever the other
/// did, so a refusal of one leaves the other done, and the error says what
/// was. They are passed in: the binary binds them to the session store and
/// to `forget_named_profile(BIN, profile)`, the tests to fakes and to
/// `forget_named_profile_in` under a temp dir, so a test never removes the
/// owner's session or profile.
fn run_logout(
    delete_session: impl FnOnce() -> Result<bool, CliError>,
    forget_profile: Option<impl FnOnce() -> Result<bool, BrowserError>>,
    who: &Profile,
) -> Result<LoggedOut, CliError> {
    let session = delete_session();
    let profile = forget_profile.map(|forget| forget().map_err(CliError::from));
    match (session, profile) {
        (Ok(session_removed), None) => Ok(LoggedOut {
            session_removed,
            browser_profile_removed: None,
        }),
        (Ok(session_removed), Some(Ok(removed))) => Ok(LoggedOut {
            session_removed,
            browser_profile_removed: Some(removed),
        }),
        (Ok(session_removed), Some(Err(e))) => Err(e.with_remedy(format!(
            "{}; the browser profile is all that is left",
            session_said(session_removed)
        ))),
        (Err(e), profile) => {
            let profile = match profile {
                None => profile_said(None),
                Some(Ok(removed)) => profile_said(Some(removed)),
                Some(Err(_)) => "the browser profile was not removed either".into(),
            };
            Err(e.with_remedy(format!("{profile}; run `{BIN} auth logout --profile {who}` again")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adult1() -> Profile {
        crate::source::resolve_profile("adult1").unwrap()
    }

    #[test]
    fn a_cookie_header_is_one_line_of_pairs() {
        let read = |s: &str| cookie_header_from(&Secret::new(s));
        assert_eq!(
            read("  dummy_a=1; dummy_b=2\n").unwrap().expose(),
            "dummy_a=1; dummy_b=2"
        );
        assert_eq!(read("Cookie: dummy_a=1").unwrap().expose(), "dummy_a=1");
        assert_eq!(read("cookie:dummy_a=1").unwrap().expose(), "dummy_a=1");
        for bad in [
            "",
            "  \n",
            "Cookie:",
            "not a header",
            "dummy_a=1\ndummy_b=2",
            "dummy_a=1\u{7}",
        ] {
            let e = read(bad).err().unwrap();
            assert_eq!(e.exit_code(), 2, "{bad:?}");
            assert!(!e.message().contains("dummy"), "{}", e.message());
        }
    }

    /// A `--stdin` import stores only what the read accepted, unless
    /// --no-verify skips the read.
    #[test]
    fn a_manual_import_stores_only_an_accepted_session() {
        let raw = Secret::new("dummy_a=1");
        let agent = Some("Mozilla/5.0 (test)");
        let ok = manual_session(&raw, agent, true, &adult1(), |h, a| {
            assert_eq!(h, "dummy_a=1");
            assert_eq!(a, Some("Mozilla/5.0 (test)"));
            Confirm::Ready
        })
        .unwrap();
        assert_eq!(ok.cookie_header.expose(), "dummy_a=1");
        assert_eq!(ok.user_agent.as_deref(), Some("Mozilla/5.0 (test)"));

        let not_signed_in = "POST /x answered HTTP 401: not signed in";
        let refused = manual_session(&raw, None, true, &adult1(), |_, _| {
            Confirm::NotYet(not_signed_in.into())
        })
        .err()
        .unwrap();
        assert_eq!(refused.exit_code(), 3);
        assert!(refused.message().contains(not_signed_in), "{}", refused.message());
        assert!(refused.to_string().contains("--no-verify"));

        let undecided = manual_session(&raw, None, true, &adult1(), |_, _| {
            Confirm::undecided("POST /x answered HTTP 503")
        })
        .err()
        .unwrap();
        assert_eq!(undecided.exit_code(), 5);

        let failed = manual_session(&raw, None, true, &adult1(), |_, _| {
            Confirm::Fail(CliError::Other("x".into()))
        })
        .err()
        .unwrap();
        assert_eq!(failed.exit_code(), 1);

        let unchecked =
            manual_session(&raw, None, false, &adult1(), |_, _| panic!("--no-verify makes no read")).unwrap();
        assert_eq!(unchecked.cookie_header.expose(), "dummy_a=1");
        assert!(unchecked.user_agent.is_none());

        // The header is checked before any read.
        let bad = manual_session(&Secret::new("nonsense"), None, true, &adult1(), |_, _| {
            panic!("no read for a bad header")
        });
        assert_eq!(bad.err().unwrap().exit_code(), 2);
    }

    /// Until the check is mapped, only --no-verify gets past it, before
    /// any keychain read or window.
    #[test]
    fn a_checked_sign_in_needs_a_mapped_check() {
        let e = can_confirm(true, None).err().unwrap();
        assert_eq!(e.exit_code(), 1);
        assert!(e.message().starts_with(site::NOT_MAPPED), "{e}");
        assert!(e.to_string().contains("--no-verify"), "{e}");
        can_confirm(false, None).unwrap();
        can_confirm(true, Some("https://member.uhc.com/example")).unwrap();
    }

    fn temp_root(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let root = std::env::temp_dir().join(format!("uhc-auth-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let profile = root.join(format!("piekstra.{BIN}")).join("browser-profile");
        (root, profile)
    }

    /// The status's browser field on a temp dir through `profile_exists_in`,
    /// so the test never reads the owner's real data dir.
    #[test]
    fn auth_status_reports_the_kept_browser_profile() {
        let (root, _) = temp_root("status");
        let profile = root.join(format!("piekstra.{BIN}")).join("browser-profile-adult1");
        let exists = || pk_cli_browser::named_profile_exists_in(&root, BIN, "adult1");
        assert_eq!(browser_profile_status(false, exists), (Some(false), None));
        assert!(!root.exists(), "asking creates nothing");

        std::fs::create_dir_all(&profile).unwrap();
        assert_eq!(browser_profile_status(false, exists), (Some(true), None));
        let mut status = AuthStatus::new(false, false, AuthMethod::BrowserSession);
        status.browser_profile = browser_profile_status(false, exists).0;
        assert_eq!(status.to_json()["browser_profile"], true);
        assert_eq!(status.to_json()["method"], "browser-session");

        // Under the test service the real profile is never asked about.
        let (field, line) = browser_profile_status(true, || panic!("the real profile was read"));
        assert_eq!(field, None);
        assert!(line.unwrap().contains(KEYCHAIN_SERVICE_ENV));

        // A profile that needs repair leaves the field out with a warning.
        std::fs::remove_dir(&profile).unwrap();
        std::fs::write(&profile, "x").unwrap();
        let (field, line) = browser_profile_status(false, exists);
        assert_eq!(field, None);
        assert!(line.unwrap().starts_with("warning: "));
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// The logout's removals on fakes and a temp dir through
    /// `forget_profile_in`: the keychain item first, then the profile, both
    /// reported.
    #[test]
    fn logout_removes_the_session_then_the_kept_profile() {
        let (root, _) = temp_root("logout");
        let profile = root.join(format!("piekstra.{BIN}")).join("browser-profile-adult1");
        std::fs::create_dir_all(profile.join("Default")).unwrap();
        let order = std::cell::RefCell::new(Vec::new());
        let done = run_logout(
            || {
                order.borrow_mut().push("keychain");
                Ok(true)
            },
            Some(|| {
                order.borrow_mut().push("profile");
                pk_cli_browser::forget_named_profile_in(&root, BIN, "adult1")
            }),
            &adult1(),
        )
        .unwrap();
        assert_eq!(*order.borrow(), ["keychain", "profile"]);
        assert_eq!(
            done,
            LoggedOut {
                session_removed: true,
                browser_profile_removed: Some(true)
            }
        );
        assert!(!profile.exists());
        assert_eq!(
            done.to_json(),
            json!({ "schema": "auth-logout/v1", "session_removed": true, "browser_profile_removed": true })
        );
        assert_eq!(
            done.line(),
            "logged out: the session was removed from the keychain; the browser profile was removed"
        );

        let nothing = run_logout(
            || Ok(false),
            Some(|| pk_cli_browser::forget_named_profile_in(&root, BIN, "adult1")),
            &adult1(),
        )
        .unwrap();
        assert_eq!(
            nothing.line(),
            "logged out: no session was stored; no browser profile was kept"
        );

        let left = run_logout(|| Ok(false), None::<fn() -> Result<bool, BrowserError>>, &adult1()).unwrap();
        assert!(left.to_json().get("browser_profile_removed").is_none());
        assert!(left.line().contains("left alone"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Each removal runs whatever the other did, and the error says what was
    /// done.
    #[test]
    fn a_logout_that_fails_halfway_says_what_was_done() {
        let e = run_logout(
            || Ok(true),
            Some(|| Err(BrowserError::ProfileInUse("a Chrome has it".into()))),
            &adult1(),
        )
        .err()
        .unwrap();
        assert_eq!(e.exit_code(), 1);
        assert!(e.to_string().contains("the session was removed"), "{e}");

        let profile_ran = std::cell::Cell::new(false);
        let e = run_logout(
            || Err(CliError::Keychain("denied".into())),
            Some(|| {
                profile_ran.set(true);
                Ok(true)
            }),
            &adult1(),
        )
        .err()
        .unwrap();
        assert!(profile_ran.get());
        assert!(e.to_string().contains("the browser profile was removed"), "{e}");
        assert!(e.to_string().contains("auth logout --profile adult1"), "{e}");
    }
}
