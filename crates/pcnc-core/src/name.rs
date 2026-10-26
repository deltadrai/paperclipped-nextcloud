//! Validation of worker names. A name is both the Nextcloud user id and the local part of the
//! worker's address, so it must be valid for both.

use crate::error::EngineError;

/// Local parts that must never be handed to a worker.
pub const DEFAULT_RESERVED: &[&str] = &[
    "abuse",
    "admin",
    "administrator",
    "hostmaster",
    "info",
    "mailer-daemon",
    "noc",
    "noreply",
    "no-reply",
    "postmaster",
    "root",
    "security",
    "support",
    "webmaster",
];

/// Checks `name` against the worker-name rules: 2–48 characters, lowercase ASCII letters,
/// digits, `.`, `-` and `_`, starting with a letter and not ending with a separator.
pub fn validate(name: &str, reserved: &[String]) -> Result<(), EngineError> {
    let bad = |msg: &str| {
        Err(EngineError::bad_request(
            "invalid_name",
            format!("{name:?}: {msg}"),
        ))
    };
    if !(2..=48).contains(&name.len()) {
        return bad("must be 2 to 48 characters");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || ".-_".contains(c))
    {
        return bad("only lowercase letters, digits, '.', '-' and '_' are allowed");
    }
    if !name.starts_with(|c: char| c.is_ascii_lowercase()) {
        return bad("must start with a letter");
    }
    if name.ends_with(['.', '-', '_']) || name.contains("..") {
        return bad("must not end with or repeat a separator");
    }
    if DEFAULT_RESERVED.contains(&name) || reserved.iter().any(|r| r == name) {
        return bad("is reserved");
    }
    Ok(())
}

/// Derives a name suggestion from a free-form agent name ("Ada Lovelace" → "ada.lovelace").
pub fn suggest(display: &str) -> String {
    let mut out = String::new();
    for c in display.trim().to_lowercase().chars() {
        let c = match c {
            'ä' => 'a',
            'ö' => 'o',
            'ü' => 'u',
            'ß' => 's',
            c if c.is_ascii_alphanumeric() => c,
            _ => '.',
        };
        if c == '.' && (out.is_empty() || out.ends_with('.')) {
            continue;
        }
        out.push(c);
    }
    out.trim_end_matches('.').chars().take(48).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_names() {
        for n in ["ada", "ada.lovelace", "scout-2", "r2_d2"] {
            validate(n, &[]).unwrap();
        }
    }

    #[test]
    fn rejects_bad_names() {
        for n in [
            "a",
            "Ada",
            "2pac",
            "ada.",
            "a..b",
            "ada@x",
            "postmaster",
            "ad a",
        ] {
            assert!(validate(n, &[]).is_err(), "{n} should be rejected");
        }
        assert!(validate("contact", &["contact".into()]).is_err());
    }

    #[test]
    fn suggests_names() {
        assert_eq!(suggest("Ada Lovelace"), "ada.lovelace");
        assert_eq!(suggest("  Jürgen   (CEO) "), "jurgen.ceo");
    }
}
