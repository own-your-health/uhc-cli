//! Where a profile's member-site session is kept: the one seam that knows.
//!
//! A [`Session`] is the `Cookie` header value, the browser's user agent
//! (none for a `--stdin` import without `--user-agent`) and when it was
//! captured, kept as one JSON item per profile. [`SessionStore`] gets, puts
//! and deletes it by profile; nothing else in uhc knows where it lives.
//!
//! For now the item is the OS keychain's, service `piekstra.uhc`, account
//! `session/<profile>`. cli-common v0.33.0 moves secrets into
//! `pk_cli_auth::vault::Vault::for_binary("uhc")` (entries named
//! `<profile>/session`); adopting it replaces this module's body and
//! nothing outside it.
//!
//! [`KEYCHAIN_SERVICE_ENV`] names another service, for the tests and the
//! smoke run: a service that holds nothing reads as signed out, with no
//! keychain prompt. While it is set ([`hermetic`]) nothing touches the real
//! browser profiles either.

use pk_cli_core::{CliError, Reason};
use pk_cli_secrets::{CredentialStore, Secret};
use serde::{Deserialize, Serialize};

use crate::cli::BIN;
use crate::source::Profile;

/// Names the keychain service read and written instead of `piekstra.uhc`.
/// For the tests and the smoke run only; it never carries a session.
pub const KEYCHAIN_SERVICE_ENV: &str = "UHC_KEYCHAIN_SERVICE";

/// The stored session. The cookie header is the secret; the rest is what
/// `auth status` reports and what a read sends beside it.
pub struct Session {
    pub cookie_header: Secret,
    pub user_agent: Option<String>,
    /// RFC 3339, UTC.
    pub captured_at: String,
}

#[derive(Serialize)]
struct ItemOut<'a> {
    cookie_header: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<&'a str>,
    captured_at: &'a str,
}

#[derive(Deserialize)]
struct ItemIn {
    cookie_header: String,
    #[serde(default)]
    user_agent: Option<String>,
    captured_at: String,
}

impl Session {
    /// A session captured now.
    pub fn new(cookie_header: Secret, user_agent: Option<String>) -> Self {
        Session {
            cookie_header,
            user_agent,
            captured_at: crate::app::now_rfc3339(),
        }
    }

    /// The same session with the cookie header the site's answers updated;
    /// when it was captured stays.
    pub fn with_cookie_header(&self, cookie_header: Secret) -> Session {
        Session {
            cookie_header,
            user_agent: self.user_agent.clone(),
            captured_at: self.captured_at.clone(),
        }
    }

    /// The item's text, held as a secret.
    fn to_item(&self) -> Result<Secret, CliError> {
        serde_json::to_string(&ItemOut {
            cookie_header: self.cookie_header.expose(),
            user_agent: self.user_agent.as_deref(),
            captured_at: &self.captured_at,
        })
        .map(Secret::new)
        .map_err(|_| CliError::Other("serializing the session item".into()))
    }

    /// The session an item holds; `None` for an item of any other shape.
    fn from_item(item: &Secret) -> Option<Session> {
        let read: ItemIn = serde_json::from_str(item.expose()).ok()?;
        (!read.cookie_header.is_empty()).then(|| Session {
            cookie_header: Secret::new(read.cookie_header),
            user_agent: read.user_agent,
            captured_at: read.captured_at,
        })
    }
}

/// The sessions, one per profile.
pub struct SessionStore {
    creds: CredentialStore,
}

impl SessionStore {
    /// The store every command uses: `piekstra.uhc`, unless
    /// [`KEYCHAIN_SERVICE_ENV`] names another service.
    pub fn open() -> SessionStore {
        SessionStore::for_service(service_env().as_deref())
    }

    fn for_service(value: Option<&str>) -> SessionStore {
        let creds = match service_override(value) {
            Some(service) => CredentialStore::new(service),
            None => CredentialStore::for_binary(BIN),
        };
        SessionStore { creds }
    }

    /// The profile's session, `None` when none is stored. A stored item
    /// this uhc cannot read is an error naming the profile, never what the
    /// item holds ([`Reason::CredentialUnreadable`]).
    pub fn get(&self, profile: &Profile) -> Result<Option<Session>, CliError> {
        match self.creds.get(&account(profile))? {
            None => Ok(None),
            Some(item) => Session::from_item(&item).map(Some).ok_or_else(|| unreadable(profile)),
        }
    }

    /// The profile's session, for a command that reads the site: none
    /// stored is exit 3, with the sign-in that stores one.
    pub fn load(&self, profile: &Profile) -> Result<Session, CliError> {
        self.get(profile)?.ok_or_else(|| {
            CliError::Auth(format!("no member-site session is stored for profile `{profile}`"))
                .because(Reason::NoCredential)
                .with_remedy(format!("{BIN} auth login --browser --profile {profile}"))
        })
    }

    pub fn put(&self, profile: &Profile, session: &Session) -> Result<(), CliError> {
        self.creds.set(&account(profile), &session.to_item()?)
    }

    /// Whether there was a session to remove.
    pub fn delete(&self, profile: &Profile) -> Result<bool, CliError> {
        self.creds.delete(&account(profile))
    }
}

fn account(profile: &Profile) -> String {
    format!("session/{}", profile.name())
}

fn unreadable(profile: &Profile) -> CliError {
    CliError::Keychain(format!(
        "the session stored for profile `{profile}` is not the shape this uhc writes; \
         `{BIN} auth logout --profile {profile}` removes it"
    ))
    .because(Reason::CredentialUnreadable)
}

/// Whether [`KEYCHAIN_SERVICE_ENV`] points the store elsewhere: a test or
/// the smoke run, under which nothing reads or removes the owner's kept
/// browser profiles.
pub fn hermetic() -> bool {
    service_override(service_env().as_deref()).is_some()
}

/// The variable's value. One that is not UTF-8 still counts as set, so it
/// never falls back to the real service.
fn service_env() -> Option<String> {
    std::env::var_os(KEYCHAIN_SERVICE_ENV).map(|v| v.to_string_lossy().into_owned())
}

/// A blank value names no service.
fn service_override(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn the_service_is_piekstra_uhc_unless_the_variable_names_another() {
        assert_eq!(SessionStore::for_service(None).creds.service(), "piekstra.uhc");
        assert_eq!(SessionStore::for_service(Some("  ")).creds.service(), "piekstra.uhc");
        assert_eq!(
            SessionStore::for_service(Some("piekstra.uhc.hermetic-tests"))
                .creds
                .service(),
            "piekstra.uhc.hermetic-tests"
        );
    }

    #[test]
    fn each_profile_has_its_own_account() {
        let p = crate::source::resolve_profile("adult1").unwrap();
        assert_eq!(account(&p), "session/adult1");
    }

    /// One item, one JSON object, read back whole; the user agent is left out
    /// when there is none.
    #[test]
    fn the_session_is_one_json_item() {
        let session = Session {
            cookie_header: Secret::new("dummy_a=1; dummy_b=2"),
            user_agent: Some("Mozilla/5.0 (test)".into()),
            captured_at: "2026-01-02T03:04:05Z".into(),
        };
        let item = session.to_item().unwrap();
        let v: Value = serde_json::from_str(item.expose()).unwrap();
        assert_eq!(
            v,
            json!({
                "cookie_header": "dummy_a=1; dummy_b=2",
                "user_agent": "Mozilla/5.0 (test)",
                "captured_at": "2026-01-02T03:04:05Z",
            })
        );
        let read = Session::from_item(&item).unwrap();
        assert_eq!(read.cookie_header.expose(), "dummy_a=1; dummy_b=2");
        assert_eq!(read.user_agent.as_deref(), Some("Mozilla/5.0 (test)"));
        assert_eq!(read.captured_at, "2026-01-02T03:04:05Z");

        let updated = read.with_cookie_header(Secret::new("dummy_a=2"));
        assert_eq!(updated.cookie_header.expose(), "dummy_a=2");
        assert_eq!(updated.captured_at, "2026-01-02T03:04:05Z");

        let bare = Session::new(Secret::new("dummy_a=1"), None).to_item().unwrap();
        let v: Value = serde_json::from_str(bare.expose()).unwrap();
        assert!(v.get("user_agent").is_none());
        assert!(v["captured_at"].as_str().unwrap().ends_with('Z'));
    }

    /// An item of another shape names the profile and never what it holds.
    #[test]
    fn an_unreadable_item_is_named_not_quoted() {
        let p = crate::source::resolve_profile("adult1").unwrap();
        for raw in [
            "dummy_secret=1",
            r#"{"cookie_header":"","captured_at":"x"}"#,
            r#"{"token":"dummy_secret"}"#,
        ] {
            assert!(Session::from_item(&Secret::new(raw)).is_none(), "{raw}");
        }
        let e = unreadable(&p);
        assert_eq!(e.exit_code(), 1);
        assert_eq!(e.reason(), Some(Reason::CredentialUnreadable));
        assert!(e.message().contains("profile `adult1`"), "{}", e.message());
    }
}
