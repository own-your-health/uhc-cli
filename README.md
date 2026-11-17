# uhc

Unofficial; not affiliated with or endorsed by UnitedHealthcare or Optum.

`uhc` reads a UnitedHealthcare member's coverage, claims, Explanation of
Benefits and deductible and out-of-pocket progress, and hands them to the
household's medical-records system as FHIR (`records-stream/v1`, read by
`medrec import --from uhc`). It is read-only: nothing it does writes to UHC.

It reads the EOB statement PDFs you download from myuhc.com into a folder.
The statement commands never contact UnitedHealthcare. Text comes from the
local `pdftotext` (poppler); nothing is sent anywhere. `docs/api.md` records why: UHC's
Patient Access API serves Medicare, Medicaid, CHIP and marketplace members,
not employer plans, and the member site's terms forbid automated access.

## Install

```sh
brew install poppler            # pdftotext
cargo install --git https://github.com/piekstra/uhc-cli --locked
# or download a release: uhc-<target>.tar.gz, then `uhc self-update` later
```

## Use

```sh
# Download each "Claim EOB" PDF from myuhc.com (Claims & Accounts) into one folder.
uhc config set import_dir ~/Documents/UHC-EOBs
uhc import check                 # counts and issue codes only; safe to share

F=~/Documents/UHC-EOBs
uhc --files $F patients list     # subscriber and dependents
uhc --files $F claims list --since 2026-01-01
uhc --files $F claims get <claim number>
uhc --files $F accumulators --year 2026     # deductible and out-of-pocket progress
uhc --files $F documents list
uhc --files $F documents download <id> -o ~/Desktop
uhc --files $F documents download --all -o ~/Archive/UHC   # re-runs save only new statements

uhc --files $F records export --patient <id> --ndjson | medrec import --from uhc
```

A command that reads records names its source: `--files <DIR>` (alias
`--dir`, or `UHC_IMPORT_DIR`) for downloaded statements, `--profile <NAME>`
(or `UHC_PROFILE`) for a member-site sign-in. Without one it is exit 2;
`config set import_dir` is `import check`'s folder only. A flag wins over
the other source's variable; both flags, or both variables and no flag, are
exit 2. `import check`, `info`, `config`, `self-update` and `completions`
need no sign-in and take no `--profile`.

Every command takes `--json` and prints one schema-tagged document, except
`records export --ndjson`, whose NDJSON stream is the output (`--json` with
it is a usage error, exit 2).

| Command | Output schema |
| --- | --- |
| `patients list` | `patient-list/v1` |
| `records export` | `records-stream/v1` (NDJSON) |
| `claims list` / `claims get` | `claim-list/v1` / `claim/v1` |
| `accumulators` | `accumulator-list/v1` |
| `documents list` / `download` | `document-list/v1` / `document-download/v1`, `document-download-batch/v1` |
| `import check` | `import-check/v1` |
| `auth login` / `status` / `logout` | `auth-login/v1` / `auth-status/v1` / `auth-logout/v1` |
| `info` | `cli-info/v1`, profiles `health-records/v1` and `documents/v1` |

Exit codes: 0 ok, 1 other (pdftotext missing), 2 usage, 3 sign-in not
finished or session refused, 4 not found, 5 the statements could not be
read (layout changed), or the member site's answer could not decide.

`documents download` never replaces a file without `--overwrite`. For one
document an existing file is a usage error (exit 2). With `--all`, a
statement already saved there (same bytes) is listed in `skipped[]` with
code `exists`, so re-running it into an archive folder saves only the new
statements. A different file holding its name leaves the statement saved
beside it as `<id>_<name>`; when that name is taken too, it is listed with
code `name_taken`. `--overwrite` replaces both.

### What the export carries

Per member: `Patient`, the payer `Organization`, `Coverage`, one
`ExplanationOfBenefit` per claim (CARIN Blue Button categories for every
amount, processing codes as notes, the plan balances the statement printed as
`benefitBalance`), and each statement PDF as `DocumentReference` + `Binary`.
`docs/fhir.md` has the mapping.

`-o DIR` writes the stream to `DIR/records.ndjson` instead (mode 0600,
written under a temporary name and renamed into place; a symbolic link
there is refused) and prints `records-export/v1`. A stream that ends
incomplete goes to `DIR/records.incomplete.ndjson` and the last complete
export stays. It is for folders of synthetic statements only; real
records go straight into medrec.

The stream, `patients list` and the `documents/v1` rows are medical-common's
`health-records/v1` shapes (`pk-med-records`). A `documents list` id is the
export's `DocumentReference` id: a statement carrying two members' claims
is listed once for each, both rows naming the one file. `documents download`
also takes a prefix of the statement id that `claims list` shows.

## auth: the member-site session

The files source needs no sign-in. `uhc auth` keeps one member-site
(member.uhc.com) session per profile: a name of 1 to 32 lowercase letters,
digits, `-` or `_`, one per member account that signs in.

```sh
uhc auth login --browser --profile adult1     # a Chrome window on member.uhc.com; sign in there
uhc auth login --browser --fresh-profile --profile adult1   # a scratch browser profile, removed afterwards
pbpaste | uhc auth login --stdin --user-agent "<the browser's>" --profile adult1   # a Cookie header from DevTools
uhc auth status --profile adult1              # stored or not, and when; sends no request
uhc auth logout --profile adult1              # removes the session, then the browser profile
```

Before a session is stored, one member-site read checks it (a one-field
GraphQL query; a 401 is not signed in); `--no-verify` stores it unchecked
(`"verified": false` in `auth-login/v1`). With a session stored for the
profile, `auth login` refuses before Chrome opens unless `--overwrite`. The
site's edge refuses a request without a browser's user agent, so a
`--stdin` session needs `--user-agent`.

| What | Where | Removed by |
| --- | --- | --- |
| The session: the `Cookie` header, the browser's user agent and when it was captured, as one JSON item | the OS keychain, service `piekstra.uhc`, account `session/<profile>` | `uhc auth logout --profile <NAME>`, first |
| The Chrome profile `--browser` signs in through, one per profile, kept so the site remembers this device: the site's cookies (encrypted by Chrome), its site storage and the window's history | `~/Library/Application Support/piekstra.uhc/browser-profile-<profile>`, owner-only (`0700`); under `$XDG_DATA_HOME` on Linux | `uhc auth logout --profile <NAME>`, after the session |

`--fresh-profile` signs in without the kept profile and leaves it as it is.
The session never goes on a command line, into a log or into a file of
uhc's.

## Try it on synthetic data

```sh
make synth                       # target/synth/*.pdf, from tests/fixtures/eob/
uhc --files target/synth import check
uhc --files target/synth claims list
```

## Development

`make verify` runs fmt-check, clippy, the tests and the smoke test. Tests
are offline and use synthetic statements only (`tests/fixtures/README.md`).
See `AGENTS.md` and `CONTRIBUTING.md`.

## License

MIT or Apache-2.0, at your option.
