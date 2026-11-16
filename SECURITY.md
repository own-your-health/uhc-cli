# Security

Report a vulnerability privately through GitHub's "Report a vulnerability"
(Security tab), not in an issue.

## What uhc touches

- Reads the PDFs in the configured import folder (one level of subfolders,
  symlinks not followed, 32 MiB per file) and runs the local `pdftotext` on
  each (60-second limit, 20 MB of text). No shell is involved.
- Writes only where asked: `documents download` (owner-only 0600 files,
  never replacing one without `--overwrite`) and `records export -o`
  (synthetic statements only).
- Contacts no server except GitHub, for `self-update`, and member.uhc.com
  for `auth login`: a Chrome window the owner signs in through, and one read
  that checks the session before it is stored (not mapped yet, so none is
  sent today).
- Stores one credential, the member-site session: one keychain item
  (service `piekstra.uhc`, account `session`) holding the `Cookie` header,
  the browser's user agent and the capture time as JSON. It never goes on a
  command line, into a log or into a file of uhc's.
- Keeps the Chrome profile `auth login --browser` signs in through, so the
  site remembers the device: `~/Library/Application Support/piekstra.uhc/browser-profile`
  (under `$XDG_DATA_HOME` on Linux), owner-only (`0700`, a launch is refused
  otherwise). It holds the site's cookies, which Chrome encrypts, and its
  site storage and the window's history, which only the mode protects.
  `--fresh-profile` signs in without it.
- `auth logout` removes the keychain item, then the profile.
- The config file holds the import folder's path.

## What uhc never does

- Writes to UnitedHealthcare.
- Prints names, amounts or ids in `import check`, which exists to be shared.
- Sends a statement or its contents anywhere: the output goes to stdout or a
  file you name.
