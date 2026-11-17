# AGENTS.md — uhc-cli

`uhc` is a read-only CLI for a UnitedHealthcare member's coverage, claims,
EOBs and deductible and out-of-pocket progress. It reads the member site
(member.uhc.com, signed in per profile) or the EOB statement PDFs the
member downloads from it, and emits FHIR for the household's
medical-records system (`records-stream/v1`, `medrec import --from uhc`).
It conforms to piekstra-cli/1 and the `health-records/v1` and
`documents/v1` profiles, on the `pk-cli-*` crates from
[cli-common](https://github.com/piekstra/cli-common) and `pk-med-records`
from [medical-common](https://github.com/piekstra/medical-common), which
owns the records stream, the export file and the profile's DTOs. Every
`pk-cli-*` stays on the cli-common tag medical-common pins.

## Layout

| File | Owns |
|---|---|
| `src/cli.rs` | the clap tree |
| `src/app.rs` | the commands, argument checks, output |
| `src/auth.rs` | `auth login`, `status`, `logout` of one `--profile`, `auth reset`: the browser-profile seams |
| `src/session.rs` | `SessionStore`: get, put and delete a profile's session in the vault; the only code that knows where it lives |
| `src/source.rs` | which source a command reads (`--files` or `--profile`), and as which configured profile (`resolve_profile`) |
| `src/site.rs` | the member site's sign-in: the browser capture, the session check (`SESSION_CHECK_URL`, `judge`) |
| `src/client.rs` | the member-site reads: the queries (`src/site/queries/`), pacing, the cookie jar, `judge` (answers to data or exit codes), and `api_request` / `read_only_graphql`, which check an `api` request is a read before it is sent |
| `src/portal.rs` | the site's answers into the model, with the files source's ids; `Site`, which writes the session back |
| `src/site_cmd.rs` | the commands on the site (`Env`: where sessions are kept and which site is read) |
| `src/files.rs` | the import folder: which PDFs, `pdftotext` bounded in time and size |
| `src/eob.rs` | the statement parser (UHC's STD-EOB layout) and its issue codes |
| `src/cents.rs` | statement amounts as integer cents |
| `src/model.rs` | statements combined: members, claims (latest version wins), balances; the site's extra fields (`Origin`, `ClaimKind`, `SiteMember`, `SiteDocument`) |
| `src/fhir.rs` | the CARIN-shaped FHIR mapping (`docs/fhir.md`) |
| `src/synth.rs` | a minimal text PDF writer, for the synthetic statements only |
| `docs/api.md` | how UHC data can be reached, with UHC's own wording, and the traps |

## Medical data rules

These come from the household's plan and are not negotiable.

- The repository carries no personal data, in code, fixtures, docs, commits,
  pull requests or issues. Fixtures are synthetic (`tests/fixtures/README.md`);
  `tests/no_personal_data.rs` scans every tracked file and checks every
  identity field in the fixtures against its dummy shape.
- Records and their contents are never sent to an outside service: no
  health-data aggregator, cloud OCR or other company's API. A Claude session
  is not one of these (the household's decision, 2026-11-17). A session may
  read, print and summarize the household's real statements, records and
  names when the work needs it: debugging a real export, mapping the member
  site with real answers, checking a pull, answering the owner. Real data
  stays out of tracked files, commits, pull requests and issues, and out of
  messages to other sessions unless they need it. Tests and fixtures stay
  synthetic (`make synth`), and a raw capture is deleted from `/tmp` once it
  has served.
- `records export -o DIR` refuses statements without the synthetic marker;
  real records go straight into medrec through a pipe.
- No FHIR, SMART or PDF libraries, no aggregators, no cloud OCR or AI.
  The household's own crates (cli-common, medical-common) are not third
  party; the FHIR here is mapped by hand in `src/fhir.rs`.
  `pdftotext` is a local subprocess; general-purpose crates are fine.
- Read-only. Nothing writes to UHC. A write would need SPEC §1.3's
  confirmation gate and a change to this paragraph in the same commit.

## Statements

- A PDF counts as a statement only when it says "Explanation of Benefits",
  names UnitedHealthcare, and has a statement's structure (`eob::is_uhc_eob`).
- Every problem the parser meets is an `Issue` with a code, never a panic and
  never a silent drop. `import check` counts them, the stream's trailer lists
  them as `statement_issues`, and a folder of statements none of which reads
  is exit 5 ("layout may have changed").
- The same claim on several statements is the latest statement's version.
- The same PDF saved twice is one statement (SHA-256).

## Sources

A command that reads records reads the source it is given, never a
default (`source::source`): `--files <DIR>` (`UHC_FILES`, medical-common
DESIGN.md §1.1; older names `--dir` and `UHC_IMPORT_DIR`, read by
`source::old_dir_variable`) or `--profile <NAME>` (`UHC_PROFILE`); neither is exit 2,
and `config set import_dir` is `import check`'s folder only. A profile is
cli-common's (`pk_cli_config::profile`, DESIGN.md §2.5): configured in
uhc's config (`profiles add`), resolved by `profile::resolve` (missing,
malformed or unknown is exit 2 listing the configured names; a lone
profile is never picked). A session is the vault entry `<profile>/session`
(`pk_cli_auth::vault::Vault::for_binary("uhc")`: one keychain item,
`vault-key` in `piekstra.uhc`, and an encrypted file per entry under
`~/.config/uhc/vault/`), and a `--browser` sign-in runs in the profile's own
kept Chrome profile (`Launch::profile_name`). `profiles remove` removes the
session and that browser profile before the profile; `auth reset` is
`Vault::clear`. The vault's remedies are replaced with uhc's commands.

The member-site sign-in is in place (`auth login --browser` through
`pk-cli-browser`, or `--stdin`), with a `UHC_KEYCHAIN_SERVICE` test-only
override, and every `make` target signs. Its session check is one GraphQL
read (`site::SESSION_CHECK_URL`, judged by `site::judge`).

Every command that reads records reads the site with `--profile`
(`site_cmd`), producing the files source's schemas and ids
(`docs/fhir.md` § Identity): a claim or member is one record to medrec
whichever source wrote it. The reads are four GraphQL queries kept as
files (`src/site/queries/`, `include_str!`) and the document `GET`; the
variables are dates only. `client::Client` sends one request at a time, at
least `client::PACE` apart, follows no redirect, and maps answers to exit
codes: a 401 or a redirect is 3 (`auth login --browser --profile <NAME>
--overwrite`), the edge's 403 page, 429, 5xx and GraphQL errors without
data are 5. A `Set-Cookie` updates the jar, written back once the command
has succeeded, unless a newer sign-in replaced the stored session
meanwhile (`portal::Site::finish` compares the stored `Cookie` header
first). A failed write-back is a warning on stderr, not an error: the
output is complete and the stored session still works. Only `query` operations: a mutation would need SPEC §1.3's
gate and a change to the read-only rule. The household chose the
member site (uhc signs in as the owner in a browser and
reads the site's own data, accepting the risk under its Terms of Use;
`docs/api.md` § Route). Map it by driving the signed-in site directly, with
the browser-explore loop or the session's own requests, and read real
answers as the work needs. Be gentle with it: one request at a time, no bulk
crawling. `examples/map-site.rs` prints an endpoint list and answer shapes
for `docs/api.md`.

## Tests are offline, always

`cargo test` reads no real keychain item or browser profile, stores no
session, contacts nothing beyond loopback, and reads no real statement.
Every test that runs a credential-reading command sets
`UHC_KEYCHAIN_SERVICE=piekstra.uhc.hermetic-tests`, a service that holds
nothing, with a temp `XDG_CONFIG_HOME`: under that variable the vault is
`$XDG_CONFIG_HOME/uhc/vault`, never the owner's, and uhc leaves the kept
browser profiles alone. No binary test stores a session, so none writes or
reads a key (the vault reads its key only when an entry exists). Unit and
in-process tests use `Vault::new(MemoryKeys::new(), dir)`; the profile
seams are filled with `named_profile_exists_in` and
`forget_named_profile_in` under a temp dir.

- `tests/cli_surface.rs`: every `--help`, exit codes, JSON errors, argument
  checks before a source is read, source and profile selection, `NO_COLOR`,
  a per-test `XDG_CONFIG_HOME`.
- `tests/fixture_shapes.rs`: the parser over `tests/fixtures/eob/*.txt`.
- `tests/e2e.rs`: the binary over the fixtures written as PDFs at test time
  and read back through the real `pdftotext` (skipped where poppler is not
  installed; CI installs it).
- `tests/site_e2e.rs`: every site command in-process from a command line
  through `--profile`, against a loopback server answering with
  `tests/fixtures/site/`, the session in an in-memory vault: the requests'
  headers and variables, the output documents and stream, the identity
  against the statement fixture, write-back, and the refusals.
- `tests/no_personal_data.rs`.

`make verify` = fmt-check, clippy, test, smoke.
