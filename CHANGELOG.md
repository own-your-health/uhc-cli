# Changelog

## Unreleased

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

- On medical-common v0.6.0 and cli-common v0.33.0 (earlier in this
  release: medical-common v0.2.0 and cli-common v0.31.1). `records export -o`
  renames the stream into place only once it is written: a complete one
  replaces `records.ndjson`, an incomplete one goes to
  `records.incomplete.ndjson` and the last complete export stays, and a
  failed write leaves nothing behind.
- A folder whose statements could not be read (exit 5) says so, as
  "the statements could not be read: …", no longer as a network error.
- `auth login --browser [--fresh-profile] [--overwrite]` and `--stdin`
  keep a member-site session as one keychain item (`piekstra.uhc`,
  `session`); `auth status` reports it (`method: "browser-session"`, its
  capture time, `browser_profile`) and `auth logout` removes it, then the
  kept Chrome profile. One GraphQL read checks a session before it is
  stored (401 JSON when signed out); `--no-verify` stores one unchecked.
  `--user-agent` goes with `--stdin`: the site's edge refuses a request
  without a browser's.

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
