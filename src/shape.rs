//! What a member-site answer looks like, never what it says: the keys and
//! types of a JSON document, and an endpoint path with every segment that
//! could carry a value replaced by `{id}`.
//!
//! This is what mapping the site's API may print (docs/api.md): a session
//! that maps it must never see a value, so a key or a path segment is
//! printed only when it reads as a name the API gives a thing. A member
//! number, a claim number, an id, a person's name (`Alex`, `ALEX`) and
//! anything with a space in it come out as `{id}` or `<key>`.

use std::collections::{BTreeMap, BTreeSet};

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

/// Whether a key or a path segment is printed as it is.
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

/// An endpoint path with the query cut and every segment that is not a word
/// replaced by `{id}`.
pub fn redact_path(path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or("");
    path.split('/')
        .map(|seg| if seg.is_empty() || is_word(seg) { seg } else { "{id}" })
        .collect::<Vec<_>>()
        .join("/")
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

fn walk(v: &Value, path: String, out: &mut BTreeMap<String, Seen>) {
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
                walk(item, format!("{path}[]"), out);
            }
            return;
        }
        Value::Object(map) => {
            let seen = out.entry(path.clone()).or_default();
            seen.kinds.insert("object");
            for (k, item) in map {
                let key = if is_word(k) { k.as_str() } else { "<key>" };
                walk(item, format!("{path}.{key}"), out);
            }
            return;
        }
    };
    out.entry(path).or_default().kinds.insert(kind);
}

/// One line per path in the document: its types, and for an array how many
/// items it held (the least and most, where the path repeats).
pub fn shape_lines(v: &Value) -> Vec<String> {
    let mut seen = BTreeMap::new();
    walk(v, String::new(), &mut seen);
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

    #[test]
    fn a_path_keeps_its_words_and_loses_its_ids() {
        assert_eq!(
            redact_path("/api/v2/members/SYNM000001/claims/SYNC00000001?from=2026-01-01"),
            "/api/v2/members/{id}/claims/{id}"
        );
        assert_eq!(redact_path("/myuhc/claims"), "/myuhc/claims");
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
        let lines = shape_lines(&doc);
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
