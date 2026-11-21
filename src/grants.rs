//! A profile's grants (medical-common DESIGN §1.1): one per member the
//! member site has shown the profile, each with the household's own label
//! for that member, set by `uhc patients label`. The first name the site
//! shows is the member's `name`, never a label (§1.3), so a member has a
//! label only once the household gives it one.
//!
//! Grants live in the profile's settings in uhc's config, which
//! `ConfigStore::save` writes owner-only and atomically: the labels name the
//! household's members.

use pk_cli_config::profile::ProfileName;
use pk_cli_config::ConfigStore;
use pk_cli_core::CliError;
use serde::{Deserialize, Serialize};

use crate::app::Config;
use crate::model::ORGANIZATION;

/// A profile's settings in uhc's config.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub grants: Vec<Grant>,
}

/// A member the profile's sign-in sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub organization: String,
    /// The member's `PatientContext.id`.
    pub patient: String,
    /// The household's own name for the member.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl Settings {
    /// The household's label for `patient`, if it has one.
    pub fn label(&self, patient: &str) -> Option<&str> {
        self.grants
            .iter()
            .find(|g| g.patient == patient)
            .and_then(|g| g.label.as_deref())
    }

    /// Add a grant for each of `patients` not held yet; whether one was.
    pub fn record<'a>(&mut self, patients: impl IntoIterator<Item = &'a str>) -> bool {
        let mut added = false;
        for p in patients {
            if !self.grants.iter().any(|g| g.patient == p) {
                self.grants.push(Grant {
                    organization: ORGANIZATION.into(),
                    patient: p.to_string(),
                    label: None,
                });
                added = true;
            }
        }
        added
    }

    /// The grant `q` names: a member's id, or its label in any case.
    pub fn find(&self, q: &str) -> Option<usize> {
        let q = q.trim();
        self.grants.iter().position(|g| g.patient == q).or_else(|| {
            self.grants
                .iter()
                .position(|g| g.label.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(q)))
        })
    }

    /// Give grant `i` the label `who`, or clear it with `None`; whether that
    /// changed it. A label is unique within the profile and never equal to a
    /// member's id, so `--patient` is never ambiguous (DESIGN §1).
    pub fn set_label(&mut self, i: usize, who: Option<&str>) -> Result<bool, CliError> {
        let who = match who {
            None => None,
            Some(w) => Some(valid_label(w)?),
        };
        if let Some(w) = &who {
            if self.grants.iter().any(|g| g.patient.eq_ignore_ascii_case(w)) {
                return Err(CliError::Usage("a label cannot be a member's id".into()));
            }
            let taken = self
                .grants
                .iter()
                .enumerate()
                .any(|(j, g)| j != i && g.label.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(w)));
            if taken {
                return Err(CliError::Usage("another member of the profile has that label".into()));
            }
        }
        let changed = self.grants[i].label != who;
        self.grants[i].label = who;
        Ok(changed)
    }
}

/// `who` as a label: trimmed, not empty, one line of printable text.
fn valid_label(who: &str) -> Result<String, CliError> {
    let who = who.trim();
    if who.is_empty() {
        return Err(CliError::Usage("the label is empty".into()));
    }
    if who.chars().any(char::is_control) {
        return Err(CliError::Usage(
            "a label is one line of text, with no control characters".into(),
        ));
    }
    Ok(who.to_string())
}

/// uhc's config and `profile`'s settings in it.
pub fn load(store: &ConfigStore, profile: &ProfileName) -> Result<(Config, Settings), CliError> {
    let config: Config = store.load()?;
    let settings = config.profiles.settings::<Settings>(profile)?;
    Ok((config, settings))
}

/// Write `settings` as `profile`'s, in `config`.
pub fn save(
    store: &ConfigStore,
    mut config: Config,
    profile: &ProfileName,
    settings: &Settings,
) -> Result<(), CliError> {
    config.profiles.set_settings(profile, settings)?;
    store.save(&config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three() -> Settings {
        let mut s = Settings::default();
        assert!(s.record(["ma", "mb", "mc"]));
        assert!(!s.record(["mb"]), "held already");
        s
    }

    #[test]
    fn a_member_is_found_by_id_or_label_and_labels_stay_unique() {
        let mut s = three();
        assert_eq!(s.find("mb"), Some(1));
        assert_eq!(s.find("Robin"), None);
        assert!(s.set_label(1, Some("  Robin ")).unwrap());
        assert_eq!(s.label("mb"), Some("Robin"));
        assert_eq!(s.find("robin"), Some(1));
        assert!(
            !s.set_label(1, Some("Robin")).unwrap(),
            "the same label changes nothing"
        );
        assert!(
            s.set_label(0, Some("ROBIN")).is_err(),
            "another member's label, in any case"
        );
        assert!(s.set_label(0, Some("mc")).is_err(), "a member's id");
        assert!(s.set_label(0, Some(" ")).is_err());
        assert!(s.set_label(0, Some("a\nb")).is_err());
        assert!(
            s.set_label(1, Some("Robin Two")).unwrap(),
            "a new label replaces the old"
        );
        assert!(s.set_label(1, None).unwrap());
        assert!(
            !s.set_label(1, None).unwrap(),
            "clearing an unset label changes nothing"
        );
        assert_eq!(s.label("mb"), None);
    }

    #[test]
    fn settings_read_and_write_as_grants() {
        let mut s = three();
        s.set_label(2, Some("Kit")).unwrap();
        let v = serde_json::to_value(&s).unwrap();
        assert_eq!(
            v["grants"][2],
            serde_json::json!({"organization": "uhc", "patient": "mc", "label": "Kit"})
        );
        assert!(v["grants"][0].get("label").is_none());
        assert_eq!(serde_json::from_value::<Settings>(v).unwrap(), s);
        assert_eq!(
            serde_json::to_value(Settings::default()).unwrap(),
            serde_json::json!({})
        );
    }
}
