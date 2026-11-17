//! Where a profile's member-site session is kept: the one seam that knows.
//!
//! A [`Session`] is the `Cookie` header value, the browser's user agent
//! (none for a `--stdin` import without `--user-agent`) and when it was
//! captured, kept as one JSON entry per profile. [`SessionStore`] gets, puts
//! and deletes it by profile; nothing else in uhc knows where it lives.
//!
//! The entries are the vault's (`pk_cli_auth::vault`, cli-common DESIGN.md
//! §2.5): `<profile>/session`, each in its own encrypted owner-only file
//! under `~/.config/uhc/vault/`, all under one key, the keychain item
//! `vault-key` in `piekstra.uhc`. A new build of uhc asks the keychain once,
//! however many profiles are signed in, and a run with nothing stored reads
//! no keychain item at all.
//!
//! [`KEYCHAIN_SERVICE_ENV`] names another keychain service for the key, for
//! the tests and the smoke run, and moves the vault under
//! `$XDG_CONFIG_HOME/uhc/vault`, which must then be set: a test's vault is
//! never the owner's. While it is set ([`hermetic`]) nothing touches the
//! real browser profiles either.

use std::path::PathBuf;

use pk_cli_auth::vault::{self, Vault};
use pk_cli_config::profile::ProfileName;
use pk_cli_core::{CliError, Reason};
use pk_cli_secrets::{CredentialStore, Secret};
use serde::{Deserialize, Serialize};

use crate::cli::BIN;

/// Names the keychain service holding the vault's key instead of
/// `piekstra.uhc`, and moves the vault under `$XDG_CONFIG_HOME`. For the
/// tests and the smoke run only.
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
struct EntryOut<'a> {
    cookie_header: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_agent: Option<&'a str>,
    captured_at: &'a str,
}

#[derive(Deserialize)]
struct EntryIn {
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

    fn entry(&self) -> EntryOut<'_> {
        EntryOut {
            cookie_header: self.cookie_header.expose(),
            user_agent: self.user_agent.as_deref(),
            captured_at: &self.captured_at,
        }
    }

    /// The session an entry holds; `None` for one with an empty header.
    fn from_entry(read: EntryIn) -> Option<Session> {
        (!read.cookie_header.is_empty()).then(|| Session {
            cookie_header: Secret::new(read.cookie_header),
            user_agent: read.user_agent,
            captured_at: read.captured_at,
        })
    }
}

/// The sessions, one per profile.
pub struct SessionStore {
    vault: Vault,
}

impl SessionStore {
    /// The store every command uses: the vault of `piekstra.uhc`, or with
    /// [`KEYCHAIN_SERVICE_ENV`] a test's ([`hermetic`]).
    pub fn open() -> Result<SessionStore, CliError> {
        let vault = match service_override(service_env().as_deref()) {
            None => Vault::for_binary(BIN)?,
            Some(service) => Vault::new(CredentialStore::new(service), test_vault_dir()?),
        };
        Ok(SessionStore { vault })
    }

    /// A store over `vault`: the tests' in-memory one
    /// (`Vault::new(MemoryKeys::new(), dir)`).
    pub fn with_vault(vault: Vault) -> SessionStore {
        SessionStore { vault }
    }

    /// The profile's session, `None` when none is stored. An entry this uhc
    /// cannot read is an error naming the profile and the command that
    /// removes it ([`Reason::CredentialUnreadable`]), never what it holds.
    pub fn get(&self, profile: &ProfileName) -> Result<Option<Session>, CliError> {
        let Some(raw) = self.vault.get(&entry(profile)).map_err(|e| remedied(e, profile))? else {
            return Ok(None);
        };
        // Parsed here, not by `get_json`, so an entry that decrypts but is
        // not a session is uhc's own typed case.
        let read: EntryIn = serde_json::from_str(raw.expose()).map_err(|_| unreadable(profile))?;
        Session::from_entry(read).map(Some).ok_or_else(|| unreadable(profile))
    }

    /// The profile's session, for a command that reads the site: none
    /// stored is exit 3, with the sign-in that stores one.
    pub fn load(&self, profile: &ProfileName) -> Result<Session, CliError> {
        self.get(profile)?.ok_or_else(|| {
            CliError::Auth(format!("no member-site session is stored for profile `{profile}`"))
                .because(Reason::NoCredential)
                .with_remedy(format!("{BIN} auth login --browser --profile {profile}"))
        })
    }

    pub fn put(&self, profile: &ProfileName, session: &Session) -> Result<(), CliError> {
        self.vault
            .put_json(&entry(profile), &session.entry())
            .map_err(|e| remedied(e, profile))
    }

    /// Whether there was a session to remove. Reads no keychain item.
    pub fn delete(&self, profile: &ProfileName) -> Result<bool, CliError> {
        self.vault.delete(&entry(profile))
    }

    /// Remove every profile's session and the vault's key (`auth reset`).
    /// How many entries were removed.
    pub fn clear(&self) -> Result<usize, CliError> {
        self.vault.clear()
    }
}

/// The vault entry of a profile's session.
fn entry(profile: &ProfileName) -> String {
    format!("{profile}/session")
}

/// The vault's errors with uhc's own commands as their remedies.
fn remedied(e: CliError, profile: &ProfileName) -> CliError {
    match e.remedy() {
        Some(vault::REMEDY_DELETE_ENTRY) => e
            .with_remedy(format!("{BIN} auth logout --profile {profile}, then sign in again"))
            .because(Reason::CredentialUnreadable),
        Some(vault::REMEDY_CLEAR) => e.with_remedy(format!(
            "{BIN} auth reset removes every profile's session and the key; then sign in again"
        )),
        _ => e,
    }
}

fn unreadable(profile: &ProfileName) -> CliError {
    CliError::Keychain(format!(
        "the session stored for profile `{profile}` is not the shape this uhc writes"
    ))
    .because(Reason::CredentialUnreadable)
    .with_remedy(format!("{BIN} auth logout --profile {profile}, then sign in again"))
}

/// Whether [`KEYCHAIN_SERVICE_ENV`] points the store elsewhere: a test or
/// the smoke run, under which nothing reads or removes the owner's kept
/// browser profiles.
pub fn hermetic() -> bool {
    service_override(service_env().as_deref()).is_some()
}

/// A test's vault: under `$XDG_CONFIG_HOME/uhc`, which must be set, so a
/// test never opens the owner's `~/.config/uhc/vault`.
fn test_vault_dir() -> Result<PathBuf, CliError> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(base) => Ok(PathBuf::from(base).join(BIN).join("vault")),
        None => Err(CliError::Usage(format!(
            "{KEYCHAIN_SERVICE_ENV} is for tests and goes with XDG_CONFIG_HOME, so a test's vault is never the \
             owner's"
        ))),
    }
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
    use pk_cli_auth::vault::MemoryKeys;
    use std::sync::Arc;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uhc-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn adult1() -> ProfileName {
        ProfileName::parse("adult1").unwrap()
    }

    #[test]
    fn a_blank_service_names_none() {
        assert_eq!(service_override(None), None);
        assert_eq!(service_override(Some("  ")), None);
        assert_eq!(
            service_override(Some("piekstra.uhc.hermetic-tests")),
            Some("piekstra.uhc.hermetic-tests")
        );
    }

    /// One entry per profile, `<profile>/session`; storing one profile's
    /// session leaves another's alone, and a store with nothing in it reads
    /// no key.
    #[test]
    fn each_profile_has_its_own_entry() {
        let dir = temp_dir("entries");
        let keys = Arc::new(MemoryKeys::new());
        let store = SessionStore::with_vault(Vault::new(keys.clone(), &dir));
        let adult2 = ProfileName::parse("adult2").unwrap();
        assert!(store.get(&adult1()).unwrap().is_none());
        assert_eq!(keys.reads(), 0, "nothing stored, nothing read");
        assert_eq!(entry(&adult1()), "adult1/session");

        let session = Session {
            cookie_header: Secret::new("dummy_a=1; dummy_b=2"),
            user_agent: Some("Mozilla/5.0 (test)".into()),
            captured_at: "2026-01-02T03:04:05Z".into(),
        };
        store.put(&adult1(), &session).unwrap();
        let read = store.get(&adult1()).unwrap().unwrap();
        assert_eq!(read.cookie_header.expose(), "dummy_a=1; dummy_b=2");
        assert_eq!(read.user_agent.as_deref(), Some("Mozilla/5.0 (test)"));
        assert_eq!(read.captured_at, "2026-01-02T03:04:05Z");
        assert!(store.get(&adult2).unwrap().is_none());

        let updated = read.with_cookie_header(Secret::new("dummy_a=2"));
        store.put(&adult1(), &updated).unwrap();
        let read = store.get(&adult1()).unwrap().unwrap();
        assert_eq!(read.cookie_header.expose(), "dummy_a=2");
        assert_eq!(read.captured_at, "2026-01-02T03:04:05Z");

        // No secret on disk in the clear.
        for file in std::fs::read_dir(&dir).unwrap() {
            let bytes = std::fs::read(file.unwrap().path()).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("dummy_a"));
        }
        assert!(store.delete(&adult1()).unwrap());
        assert!(!store.delete(&adult1()).unwrap());
        assert!(store.load(&adult1()).err().unwrap().is_auth());
        assert_eq!(store.clear().unwrap(), 0);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An entry of another shape names the profile and the command that
    /// removes it, never what it holds.
    #[test]
    fn an_unreadable_entry_is_named_not_quoted() {
        let dir = temp_dir("unreadable");
        let vault = Vault::new(MemoryKeys::new(), &dir);
        vault
            .put("adult1/session", &Secret::new(r#"{"token":"dummy_secret"}"#))
            .unwrap();
        vault
            .put(
                "adult2/session",
                &Secret::new(r#"{"cookie_header":"","captured_at":"x"}"#),
            )
            .unwrap();
        let store = SessionStore::with_vault(vault);
        for name in ["adult1", "adult2"] {
            let e = store.get(&ProfileName::parse(name).unwrap()).err().unwrap();
            assert_eq!(e.exit_code(), 1, "{name}");
            assert_eq!(e.reason(), Some(Reason::CredentialUnreadable), "{name}");
            assert!(!e.to_string().contains("dummy"), "{e}");
            assert!(
                e.remedy().unwrap().contains(&format!("auth logout --profile {name}")),
                "{e}"
            );
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Entries with no key to read them: the remedy is `auth reset`.
    #[test]
    fn a_lost_key_names_auth_reset() {
        let dir = temp_dir("lost");
        let keys = Arc::new(MemoryKeys::new());
        let store = SessionStore::with_vault(Vault::new(keys.clone(), &dir));
        store
            .put(&adult1(), &Session::new(Secret::new("dummy_a=1"), None))
            .unwrap();
        keys.forget();
        let fresh = SessionStore::with_vault(Vault::new(keys, &dir));
        let e = fresh.get(&adult1()).err().unwrap();
        assert!(e.remedy().unwrap().contains("auth reset"), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
