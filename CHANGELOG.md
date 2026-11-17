# Changelog

## Unreleased

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
- A command that reads records names its source: `--files <DIR>` (new
  alias of `--dir`; `UHC_IMPORT_DIR`) for downloaded statements, or
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
- `info` lists the `login-profiles` capability beside `health-records/v1`
  and `documents/v1`.
- A statement's claim block holding several claims, each opening with its
  own "Provider:" line, is read as that many claims (UHC's layout). Before,
  a second claim in a block was folded into the first. A continued page
  heading over the processing codes alone opens no claim.
- A pharmacy benefit's claim on a medical statement ("Provider: Pharmacy")
  is `rx` + the first 17 digits of its pharmacy claim number, which the
  statement prints as the patient account number (`docs/fhir.md`).

- On medical-common v0.6.1 and cli-common v0.33.1 (earlier in this
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
