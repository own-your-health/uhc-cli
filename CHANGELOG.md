# Changelog

## v0.2.3

- Error text names no member, by id or by name (medical-common DESIGN §1.3,
  e4ef786: a member id derived from a nine-digit member id is handled as
  that member id). With several members and no `--patient`, a shared first
  name, or no match, the error says how many and points at `patients list`
  instead of listing ids. A statement read with issues is named in the
  stream by its own id and date, not by its `DocumentReference` id, which
  carries the member's.
- docs/fhir.md says the member id is §1.3's form; AGENTS.md says how a
  member id is handled.

## v0.2.2

- Every exported resource carries `meta.lastUpdated`, by medical-common
  v0.6.6's DESIGN §1.4:
  - from statements, a claim and a document carry their own statement's
    date, and the member, plan and payer carry the latest statement's date;
  - from the member site, a claim, the member, plan and payer carry the
    time of the read (the stream's `pulled_at`), and a document carries its
    creation date, so it stays the same resource from one read to the next.

  A date is that day at `00:00:00Z`; a missing or impossible one leaves the
  stamp out. A records store that gets one claim from both sources keeps
  the newer version instead of whichever it imported last, so a statement
  imported after the site no longer replaces the site's procedure codes and
  claim type.
- docs/fhir.md says how the member id meets medical-common's patient id
  rule (DESIGN §1.3) and why its form stays as it is: a new derivation
  would make a store file every member again.
- medical-common v0.6.6. Its store library treats a site re-read that
  changed only `meta.lastUpdated` as the same version, refreshed in place,
  so a store files no new copy. The member, plan and payer built from
  statements come from the whole folder, never from `--since`'s range, as
  its DESIGN §1.4 requires; a test now holds that.

## v0.2.1

The statements and the site now agree on who a member is.

- A member id printed with a letter before its digits (statements into
  2025: `A` + nine digits) is the site's nine digits. Each member on
  those statements had a second patient id, so a store filed their older
  claims apart from the site's.
- A missing or unknown `--patient` names the source's own command
  (`uhc --profile <NAME> patients list` or `uhc --files <DIR> patients
  list`); plain `uhc patients list` names no source and is exit 2.
- docs/fhir.md lists the claims the two sources still file differently,
  and why: a newborn's claim before naming, pharmacy reversal rows, a claim
  reprocessed under a new number.

## v0.2.0

The member site as a source. `uhc auth login --browser --profile <NAME>`
signs in to myuhc.com in a Chrome window as the member, and every record
command run with `--profile` reads the site's own data: members, claims with
their service lines, plan balances and documents. The statement files stay
a source, named with `--files`. Each login is its own profile, and the
sessions live in an encrypted vault.

- The member site is a source: every command that reads records reads
  member.uhc.com with `--profile <NAME>` (`patients list`, `claims list`
  and `get`, `accumulators`, `documents list` and `download`,
  `records export --ndjson`), with the files source's schemas and ids, so
  a member or claim is one record to medrec from either. The site adds the
  claim type (professional, institutional, pharmacy, oral), CPT, HCPCS and
  CDT codes, revenue codes, tooth numbers, remark codes as process notes,
  the network status, members' birth dates and plan periods, and every
  document addressed to the member (EOBs, health statements, letters). A
  reprocessed claim is its latest version and a reversed pharmacy claim is
  cancelled; a claim still in process is skipped (`in_process`). From the
  site, `--since D` reads the details of the claims processed since D only.
  Four GraphQL queries and the document download, one request a second,
  queries only; an expired session is exit 3 naming the sign-in, the
  edge's refusal, 429, 5xx and GraphQL errors are exit 5. `records export
  -o` is refused for the site.
- Statements: a pharmacy claim's `ExplanationOfBenefit` is `type`
  `pharmacy`; `claims list` items gain `type`, and a claim's lines their
  site codes. `document` in `claims list` and `accumulators` is left out
  when there is none (the site's balances).
- A command that reads records names its source: `--files <DIR>` or
  `UHC_FILES`, the family's spelling (`--dir` and `UHC_IMPORT_DIR` stay as
  older names; the two variables naming different folders is exit 2) for
  downloaded statements, or
  `--profile <NAME>` (`UHC_PROFILE`) for a member-site sign-in. Without one
  it is exit 2; before, no flag read `config set import_dir`, which is now
  `import check`'s folder only. `import check`, `info`, `config`,
  `self-update` and `completions` take no `--profile`.
- Profiles (cli-common's `pk_cli_config::profile`): `profiles add <NAME>`,
  `profiles list` (`profile-list/v1`) and `profiles remove <NAME>`, kept in
  uhc's config. A command that signs in needs a configured one: missing,
  malformed or unknown is exit 2 listing the configured names, and a lone
  profile is never picked. `profiles remove` removes the profile's session
  and its kept browser profile first; it asks, and without a terminal needs
  `--force` (exit 6).
- The member-site session is per profile: `auth login`, `status` and
  `logout` take `--profile <NAME>`, and `--browser` signs in through that
  profile's own kept Chrome profile, which `auth logout --profile <NAME>`
  removes. Sessions are entries `<profile>/session` in uhc's vault
  (`pk_cli_auth::vault`): encrypted owner-only files under
  `~/.config/uhc/vault/`, all under one keychain item (`vault-key` in
  `piekstra.uhc`), so a new build asks the keychain once for every profile.
  `auth reset` removes every session and the key (asks; `--force` without a
  terminal, exit 6 without).
- `info` lists `records-files` among its capabilities: `records export
  --files <DIR>` streams the statement files with no sign-in (`method:
  "portal"`, no `patient.session`), and a records store passes `--files`
  only to a provider listing it.
- `info` lists `login-profiles` among its capabilities, where a records
  store looks for it (medical-common DESIGN.md §1.1), and `auth.required`
  is true, in `auth status` too: without `--files`, a record command signs
  in, so medrec passes `--profile`. Its profiles stay `health-records/v1`
  and `documents/v1`.
- `uhc --profile <NAME> api GET <PATH>` and `api POST <PATH> --data
  '{"query": ...}'`: one read of the member site, printed as the JSON it
  answers. Read-only: another method or host, a GraphQL document with any
  operation but a query, a persisted query or a sign-out path is exit 2
  before anything is sent.
- A statement's claim block holding several claims, each opening with its
  own "Provider:" line, is read as that many claims (UHC's layout). Before,
  a second claim in a block was folded into the first. A continued page
  heading over the processing codes alone opens no claim.
- A pharmacy benefit's claim on a medical statement ("Provider: Pharmacy")
  is `rx` + the first 17 digits of its pharmacy claim number, which the
  statement prints as the patient account number (`docs/fhir.md`).

- From `--files`, `patients list` lists no `label` (a label belongs to a
  profile, medical-common DESIGN.md §1.1), and `--patient` takes a
  member's id only; a first name is exit 4. `records export` needs no
  `--patient` when there is one member, from either source; with several
  it is exit 2 naming their ids. A member has the same `organization` and
  `patient.id` from either source, and a records store merges the two.
- On medical-common v0.6.2 and cli-common v0.33.2 (earlier in this
  release: medical-common v0.2.0 and cli-common v0.31.1). The config file,
  which holds the profile names, is written owner-only (`0600`) by rename.
  `documents download` writes through `pk_cli_core::private_file`: an
  `--overwrite` replaces a file by rename, so a failed write keeps the old
  one, and never leaves a partial file. `records export -o`
  renames the stream into place only once it is written: a complete one
  replaces `records.ndjson`, an incomplete one goes to
  `records.incomplete.ndjson` and the last complete export stays, and a
  failed write leaves nothing behind.
- A folder whose statements could not be read (exit 5) says so, as
  "the statements could not be read: …", no longer as a network error.
- `auth login --browser [--fresh-profile] [--overwrite]` and `--stdin`
  keep a member-site session; `auth status` reports it (`method: "browser-session"`, its
  capture time, `browser_profile`) and `auth logout` removes it, then the
  kept Chrome profile. One GraphQL read checks a session before it is
  stored (401 JSON when signed out); `--no-verify` stores one unchecked.
  `--stdin` and `--from-env` need `--user-agent`, the browser's: the site's
  edge refuses a request without one.

## v0.1.0

The first release. `uhc` reads the UnitedHealthcare Explanation of Benefits
statement PDFs downloaded from myuhc.com into a folder (`docs/api.md` has
why: UHC's Patient Access API does not serve employer plans).

- `records export --patient <id> --ndjson`: `records-stream/v1` with
  `Patient`, `Organization`, `Coverage`, one `ExplanationOfBenefit` per
  claim and each statement as `DocumentReference` + `Binary`
  (`docs/fhir.md`). `--since D` is what changed since D.
- `patients list`, `claims list`, `claims get`, `accumulators`,
  `documents list`, `documents download [--all]`, `import check`.
- `auth status` reports no sign-in needed; `auth login` is a usage error.
- Built on cli-common v0.31.0 and medical-common v0.1.0: the stream,
  `records-export/v1`, `patient-list/v1` (`PatientContext`) and
  `documents/v1` are the `health-records/v1` profile's. A `documents list`
  id is the export's `DocumentReference` id.
- Read-only: nothing writes to UnitedHealthcare, and nothing is sent
  anywhere. Text comes from the local `pdftotext`.
