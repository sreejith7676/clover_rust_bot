//! Small, pure text helpers shared by the handlers and the GitHub client.

use reqwest::Url;
use teloxide::utils::html::escape;

/// Codenames are interpolated into URL paths, so only allow a conservative charset.
pub fn valid_codename(s: &str) -> bool {
    (1..=32).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn capitalize(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Escape for use inside a double-quoted HTML attribute.
pub fn escape_attr(s: &str) -> String {
    escape(s).replace('"', "&quot;")
}

/// Returns the normalized URL only if it is a well-formed http(s) link.
pub fn safe_link(url: &str) -> Option<String> {
    Url::parse(url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"))
        .map(|u| u.to_string())
}

/// Append `piece` only if the result stays within `limit` bytes.
/// Pieces are always appended whole, so HTML tags are never split.
pub fn append_limited(out: &mut String, piece: &str, limit: usize) -> bool {
    if out.len() + piece.len() > limit {
        false
    } else {
        out.push_str(piece);
        true
    }
}

/// Keep the newest lines of `text` that fit in `budget` bytes *after* HTML escaping.
/// Returns `(escaped_text, was_truncated)`.
pub fn tail_lines_within(text: &str, budget: usize) -> (String, bool) {
    let mut kept: Vec<String> = Vec::new();
    let mut used = 0usize;
    let mut truncated = false;

    for line in text.lines().rev() {
        let esc = escape(line);
        let cost = esc.len() + 1;
        if used + cost > budget {
            truncated = true;
            if kept.is_empty() {
                kept.push(escape_tail(line, budget));
            }
            break;
        }
        used += cost;
        kept.push(esc);
    }

    kept.reverse();
    (kept.join("\n"), truncated)
}

/// Escaped tail of a single over-long line, cut on character boundaries.
fn escape_tail(line: &str, budget: usize) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0usize;
    for c in line.chars().rev() {
        let mut buf = [0u8; 4];
        let esc = escape(c.encode_utf8(&mut buf));
        if used + esc.len() > budget {
            break;
        }
        used += esc.len();
        parts.push(esc);
    }
    parts.reverse();
    parts.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codename_validation() {
        assert!(valid_codename("peridot"));
        assert!(valid_codename("Mi-11_Lite"));
        assert!(!valid_codename(""));
        assert!(!valid_codename("../../etc/passwd"));
        assert!(!valid_codename("a b"));
        assert!(!valid_codename("x?y#z"));
        assert!(!valid_codename(&"a".repeat(33)));
    }

    #[test]
    fn capitalize_works() {
        assert_eq!(capitalize("peridot"), "Peridot");
        assert_eq!(capitalize("PERIDOT"), "Peridot");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn links_are_filtered_and_escaped() {
        assert!(safe_link("https://github.com/x").is_some());
        assert!(safe_link("javascript:alert(1)").is_none());
        assert!(safe_link("not a url").is_none());
        assert_eq!(escape_attr("a\"b&c"), "a&quot;b&amp;c");
    }

    #[test]
    fn append_limited_never_splits_pieces() {
        let mut s = String::new();
        assert!(append_limited(&mut s, "<b>abc</b>", 12));
        assert!(!append_limited(&mut s, "<b>def</b>", 12));
        assert_eq!(s, "<b>abc</b>");
    }

    #[test]
    fn tail_lines_keep_newest_and_respect_budget() {
        let text = "one\ntwo <x>\nthree";
        let (out, truncated) = tail_lines_within(text, 1000);
        assert_eq!(out, "one\ntwo &lt;x&gt;\nthree");
        assert!(!truncated);

        let (out, truncated) = tail_lines_within(text, 8);
        assert_eq!(out, "three");
        assert!(truncated);
    }

    #[test]
    fn tail_of_overlong_multibyte_line_does_not_panic() {
        // 4-byte emoji repeated: any byte-index slicing would panic here.
        let line = "😀".repeat(2000);
        let (out, truncated) = tail_lines_within(&line, 3700);
        assert!(truncated);
        assert!(out.len() <= 3700);
        assert!(out.chars().all(|c| c == '😀'));
    }
}
