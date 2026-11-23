# uhc

Unofficial; not affiliated with or endorsed by UnitedHealthcare or Optum.

`uhc` reads a UnitedHealthcare member's coverage, claims, Explanation of
Benefits and deductible and out-of-pocket progress, and hands them to a
personal medical-records store as FHIR (`records-stream/v1`, read by
`medrec import --from uhc`). It is read-only: nothing it does writes to UHC.

It is for anyone with a UnitedHealthcare plan and a myuhc.com sign-in
(HealthSafe ID), including employer plans. It reads every member that
sign-in shows: the subscriber and their dependents. No developer account
or app registration is needed: uhc reads the member site as you, with the
session you sign in to.

It reads one of two sources, named on each command:

- the member site, member.uhc.com (`--profile <NAME>`): you sign in once in
  a Chrome window uhc opens, and uhc reads the site's own answers with that
  session: the claims with their procedure and remark codes, the plan
  balances, and the EOBs, health statements and letters;
- the EOB statement PDFs you download from myuhc.com into a folder
  (`--files <DIR>`): these commands never contact UnitedHealthcare. Text
  comes from the local `pdftotext` (poppler); nothing is sent anywhere.

`docs/api.md` records why there is no API route (UHC's Patient Access API
serves Medicare, Medicaid, CHIP and marketplace members, not employer
plans), the clauses of the site's Terms of Use on automated access, quoted
with where the full text is, and every read uhc makes.

## The site's terms, in plain words

The HealthSafe ID and member-site Terms of Use give you a personal,
non-commercial license to view your information. They say you will not use
software, scripts or "other means" to access, "scrape", "crawl" or
"spider" the site, or harvest data from it, and that a breach ends your
license. uhc is exactly such software, even though it only reads your own
records, one request a second. Using it against the member site is your
decision and your risk: UHC could block the account or end its access. The
`--files` source reads statements you downloaded yourself and never
contacts UHC.

## Quick start

1. Install (below). The member site needs Google Chrome; the files source
   needs `pdftotext` (poppler).
2. Name the account: `uhc profiles add adult1` (any name; one per sign-in).
3. Sign in: `uhc auth login --browser --profile adult1` opens Chrome on
   member.uhc.com. Sign in there as usual; uhc stores the session when the
   site shows you signed in.
4. See who is on the plan: `uhc --profile adult1 patients list` prints each
   member's id. To pass your own name for a member instead, label it once:
   `uhc --profile adult1 patients label --patient <ID> Robin`.
5. Export into medrec, once per member:
   `medrec import --from uhc --from-profile adult1 --patient <ID>`, or
   `uhc --profile adult1 records export --patient <ID> --ndjson | medrec import --from uhc`.
   The first import of a member asks medrec which of its people that is
   (`--person`).

What needs a person, and when:

| Step | When | What you do |
| --- | --- | --- |
| Sign in | once per sitting | the site's own sign-in in uhc's Chrome window: your user id and password, and any code the site sends by text or email. If the site offers a passkey, "Sign in with password" works too |
| Keychain approval (macOS) | the first run after each install or upgrade | click "Always Allow" on one dialog: uhc's vault key, one keychain item for every profile |
| Who is who | a member's first import into medrec | name the medrec person (`--person`) |

Everything else runs unattended: exports, imports and re-imports.

A sign-in does not last long. The site's session ends when Chrome closes,
and unattended it lasted at least 40 minutes idle and under about two and a
half hours in one measurement (`docs/api.md`). Expect to sign in once per
sitting; a session from the morning is gone by the afternoon. A command run
on an expired session is exit 3 and names the sign-in that renews it.

## Install

```sh
brew install poppler            # pdftotext
cargo install --git https://github.com/own-your-health/uhc-cli --locked
# or download a release: uhc-<target>.tar.gz, then `uhc self-update` later
```

## Use

From the member site, one profile per member account that signs in:

```sh
uhc profiles add adult1
uhc auth login --browser --profile adult1       # sign in in the Chrome window it opens
uhc --profile adult1 patients list              # subscriber and dependents
uhc --profile adult1 patients label --patient <id> Robin   # your own name for one; --clear removes it
uhc --profile adult1 claims list --since 2026-01-01
uhc --profile adult1 claims get <claim id>      # with its lines, procedure and remark codes
uhc --profile adult1 accumulators --year 2026   # deductible and out-of-pocket progress, today
uhc --profile adult1 documents list             # EOBs, health statements, letters
uhc --profile adult1 documents download --all -o ~/Archive/UHC

uhc --profile adult1 records export --patient <id> --ndjson | medrec import --from uhc

# One read of the site, printed as the JSON it answers (docs/api.md lists them):
uhc --profile adult1 api POST '/ce-claims-aggregate-api/graphql?q=getSpendingOverview' \
  --data '{"query": "query getSpendingOverview { spendingOverviews { __typename } }"}'
```

`api` is read-only too: it sends a `GET`, or a GraphQL request of queries
only `POST`ed to a `/graphql` path, on `https://member.uhc.com` only. Any
other method, host or operation (a `mutation`, a persisted query by hash)
is exit 2 before a request is sent, and so is a sign-out path.

uhc reads the site one request at a time, a second apart, and only with
queries: nothing it does writes to UHC. A session that has expired is exit
3 naming the sign-in that renews it.

From downloaded statements:

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

A command that reads records names its source: `--files <DIR>` (or
`UHC_FILES`; the older `--dir` and `UHC_IMPORT_DIR` still work) for
downloaded statements, `--profile <NAME>`
(or `UHC_PROFILE`) for a member-site sign-in. Without one it is exit 2;
`config set import_dir` is `import check`'s folder only. A flag wins over
the other source's variable; both flags, or both variables and no flag, are
exit 2. A profile must be configured (`uhc profiles add <NAME>`); there is
no default, and a lone profile is never picked. `import check`, `info`,
`config`, `profiles`, `auth reset`, `self-update` and `completions` need no
sign-in and take no `--profile`. `api` reads the site, so it takes
`--profile` only.

A member is the same patient from either source: organization `uhc`, and
an id derived from the member id and first name that both the site and a
statement carry. medrec files the two streams as one patient and merges
the claims they share (statements print a few claims differently from the
site: a newborn before naming, pharmacy reversals, a claim reprocessed
under a new number; `docs/fhir.md` § Where the sources still differ).
`--patient` takes that id. With `--profile` it also takes the member's
label: your own name for it, which `patients label` sets
(medical-common DESIGN §1). The first name the site shows is the member's
`name`, never a label, so a first name is exit 4 until you give a member
that label. With `--files` there is no label, since a label belongs to a
sign-in's profile. When there is one member, `records export` needs no
`--patient`; with several it is exit 2, saying how many there are and
pointing at `patients list`, which shows their labels, names and ids.
Error text names no member, by id or by name.

`patients label --patient <ID or LABEL> (<WHO> | --clear)` takes
`--profile` and makes no request: it labels a member of that profile that
a read of the site has listed (`patients list`, or any other read). A
label is unique within the profile and is never a member's id; either
collision, an empty label, or both or neither of `<WHO>` and `--clear` is
exit 2, and a member the profile does not have is exit 4. Setting the label
a member already has, or clearing one that is not set, changes nothing.
`--json` prints `patient-list/v1` holding that member. The labels live in
the profile's settings in uhc's config (owner-only), one grant per member,
and `profiles remove` removes them.

Every command takes `--json` and prints one schema-tagged document, except
`records export --ndjson`, whose NDJSON stream is the output (`--json` with
it is a usage error, exit 2). A failure with `--json` prints
`{"error": {"code", "message"}}`, with `reason` (a typed cause, such as
`credential_unreadable`) and `remedy` (the next step, also printed after
the message) when there is one.

| Command | Output schema |
| --- | --- |
| `patients list` | `patient-list/v1` |
| `records export` | `records-stream/v1` (NDJSON) |
| `claims list` / `claims get` | `claim-list/v1` / `claim/v1` |
| `accumulators` | `accumulator-list/v1` |
| `documents list` / `download` | `document-list/v1` / `document-download/v1`, `document-download-batch/v1` |
| `import check` | `import-check/v1` |
| `api` | the site's own JSON answer |
| `auth login` / `status` / `logout` / `reset` | `auth-login/v1` / `auth-status/v1` / `auth-logout/v1` / `auth-reset/v1` |
| `profiles list` / `add` / `remove` | `profile-list/v1` / `profile-add/v1` / `profile-remove/v1` |
| `info` | `cli-info/v1`: profiles `health-records/v1` and `documents/v1`; capabilities `login-profiles`, `records-files` (`records export --files <DIR>`, no sign-in) and `patient-labels` (`patients label`); `auth.required` true (without `--files`, a record command signs in) |

A `claim/v1` line's `procedure` is `{code, display}`. Where the site
appended a letter or digit to a CPT or HCPCS code (`710467`), `code` is the
five-character code, `suffix` the character and `site_code` the site's
whole code; `suffix` and `site_code` appear only on such lines.

Exit codes: 0 ok, 1 other (pdftotext missing), 2 usage, 3 sign-in not
finished or session refused, 4 not found, 5 the statements could not be
read (layout changed), or the member site's answer could not decide, 6 a
destructive command (`auth reset`, `profiles remove`) without a terminal
to ask on and without `--force`.

`documents download` never replaces a file without `--overwrite`. For one
document an existing file is a usage error (exit 2). With `--all`, a
statement already saved there (same bytes) is listed in `skipped[]` with
code `exists`, so re-running it into an archive folder saves only the new
statements. A different file holding its name leaves the statement saved
beside it as `<id>_<name>`; when that name is taken too, it is listed with
code `name_taken`. `--overwrite` replaces both.

### What the export carries

Every resource carries `meta.lastUpdated` (medical-common DESIGN §1.4): a
statement's date, a site document's creation date, or the time the site was
read, so a store keeps the newer of two versions (`docs/fhir.md`). Per member: `Patient`, the payer `Organization`, `Coverage`, one
`ExplanationOfBenefit` per claim (CARIN Blue Button categories for every
amount, processing codes as notes, the plan balances the statement printed as
`benefitBalance`), and each statement PDF as `DocumentReference` + `Binary`.
From the site each claim also carries its claim type (professional,
institutional, pharmacy, oral), each line its CPT, HCPCS or CDT code and
revenue code, the remark codes as notes, the member's birth date and plan
period, and every document addressed to the member. Where the site says
the medical plan counts by calendar year (it gives no dates), each claim
carries the calendar year it was served in as `benefitPeriod`, and
`accumulators --json` gives each row the current year's `period`; their
`plan_year` and `--year` are then that year. When the export cannot read the
plan balances, its claims have no `benefitPeriod` and the trailer's
`skipped[]` says so. A claim and a member
have the same id from either source, so medrec keeps one record of each.
`docs/fhir.md` has the mapping. From the site, `--since D` reads the details
of the claims processed since D only. A claim the plan has not processed
yet is skipped (`in_process` in the trailer): its id changes once it is.

`-o DIR` writes the stream to `DIR/records.ndjson` instead (mode 0600,
written under a temporary name and renamed into place; a symbolic link
there is refused) and prints `records-export/v1`. A stream that ends
incomplete goes to `DIR/records.incomplete.ndjson` and the last complete
export stays. It is for folders of synthetic statements only, so the site
refuses it; real records go straight into medrec.

The stream, `patients list` and the `documents/v1` rows are medical-common's
`health-records/v1` shapes (`pk-med-records`). A `documents list` id is the
export's `DocumentReference` id: a statement carrying two members' claims
is listed once for each, both rows naming the one file. `documents download`
also takes a prefix of the statement id that `claims list` shows, or of the
site's document id. The site's documents are saved as
`<date>_uhc-<kind>_<document id>.pdf`; `--all` leaves a file of that name
alone (`exists`) and reads only the new documents.

## Profiles and the member-site session

The files source needs no sign-in. The member site's is one session per
profile: one per member account that signs in, named by you (1 to 32
lowercase letters, digits, `-` or `_`) and kept in uhc's config.

```sh
uhc profiles add adult1                       # once per account
uhc profiles list
uhc auth login --browser --profile adult1     # a Chrome window on member.uhc.com; sign in there
uhc auth login --browser --fresh-profile --profile adult1   # a scratch browser profile, removed afterwards
pbpaste | uhc auth login --stdin --user-agent "<the browser's>" --profile adult1   # a Cookie header from DevTools
uhc auth status --profile adult1              # stored or not, and when; sends no request
uhc auth logout --profile adult1              # removes the session, then the browser profile
uhc profiles remove adult1                    # the session, the browser profile, then the profile
uhc auth reset                                # every profile's session and the vault's key
```

Before a session is stored, one member-site read checks it (a one-field
GraphQL query; a 401 is not signed in); `--no-verify` stores it unchecked
(`"verified": false` in `auth-login/v1`). With a session stored for the
profile, `auth login` refuses before Chrome opens unless `--overwrite`. The
site's edge refuses a request without a browser's user agent, so a
`--stdin` session needs `--user-agent`.

| What | Where | Removed by |
| --- | --- | --- |
| The session: the `Cookie` header, the browser's user agent and when it was captured, as one JSON entry `<profile>/session` | uhc's vault: one encrypted, owner-only (`0600`) file per entry under `~/.config/uhc/vault/`, sealed with one key, the keychain item `vault-key` in service `piekstra.uhc` | `uhc auth logout --profile <NAME>`, first; `uhc auth reset` removes every entry and the key |
| The Chrome profile `--browser` signs in through, one per profile, kept so the site remembers this device: the site's cookies (encrypted by Chrome), its site storage and the window's history | `~/Library/Application Support/piekstra.uhc/browser-profile-<profile>`, owner-only (`0700`); under `$XDG_DATA_HOME` on Linux | `uhc auth logout --profile <NAME>`, after the session |

`--fresh-profile` signs in without the kept profile and leaves it as it is.
The session never goes on a command line or into a log, and on disk only
encrypted. One keychain item for every profile means a new build of uhc
asks the keychain once, and a run with nothing stored reads no item at all
(cli-common DESIGN.md §2.5). `auth reset` is for a lost or damaged key:
it asks first, and without a terminal needs `--force`.

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
