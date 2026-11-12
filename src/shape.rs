//! What a member-site answer looks like, never what it says: the keys and
//! types of a JSON document, and an endpoint path with every segment that
//! could carry a value replaced by `{id}`.
//!
//! This is what mapping the site's API may print (docs/api.md): a session
//! that maps it must never see a value, so a key or a path segment is
//! printed only when it reads as a name the API gives a thing. A member
//! number, a claim number, an id, a capitalized name (`Alex`, `ALEX`),
//! anything with a space in it, a slug of three or more parts
//! (`jane-doe-md`) and anything holding one of the household's own names
//! (the [`Redactor`]'s deny words: `alex`, `alexExample`) come out as `{id}`
//! or `<key>`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use pk_cli_core::CliError;
use serde_json::Value;

/// Lines printed for one document at most.
pub const MAX_LINES: usize = 400;

/// Single capitalized words an API names fields with. Any other such word
/// could be a person's name, so it is printed as `<key>`.
const FIELD_WORDS: &[&str] = &[
    "Amount",
    "Amounts",
    "Balance",
    "Balances",
    "Benefit",
    "Benefits",
    "Claim",
    "Claims",
    "Code",
    "Codes",
    "Count",
    "Coverage",
    "Data",
    "Date",
    "Dates",
    "Description",
    "Details",
    "Document",
    "Documents",
    "Error",
    "Errors",
    "Id",
    "Items",
    "Limit",
    "Limits",
    "Member",
    "Members",
    "Message",
    "Name",
    "Network",
    "Plan",
    "Plans",
    "Provider",
    "Providers",
    "Result",
    "Results",
    "Status",
    "Summary",
    "Total",
    "Totals",
    "Type",
    "Types",
    "Value",
    "Year",
];

/// The household's own names, which no printed key or segment may hold.
///
/// Read from a file the owner writes outside any agent session
/// ([`Redactor::default_path`], one word per line, owner-only), and never
/// printed: errors name the file, not what is in it.
#[derive(Debug, Default)]
pub struct Redactor {
    words: Vec<String>,
}

impl Redactor {
    pub fn new<S: AsRef<str>>(words: impl IntoIterator<Item = S>) -> Self {
        let mut words: Vec<String> = words
            .into_iter()
            .map(|w| w.as_ref().trim().to_lowercase())
            .filter(|w| !w.is_empty())
            .collect();
        words.sort();
        words.dedup();
        Redactor { words }
    }

    /// `$XDG_CONFIG_HOME/uhc/redact-words`, else `~/.config/uhc/redact-words`.
    pub fn default_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("uhc").join("redact-words"))
    }

    /// The deny words in `path`. A missing or empty file, or one that others
    /// can read, is refused (exit 2), since mapping without it could print a
    /// name.
    pub fn from_file(path: &Path) -> Result<Redactor, CliError> {
        let shown = path.display();
        let meta = std::fs::symlink_metadata(path).map_err(|_| {
            CliError::Usage(format!("no deny-word file at {shown}"))
                .with_remedy("in a terminal outside any agent session, write the household's first and last names there, one per line, mode 0600")
        })?;
        if !meta.is_file() {
            return Err(CliError::Usage(format!("{shown} is not a plain file")));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err(
                    CliError::Usage(format!("{shown} is readable by others")).with_remedy(format!("chmod 600 {shown}"))
                );
            }
        }
        let text =
            std::fs::read_to_string(path).map_err(|e| CliError::Other(format!("reading {shown}: {}", e.kind())))?;
        let r = Redactor::new(text.lines());
        if r.words.is_empty() {
            return Err(CliError::Usage(format!("{shown} holds no word")));
        }
        Ok(r)
    }

    /// How many deny words there are (a count, never the words).
    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    fn denied(&self, s: &str) -> bool {
        let s = s.to_lowercase();
        self.words.iter().any(|w| s.contains(w.as_str()))
    }

    /// Whether a key is printed as it is.
    pub fn key(&self, s: &str) -> bool {
        is_word(s) && !self.denied(s)
    }

    /// Whether a path segment is printed as it is: a key that is not also a
    /// slug of three or more parts.
    pub fn segment(&self, s: &str) -> bool {
        self.key(s) && s.split(['-', '.']).count() <= 2
    }

    /// An endpoint path with the query cut and every segment that is not
    /// printed as it is replaced by `{id}`.
    pub fn redact_path(&self, path: &str) -> String {
        let path = path.split(['?', '#']).next().unwrap_or("");
        path.split('/')
            .map(|seg| {
                if seg.is_empty() || self.segment(seg) {
                    seg
                } else {
                    "{id}"
                }
            })
            .collect::<Vec<_>>()
            .join("/")
    }

    /// One line per path in the document: its types, and for an array how
    /// many items it held (the least and most, where the path repeats).
    pub fn shape_lines(&self, v: &Value) -> Vec<String> {
        shape_lines_with(v, &|k| self.key(k))
    }
}

/// Whether a key or a path segment reads as a name an API gives a thing,
/// before the deny words: [`Redactor::key`] adds those.
pub fn is_word(s: &str) -> bool {
    if s.is_empty() || s.len() > 40 {
        return false;
    }
    if !s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '$'))
    {
        return false;
    }
    let digits = s.chars().filter(|c| c.is_ascii_digit()).count();
    let mut run = 0;
    for c in s.chars() {
        run = if c.is_ascii_digit() { run + 1 } else { 0 };
        if run >= 3 {
            return false;
        }
    }
    // An id: hex with a digit in it, or letters and digits mixed, 8 or more.
    if s.len() >= 8 && digits >= 2 {
        return false;
    }
    if s.len() >= 8 && digits >= 1 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        return false;
    }
    // No lower-case letter: a code printed in capitals, or a name.
    if !s.chars().any(|c| c.is_ascii_lowercase()) {
        return false;
    }
    // One capitalized word: a field name only if it is a known one.
    let mut chars = s.chars();
    let first = chars.next().unwrap_or('a');
    if first.is_ascii_uppercase() && chars.clone().all(|c| c.is_ascii_lowercase()) {
        return FIELD_WORDS.contains(&s);
    }
    true
}

/// What a string looks like, never what it says.
fn string_kind(s: &str) -> &'static str {
    let b = s.as_bytes();
    let all = |f: fn(&u8) -> bool| !b.is_empty() && b.iter().all(f);
    if s.is_empty() {
        "string(empty)"
    } else if b.len() == 10 && b[4] == b'-' && b[7] == b'-' && all(|c| c.is_ascii_digit() || *c == b'-') {
        "string(date)"
    } else if b.len() > 10 && b[4] == b'-' && b[7] == b'-' && b[10] == b'T' {
        "string(datetime)"
    } else if s.split('/').count() == 3 && all(|c| c.is_ascii_digit() || *c == b'/') {
        "string(m/d/y)"
    } else if all(|c| c.is_ascii_digit()) {
        "string(digits)"
    } else if s == "true" || s == "false" {
        "string(bool)"
    } else if all(|c| c.is_ascii_digit() || matches!(c, b'.' | b',' | b'-' | b'$')) && s.contains('.') {
        "string(amount)"
    } else if s.starts_with("https://") || s.starts_with("http://") {
        "string(url)"
    } else {
        "string"
    }
}

#[derive(Default)]
struct Seen {
    kinds: BTreeSet<&'static str>,
    lengths: Option<(usize, usize)>,
}

fn walk(v: &Value, path: String, out: &mut BTreeMap<String, Seen>, key: &dyn Fn(&str) -> bool) {
    let kind = match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_i64() || n.is_u64() => "int",
        Value::Number(_) => "number",
        Value::String(s) => string_kind(s),
        Value::Array(items) => {
            let seen = out.entry(path.clone()).or_default();
            seen.kinds.insert("array");
            let n = items.len();
            seen.lengths = Some(seen.lengths.map_or((n, n), |(lo, hi)| (lo.min(n), hi.max(n))));
            for item in items {
                walk(item, format!("{path}[]"), out, key);
            }
            return;
        }
        Value::Object(map) => {
            let seen = out.entry(path.clone()).or_default();
            seen.kinds.insert("object");
            for (k, item) in map {
                let shown = if key(k) { k.as_str() } else { "<key>" };
                walk(item, format!("{path}.{shown}"), out, key);
            }
            return;
        }
    };
    out.entry(path).or_default().kinds.insert(kind);
}

fn shape_lines_with(v: &Value, key: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut seen = BTreeMap::new();
    walk(v, String::new(), &mut seen, key);
    let mut lines: Vec<String> = seen
        .into_iter()
        .map(|(path, s)| {
            let kinds: Vec<&str> = s.kinds.into_iter().collect();
            let path = if path.is_empty() { "." } else { path.as_str() };
            match s.lengths {
                Some((lo, hi)) if lo == hi => format!("{path}: {} [{lo}]", kinds.join(" | ")),
                Some((lo, hi)) => format!("{path}: {} [{lo}..{hi}]", kinds.join(" | ")),
                None => format!("{path}: {}", kinds.join(" | ")),
            }
        })
        .collect();
    if lines.len() > MAX_LINES {
        let more = lines.len() - MAX_LINES;
        lines.truncate(MAX_LINES);
        lines.push(format!("... {more} more paths"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_and_numbers_never_print() {
        for value in [
            "Alex",
            "ALEX",
            "EXAMPLE",
            "SYNM000001",
            "123456",
            "a1b2c3d4e5",
            "deadbeef1",
            "Alex Example",
            "x@y.com",
            "4f9c2e10-aaaa-bbbb-cccc-0123456789ab",
        ] {
            assert!(!is_word(value), "{value}");
        }
        for word in [
            "claims",
            "claimNumber",
            "v1",
            "api",
            "myuhc",
            "member_id",
            "Status",
            "ClaimNumber",
            "eob-pdf",
        ] {
            assert!(is_word(word), "{word}");
        }
    }

    fn household() -> Redactor {
        Redactor::new(["Alex", "EXAMPLE", "", "  sam "])
    }

    #[test]
    fn a_path_keeps_its_words_and_loses_its_ids() {
        let r = household();
        assert_eq!(
            r.redact_path("/api/v2/members/SYNM000001/claims/SYNC00000001?from=2026-01-01"),
            "/api/v2/members/{id}/claims/{id}"
        );
        assert_eq!(r.redact_path("/myuhc/claims"), "/myuhc/claims");
        assert_eq!(r.redact_path("/api/claims-summary"), "/api/claims-summary");
    }

    #[test]
    fn the_households_names_never_print_in_any_case() {
        let r = household();
        assert_eq!(r.len(), 3);
        for name in ["alex", "alexExample", "example_md", "samsClaims", "ALEX"] {
            assert!(!r.key(name), "{name}");
        }
        assert_eq!(r.redact_path("/members/alex.example/claims"), "/members/{id}/claims");
        // A slug of three or more parts is an id even with no deny word in it.
        assert_eq!(r.redact_path("/providers/jane-doe-md/visits"), "/providers/{id}/visits");
        let lines = r
            .shape_lines(&json!({"alex": {"total": 1}, "alexExample": 2, "count": 3}))
            .join("\n");
        assert!(!lines.to_lowercase().contains("alex"), "{lines}");
        assert!(lines.contains(".<key>.total: int"), "{lines}");
        assert!(lines.contains(".count: int"), "{lines}");
    }

    #[test]
    fn a_deny_file_must_exist_hold_a_word_and_be_the_owners() {
        let dir = std::env::temp_dir().join(format!("uhc-redact-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("redact-words");
        assert_eq!(Redactor::from_file(&path).unwrap_err().exit_code(), 2, "missing");
        std::fs::write(&path, "\n\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(Redactor::from_file(&path).unwrap_err().exit_code(), 2, "empty");
            std::fs::write(&path, "Alex\nExample\n").unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            let e = Redactor::from_file(&path).unwrap_err();
            assert_eq!(e.exit_code(), 2, "readable by others");
            assert!(!e.message().contains("Alex"), "an error never names a word");
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            assert_eq!(Redactor::from_file(&path).unwrap().len(), 2);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_shape_has_keys_and_types_only() {
        let doc = json!({
            "claims": [
                {"claimNumber": "SYNC00000001", "patient": "ALEX EXAMPLE", "billed": 150.0, "date": "2026-03-01"},
                {"claimNumber": "SYNC00000002", "patient": "SAM EXAMPLE", "billed": "77.13", "paid": null}
            ],
            "byMember": {"SYNM000001": {"total": 3}, "Alex": {"total": 1}},
            "count": 2
        });
        let lines = household().shape_lines(&doc);
        let text = lines.join("\n");
        for value in ["SYNC", "ALEX", "SAM", "150", "77.13", "2026", "SYNM", "Alex"] {
            assert!(!text.contains(value), "{value} in {text}");
        }
        assert!(lines.contains(&".claims: array [2]".to_string()), "{text}");
        assert!(
            lines.contains(&".claims[].billed: number | string(amount)".to_string()),
            "{text}"
        );
        assert!(lines.contains(&".claims[].date: string(date)".to_string()), "{text}");
        assert!(lines.contains(&".byMember.<key>.total: int".to_string()), "{text}");
    }
}
