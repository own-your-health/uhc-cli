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
uhc patients list                # subscriber and dependents
uhc claims list --since 2026-01-01
uhc claims get <claim number>
uhc accumulators --year 2026     # deductible and out-of-pocket progress
uhc documents list
uhc documents download <id> -o ~/Desktop
uhc documents download --all -o ~/Archive/UHC   # re-runs save only new statements

uhc records export --patient <id> --ndjson | medrec import --from uhc
```

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

The statement commands need no sign-in. `uhc auth` keeps a session for the
member site, member.uhc.com, whose API is being mapped (`docs/api.md`); no
command reads it yet.

```sh
uhc auth login --browser                  # a Chrome window on member.uhc.com; sign in there
uhc auth login --browser --fresh-profile  # one sign-in in a scratch profile, removed afterwards
pbpaste | uhc auth login --stdin          # the Cookie header of a member.uhc.com request, from DevTools
uhc auth status                           # stored or not, and when; sends no request
uhc auth logout                           # removes the session, then the browser profile
```

Before a session is stored, one member-site read checks it. That check is
not mapped yet, so for now `auth login` refuses (exit 1) before Chrome opens
unless `--no-verify` is given, which stores the session unchecked
(`"verified": false` in `auth-login/v1`). With a session stored, `auth login`
refuses before Chrome opens unless `--overwrite`.

| What | Where | Removed by |
| --- | --- | --- |
| The session: the `Cookie` header, the browser's user agent and when it was captured, as one JSON item | the OS keychain, service `piekstra.uhc`, account `session` | `uhc auth logout`, first |
| The Chrome profile `--browser` signs in through, kept so the site remembers this device: the site's cookies (encrypted by Chrome), its site storage and the window's history | `~/Library/Application Support/piekstra.uhc/browser-profile`, owner-only (`0700`); under `$XDG_DATA_HOME` on Linux | `uhc auth logout`, after the keychain item |

`--fresh-profile` signs in without the kept profile and leaves it as it is.
The session never goes on a command line, into a log or into a file of
uhc's.

## Try it on synthetic data

```sh
make synth                       # target/synth/*.pdf, from tests/fixtures/eob/
uhc --dir target/synth import check
```

## Development

`make verify` runs fmt-check, clippy, the tests and the smoke test. Tests
are offline and use synthetic statements only (`tests/fixtures/README.md`).
See `AGENTS.md` and `CONTRIBUTING.md`.

## License

MIT or Apache-2.0, at your option.
