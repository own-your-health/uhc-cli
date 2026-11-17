//! Which source a command reads, and as which profile.
//!
//! - The files source applies only when it is named: `--files <DIR>` (or
//!   `--dir`, or `UHC_IMPORT_DIR`). Otherwise a command that can read
//!   either reads the member site as the profile `--profile <NAME>` (or
//!   `UHC_PROFILE`) names, and a missing profile is exit 2. No flag never
//!   means the files.
//! - A flag on the command line wins over the other source's variable;
//!   both flags, or both variables and neither flag, are exit 2.
//! - A profile must be configured (`uhc profiles add <NAME>`):
//!   [`resolve_profile`] is `pk_cli_config::profile::resolve`, so a missing,
//!   malformed or unknown name is exit 2 listing the configured ones, and a
//!   lone profile is never picked.
//! - The commands that need no sign-in (`import check`, `info`, `config`,
//!   `profiles`, `auth reset`, `self-update`, `completions`) take no
//!   `--profile`.

use std::path::PathBuf;

use clap::parser::ValueSource;
use clap::ArgMatches;
use pk_cli_config::profile::{self, ProfileName, Profiles};
use pk_cli_core::CliError;

use crate::cli::{Cli, BIN};

pub const PROFILE_ENV: &str = "UHC_PROFILE";
pub const DIR_ENV: &str = "UHC_IMPORT_DIR";

/// The profile a command signs in as: `given` (the flag, or clap's value
/// from `UHC_PROFILE`), which must be configured.
pub fn resolve_profile(given: Option<&str>, configured: &Profiles) -> Result<ProfileName, CliError> {
    profile::resolve(given.filter(|g| !g.is_empty()), PROFILE_ENV, configured, BIN)
}

/// Where a command reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// The EOB statement PDFs downloaded into this folder.
    Files(PathBuf),
    /// The member site, signed in as this profile.
    Site(ProfileName),
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
pub fn source(cli: &Cli, configured: &Profiles) -> Result<Source, CliError> {
    let given = cli.given;
    let profile = cli.profile.as_deref().filter(|p| !p.is_empty());
    match (profile, &cli.dir) {
        (Some(_), Some(_)) if given.profile_flag && given.dir_flag => Err(CliError::Usage(
            "--profile reads the member site and --files the downloaded statements; give one".into(),
        )),
        (Some(p), Some(_)) if given.profile_flag => Ok(Source::Site(resolve_profile(Some(p), configured)?)),
        (Some(_), Some(d)) if given.dir_flag => Ok(Source::Files(d.clone())),
        (Some(_), Some(_)) => Err(CliError::Usage(format!(
            "{PROFILE_ENV} and {DIR_ENV} both name a source; pass --profile <NAME> or --files <DIR> to choose"
        ))),
        (Some(p), None) => Ok(Source::Site(resolve_profile(Some(p), configured)?)),
        (None, Some(d)) => Ok(Source::Files(d.clone())),
        (None, None) => Err(CliError::Usage(format!(
            "say where to read: --profile <NAME> for the member site (signed in with `{BIN} auth login --browser \
             --profile <NAME>`), or --files <DIR> for the EOB statements downloaded into a folder"
        ))),
    }
}

/// The profile of a command that only the member site has (`auth login`,
/// `status`, `logout`, `api`).
pub fn site_profile(cli: &Cli, configured: &Profiles) -> Result<ProfileName, CliError> {
    if cli.given.dir_flag {
        return Err(CliError::Usage(
            "the sign-in is the member site's; --files reads downloaded statements and needs none".into(),
        ));
    }
    resolve_profile(cli.profile.as_deref(), configured)
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

    fn parse(args: &[&str]) -> Cli {
        let matches = Cli::command().try_get_matches_from(args).unwrap();
        let mut cli = Cli::from_arg_matches(&matches).unwrap();
        cli.given = Given::of(&matches);
        cli
    }

    fn configured(names: &[&str]) -> Profiles {
        let mut p = Profiles::new();
        for n in names {
            p.add(ProfileName::parse(n).unwrap());
        }
        p
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
        assert_eq!(source(&cli, &Profiles::new()).unwrap(), Source::Files("/tmp/x".into()));
        let cli = parse(&["uhc", "--dir", "/tmp/x", "claims", "list"]);
        assert!(cli.given.dir_flag);
    }

    #[test]
    fn the_source_is_named_never_assumed() {
        let p = configured(&["adult1", "adult2"]);
        let mut cli = parse(&["uhc", "claims", "list"]);
        cli.profile = None;
        cli.dir = None;
        assert_eq!(source(&cli, &p).unwrap_err().exit_code(), 2);

        let cli = parse(&["uhc", "--profile", "adult1", "claims", "list"]);
        assert_eq!(
            source(&cli, &p).unwrap(),
            Source::Site(ProfileName::parse("adult1").unwrap())
        );

        let cli = parse(&["uhc", "--profile", "adult1", "--files", "/tmp/x", "claims", "list"]);
        assert_eq!(source(&cli, &p).unwrap_err().exit_code(), 2);

        // A flag wins over the other source's variable.
        let mut cli = parse(&["uhc", "--profile", "adult1", "claims", "list"]);
        cli.dir = Some("/tmp/from-env".into());
        assert!(matches!(source(&cli, &p).unwrap(), Source::Site(_)));
        let mut cli = parse(&["uhc", "--files", "/tmp/x", "claims", "list"]);
        cli.profile = Some("from-env".into());
        assert_eq!(source(&cli, &p).unwrap(), Source::Files("/tmp/x".into()));
        // Both variables and no flag: no guess.
        let mut cli = parse(&["uhc", "claims", "list"]);
        cli.profile = Some("adult1".into());
        cli.dir = Some("/tmp/from-env".into());
        assert_eq!(source(&cli, &p).unwrap_err().exit_code(), 2);
    }

    /// Malformed or not configured is exit 2 naming the configured ones; a
    /// lone profile is never picked for a command that names none.
    #[test]
    fn a_profile_must_be_configured() {
        let one = configured(&["adult1"]);
        for name in ["Bad Name", "adult3", "-x", "Adult1"] {
            let cli = parse(&["uhc", &format!("--profile={name}"), "claims", "list"]);
            assert_eq!(source(&cli, &one).unwrap_err().exit_code(), 2, "{name}");
        }
        let cli = parse(&["uhc", "--profile", "adult3", "claims", "list"]);
        assert!(source(&cli, &one).unwrap_err().to_string().contains("adult1"));
        let mut cli = parse(&["uhc", "auth", "status"]);
        cli.profile = None;
        let e = site_profile(&cli, &one).unwrap_err();
        assert_eq!(e.exit_code(), 2);
        assert!(e.to_string().contains("adult1"), "{e}");
        let e = site_profile(&cli, &Profiles::new()).unwrap_err();
        assert!(e.remedy().unwrap().contains("uhc profiles add"), "{e}");
    }

    #[test]
    fn sign_in_commands_need_a_profile_and_no_folder() {
        let p = configured(&["adult1"]);
        let cli = parse(&["uhc", "auth", "status", "--profile", "adult1"]);
        assert_eq!(site_profile(&cli, &p).unwrap().as_str(), "adult1");
        let cli = parse(&["uhc", "--files", "/tmp/x", "auth", "status", "--profile", "adult1"]);
        assert_eq!(site_profile(&cli, &p).unwrap_err().exit_code(), 2);
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
