//! Which source a command reads, and which member-site profile.
//!
//! Provisional, kept in this one module until cli-common v0.33.0 brings
//! `pk_cli_config::profile` (medical-common's shared profile design):
//!
//! - The files source applies only when it is named: `--files <DIR>` (or
//!   `--dir`, or `UHC_IMPORT_DIR`). Otherwise a command that can read
//!   either reads the member site, as the profile `--profile <NAME>` (or
//!   `UHC_PROFILE`) names, and a missing profile is exit 2. No flag never
//!   means the files.
//! - A flag on the command line wins over the other source's variable;
//!   both flags, or both variables and neither flag, are exit 2.
//! - The commands that need no sign-in (`import check`, `info`, `config`,
//!   `self-update`, `completions`) take no `--profile`.
//! - [`resolve_profile`] checks a name's shape and accepts any well-formed
//!   one. v0.33.0's `resolve(...)` replaces its body: an unknown name is
//!   exit 2 there, listing the configured ones.

use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::ArgMatches;
use pk_cli_core::CliError;

use crate::cli::{Cli, BIN};

pub const PROFILE_ENV: &str = "UHC_PROFILE";
pub const DIR_ENV: &str = "UHC_IMPORT_DIR";
/// The longest profile name.
pub const MAX_PROFILE_LEN: usize = 32;

/// A member-site profile: one signed-in account at the site.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile(String);

impl Profile {
    pub fn name(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The one place a profile name becomes a [`Profile`]. A name is 1 to 32
/// characters: lowercase ASCII letters, digits, `-` and `_`, starting with a
/// letter or a digit. Anything else is exit 2. Any well-formed name is
/// accepted for now; v0.33.0's `pk_cli_config::profile::resolve` replaces
/// this body and refuses a name no profile is configured under.
pub fn resolve_profile(name: &str) -> Result<Profile, CliError> {
    let ok_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
    let first_ok = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if name.is_empty() || name.len() > MAX_PROFILE_LEN || !first_ok || !name.chars().all(ok_char) {
        return Err(CliError::Usage(format!(
            "profile name `{}` must be 1 to {MAX_PROFILE_LEN} lowercase letters, digits, `-` or `_`, starting \
             with a letter or a digit",
            name.escape_debug()
        )));
    }
    Ok(Profile(name.to_string()))
}

/// Where a command reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The EOB statement PDFs downloaded into this folder.
    Files(PathBuf),
    /// The member site, signed in as this profile.
    Site(Profile),
}

/// Whether `--profile` and `--dir` were given on the command line rather
/// than through their variables.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Given {
    pub profile_flag: bool,
    pub dir_flag: bool,
}

impl Given {
    pub fn of(matches: &ArgMatches) -> Given {
        let flag = |id: &str| {
            let mut m = matches;
            let mut on_line = m.value_source(id) == Some(ValueSource::CommandLine);
            // A global flag given after a subcommand is that subcommand's.
            while let Some((_, sub)) = m.subcommand() {
                m = sub;
                on_line |= m.value_source(id) == Some(ValueSource::CommandLine);
            }
            on_line
        };
        Given {
            profile_flag: flag("profile"),
            dir_flag: flag("dir"),
        }
    }
}

/// The source of a command that reads either.
pub fn source(cli: &Cli) -> Result<Source, CliError> {
    let given = cli.given;
    match (&cli.profile, &cli.dir) {
        (Some(_), Some(_)) if given.profile_flag && given.dir_flag => Err(CliError::Usage(
            "--profile reads the member site and --files the downloaded statements; give one".into(),
        )),
        (Some(p), Some(_)) if given.profile_flag => Ok(Source::Site(resolve_profile(p)?)),
        (Some(_), Some(d)) if given.dir_flag => Ok(Source::Files(d.clone())),
        (Some(_), Some(_)) => Err(CliError::Usage(format!(
            "{PROFILE_ENV} and {DIR_ENV} both name a source; pass --profile <NAME> or --files <DIR> to choose"
        ))),
        (Some(p), None) => Ok(Source::Site(resolve_profile(p)?)),
        (None, Some(d)) => Ok(Source::Files(d.clone())),
        (None, None) => Err(CliError::Usage(format!(
            "say where to read: --profile <NAME> for the member site (signed in with `{BIN} auth login --browser \
             --profile <NAME>`), or --files <DIR> for the EOB statements downloaded into a folder"
        ))),
    }
}

/// The profile of a command that only the member site has (`auth`).
pub fn site_profile(cli: &Cli) -> Result<Profile, CliError> {
    if cli.given.dir_flag {
        return Err(CliError::Usage(
            "the sign-in is the member site's; --files reads downloaded statements and needs none".into(),
        ));
    }
    match &cli.profile {
        Some(p) => resolve_profile(p),
        None => Err(CliError::Usage(format!(
            "name the member-site account: --profile <NAME> (or {PROFILE_ENV}), 1 to {MAX_PROFILE_LEN} lowercase \
             letters, digits, `-` or `_`"
        ))),
    }
}

/// For a command that needs no sign-in: `--profile` on its command line is
/// exit 2, and `UHC_PROFILE` is not read.
pub fn no_profile(cli: &Cli, command: &str) -> Result<(), CliError> {
    if cli.given.profile_flag {
        return Err(CliError::Usage(format!(
            "`{BIN} {command}` needs no sign-in and takes no --profile"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, FromArgMatches};

    #[test]
    fn profile_names_are_short_lowercase_words() {
        for ok in ["adult1", "a", "0kid", "kid_2", "x-y", &"a".repeat(32)] {
            assert_eq!(resolve_profile(ok).unwrap().name(), ok);
        }
        for bad in ["", "Adult", "-a", "_a", "a b", "a.b", "é", &"a".repeat(33), "a/b"] {
            let e = resolve_profile(bad).err().unwrap();
            assert_eq!(e.exit_code(), 2, "{bad:?}");
        }
    }

    fn parse(args: &[&str]) -> Cli {
        let matches = Cli::command().try_get_matches_from(args).unwrap();
        let mut cli = Cli::from_arg_matches(&matches).unwrap();
        cli.given = Given::of(&matches);
        cli
    }

    /// Before or after the subcommand, a flag is the command line's; the
    /// variables are not (clap fills the values from them, so the tests
    /// set them through clap's own fields).
    #[test]
    fn a_flag_counts_wherever_it_is_given() {
        let cli = parse(&["uhc", "--profile", "adult1", "claims", "list"]);
        assert!(cli.given.profile_flag && !cli.given.dir_flag);
        let cli = parse(&["uhc", "claims", "list", "--profile", "adult1"]);
        assert!(cli.given.profile_flag);
        let cli = parse(&["uhc", "claims", "list", "--files", "/tmp/x"]);
        assert!(cli.given.dir_flag && !cli.given.profile_flag);
        assert_eq!(source(&cli).unwrap(), Source::Files("/tmp/x".into()));
        let cli = parse(&["uhc", "--dir", "/tmp/x", "claims", "list"]);
        assert!(cli.given.dir_flag);
    }

    #[test]
    fn the_source_is_named_never_assumed() {
        let mut cli = parse(&["uhc", "claims", "list"]);
        cli.profile = None;
        cli.dir = None;
        assert_eq!(source(&cli).unwrap_err().exit_code(), 2);

        let cli = parse(&["uhc", "--profile", "adult1", "claims", "list"]);
        assert_eq!(source(&cli).unwrap(), Source::Site(resolve_profile("adult1").unwrap()));

        let cli = parse(&["uhc", "--profile", "adult1", "--files", "/tmp/x", "claims", "list"]);
        assert_eq!(source(&cli).unwrap_err().exit_code(), 2);

        // A flag wins over the other source's variable.
        let mut cli = parse(&["uhc", "--profile", "adult1", "claims", "list"]);
        cli.dir = Some("/tmp/from-env".into());
        assert!(matches!(source(&cli).unwrap(), Source::Site(_)));
        let mut cli = parse(&["uhc", "--files", "/tmp/x", "claims", "list"]);
        cli.profile = Some("from-env".into());
        assert_eq!(source(&cli).unwrap(), Source::Files("/tmp/x".into()));
        // Both variables and no flag: no guess.
        let mut cli = parse(&["uhc", "claims", "list"]);
        cli.profile = Some("from-env".into());
        cli.dir = Some("/tmp/from-env".into());
        assert_eq!(source(&cli).unwrap_err().exit_code(), 2);

        let cli = parse(&["uhc", "--profile", "Bad Name", "claims", "list"]);
        assert_eq!(source(&cli).unwrap_err().exit_code(), 2);
    }

    #[test]
    fn sign_in_commands_need_a_profile_and_no_folder() {
        let mut cli = parse(&["uhc", "auth", "status"]);
        cli.profile = None;
        assert_eq!(site_profile(&cli).unwrap_err().exit_code(), 2);
        let cli = parse(&["uhc", "auth", "status", "--profile", "adult1"]);
        assert_eq!(site_profile(&cli).unwrap().name(), "adult1");
        let cli = parse(&["uhc", "--files", "/tmp/x", "auth", "status", "--profile", "adult1"]);
        assert_eq!(site_profile(&cli).unwrap_err().exit_code(), 2);
    }

    #[test]
    fn commands_without_a_sign_in_take_no_profile_flag() {
        let cli = parse(&["uhc", "--profile", "adult1", "info"]);
        assert_eq!(no_profile(&cli, "info").unwrap_err().exit_code(), 2);
        let mut cli = parse(&["uhc", "info"]);
        cli.profile = Some("from-env".into());
        no_profile(&cli, "info").unwrap();
    }
}
