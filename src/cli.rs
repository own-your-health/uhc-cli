//! The command tree (piekstra-cli/1, health-records/v1, documents/v1).

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;
use pk_cli_auth::LoginArgs;
use pk_cli_browser::BrowserArgs;
use pk_cli_core::{CommonArgs, RangeArgs};
use pk_cli_selfupdate::SelfUpdateArgs;

pub const BIN: &str = "uhc";

/// UnitedHealthcare coverage, claims, EOBs and deductible progress, read from
/// the EOB statements you download from myuhc.com. Read-only; unofficial.
#[derive(Parser, Debug)]
#[command(name = BIN, version, about, long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub common: CommonArgs,

    /// Override the config file location.
    #[arg(long, global = true, value_name = "PATH")]
    pub config: Option<PathBuf>,

    /// The folder holding your downloaded EOB statement PDFs (overrides
    /// `config set import_dir`).
    #[arg(long, global = true, env = "UHC_IMPORT_DIR", value_name = "DIR")]
    pub dir: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// The member-site sign-in (myuhc.com). The files source needs none.
    #[command(subcommand)]
    Auth(AuthCmd),
    /// Non-secret settings (`import_dir`).
    #[command(subcommand)]
    Config(ConfigCmd),
    /// The members the statements name: subscriber and dependents (patient-list/v1).
    #[command(subcommand)]
    Patients(PatientsCmd),
    /// Coverage and claims as records-stream/v1, for `medrec import --from uhc`.
    #[command(subcommand)]
    Records(RecordsCmd),
    /// Claims, each once, as its latest statement prints it.
    #[command(subcommand)]
    Claims(ClaimsCmd),
    /// Deductible and out-of-pocket progress per member and plan year
    /// (accumulator-list/v1).
    Accumulators(AccumulatorsArgs),
    /// The EOB statement PDFs (documents/v1).
    #[command(subcommand)]
    Documents(DocumentsCmd),
    /// Check the import folder: how many statements, claims and balances were
    /// read and what could not be. Counts only, never a value.
    #[command(subcommand)]
    Import(ImportCmd),
    /// Update to the latest release from GitHub.
    SelfUpdate(SelfUpdateArgs),
    /// Print a shell completion script.
    Completions { shell: Shell },
    /// Machine-readable capability discovery (cli-info/v1).
    Info,
}

#[derive(Subcommand, Debug)]
pub enum AuthCmd {
    /// Sign in to the member site and keep the session in the OS keychain
    /// (auth-login/v1).
    ///
    /// --browser opens a Chrome window on member.uhc.com, where you sign in;
    /// --stdin reads the Cookie header of a member.uhc.com request, copied
    /// from a signed-in browser's DevTools. One member-site read checks the
    /// session before it is stored, unless --no-verify.
    Login(LoginCmd),
    /// Whether a member-site session is stored and when it was captured
    /// (auth-status/v1). Sends no request.
    Status,
    /// Remove the stored session, then the browser profile a --browser
    /// sign-in keeps (auth-logout/v1).
    Logout,
}

// The family's login flags with `--browser` beside them (DESIGN.md §2.2).
// A plain comment: a doc comment here would replace `auth login`'s about.
#[derive(Args, Debug)]
pub struct LoginCmd {
    #[command(flatten)]
    pub login: LoginArgs,
    #[command(flatten)]
    pub browser: BrowserArgs,
    /// With --stdin or --from-env: the User-Agent of the browser the Cookie
    /// header was copied from. The site's edge refuses requests without a
    /// browser's, so the check and every later read send it.
    #[arg(long, value_name = "UA")]
    pub user_agent: Option<String>,
}

#[derive(Subcommand, Debug)]
pub enum ConfigCmd {
    /// Print the config file path.
    Path,
    /// Show the effective configuration.
    Show,
    /// Set a key: `import_dir <DIR>`.
    Set { key: String, value: String },
    /// Remove a key.
    Unset { key: String },
}

#[derive(Subcommand, Debug)]
pub enum PatientsCmd {
    /// List the members (patient-list/v1).
    #[command(visible_alias = "ls")]
    List,
}

#[derive(Args, Debug, Clone)]
pub struct PatientArg {
    /// A member: its id from `uhc patients list`, or its first name.
    #[arg(long, value_name = "ID")]
    pub patient: String,
}

#[derive(Subcommand, Debug)]
pub enum RecordsCmd {
    /// One member's Coverage, ExplanationOfBenefit and EOB documents as
    /// records-stream/v1.
    Export(ExportArgs),
}

#[derive(Args, Debug)]
pub struct ExportArgs {
    #[command(flatten)]
    pub who: PatientArg,
    /// Only what changed since this day (YYYY-MM-DD): claims whose latest
    /// statement is dated on or after it, and the statements dated on or
    /// after it.
    #[arg(long, value_name = "YYYY-MM-DD")]
    pub since: Option<String>,
    /// Write the stream to stdout (`| medrec import --from uhc`). The stream
    /// is the output, so --json with it is a usage error.
    #[arg(long, conflicts_with = "output")]
    pub ndjson: bool,
    /// Write the stream to DIR/records.ndjson instead (synthetic statements
    /// only: real records go straight into medrec).
    #[arg(short, long, value_name = "DIR")]
    pub output: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
pub enum ClaimsCmd {
    /// Claims, newest service first (claim-list/v1).
    #[command(visible_alias = "ls")]
    List(ClaimsListArgs),
    /// One claim with its lines (claim/v1).
    Get {
        /// The claim's id or UHC claim number.
        claim: String,
    },
}

#[derive(Args, Debug)]
pub struct ClaimsListArgs {
    /// Only this member's claims (id or first name).
    #[arg(long, value_name = "ID")]
    pub patient: Option<String>,
    #[command(flatten)]
    pub range: RangeArgs,
}

#[derive(Args, Debug)]
pub struct AccumulatorsArgs {
    /// Only this member (id or first name); the family's rows stay.
    #[arg(long, value_name = "ID")]
    pub patient: Option<String>,
    /// Only this plan year (default: every year the statements cover).
    #[arg(long, value_name = "YYYY")]
    pub year: Option<i32>,
}

#[derive(Subcommand, Debug)]
pub enum DocumentsCmd {
    /// The statements, newest first (document-list/v1).
    #[command(visible_alias = "ls")]
    List(RangeArgs),
    /// Copy one statement, or `--all` of them, out of the import folder
    /// (document-download/v1, document-download-batch/v1).
    #[command(visible_alias = "get")]
    Download(DownloadArgs),
}

#[derive(Args, Debug)]
pub struct DownloadArgs {
    /// A document id from `uhc documents list`, all of it or a prefix, or a
    /// prefix of the statement id `uhc claims list` shows (6 characters or
    /// more).
    #[arg(required_unless_present = "all", conflicts_with = "all")]
    pub id: Option<String>,
    /// Every document. A statement carrying two members' claims is two rows
    /// naming one file, written once. Files already saved in the folder
    /// (same bytes) are skipped, listed in skipped[] with code `exists`,
    /// unless --overwrite.
    #[arg(long)]
    pub all: bool,
    /// The file to write (`-` for stdout), or with `--all` the folder.
    #[arg(short, long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    /// Replace a file that is already there. Without it, one document onto
    /// an existing file is a usage error (exit 2).
    #[arg(long)]
    pub overwrite: bool,
}

#[derive(Subcommand, Debug)]
pub enum ImportCmd {
    /// Read every PDF in the folder and report counts and issue codes
    /// (import-check/v1). Safe to share: it prints no names, amounts or ids.
    Check,
}
