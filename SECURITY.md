# Security

Report a vulnerability privately through GitHub's "Report a vulnerability"
(Security tab), not in an issue.

## What uhc touches

- Reads the PDFs in the folder given with `--files` (or `import check`'s
  configured folder; one level of subfolders,
  symlinks not followed, 32 MiB per file) and runs the local `pdftotext` on
  each (60-second limit, 20 MB of text). No shell is involved.
- Writes only where asked: `documents download` (owner-only 0600 files,
  never replacing one without `--overwrite`) and `records export -o`
  (statement files only; the member site streams to stdout).
- Contacts no server except GitHub, for `self-update`, and
  `https://member.uhc.com`: for `auth login`, a Chrome window the owner signs
  in through and one GraphQL read that checks the session before it is
  stored; for a command run with `--profile`, the member's own reads. Every
  request is a GraphQL query or a `GET` (uhc sends no mutation), one at a
  time and at least a second apart, with a 30-second limit and no redirect
  followed. Every URL is built from that fixed origin; a document's PDF is
  asked for by its id and class, never by a link from an answer.
- A cookie the site changes is written back to the profile's session once
  the command has succeeded.
- Stores one credential per profile, its member-site session: the
  `Cookie` header, the browser's user agent and the capture time, as the
  vault entry `<profile>/session`. The vault (`pk_cli_auth::vault`) is
  encrypted owner-only files under `~/.config/uhc/vault/`, with one key in
  the keychain (service `piekstra.uhc`, account `vault-key`). A session never
  goes on a command line or into a log.
- Keeps one Chrome profile per uhc profile for `auth login --browser`, so
  the site remembers the device:
  `~/Library/Application Support/piekstra.uhc/browser-profile-<profile>`
  (under `$XDG_DATA_HOME` on Linux), owner-only (`0700`, a launch is refused
  otherwise). It holds the site's cookies, which Chrome encrypts, and its
  site storage and the window's history, which only the mode protects.
  `--fresh-profile` signs in without it.
- `auth logout --profile <NAME>` and `profiles remove <NAME>` remove that
  profile's session, then its Chrome profile. `auth reset` removes every
  session and the vault key.
- The config file holds the import folder's path and the profile names.

## What uhc never does

- Writes to UnitedHealthcare.
- Prints names, amounts or ids in `import check`, which exists to be shared.
- Sends a statement or its contents anywhere: the output goes to stdout or a
  file you name.
