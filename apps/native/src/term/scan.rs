//! **Pure text scanners** for the terminal grid (native-pivot T6).
//!
//! URL detection (decision D5: localhost/link discovery is a *client-plane* scan
//! of the grid, never a server event) and the literal-search pattern builder for
//! find-in-terminal. Everything here is plain `&str -> data`, gpui-free and
//! grid-free, so it unit-tests in WSL under `--no-default-features`;
//! [`crate::term::TermSession`] feeds it viewport text.

/// A URL found in a logical line: `[start, end)` **char** indices plus the URL text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlMatch {
    pub start: usize,
    pub end: usize,
    pub url: String,
}

/// Schemes we recognize, matched case-insensitively. Mirrors the useful subset of
/// alacritty's default hint regex (mailto/magnet/etc. omitted as grid noise).
const SCHEMES: [&str; 4] = ["https://", "http://", "file://", "ftp://"];

/// Characters that terminate a URL. Mirrors alacritty's default hint character
/// class: control chars, whitespace, `<>"{}|^` and backticks stop the scan;
/// parens and quotes are allowed inside but trimmed from the tail (below).
fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(c, '<' | '>' | '"' | '{' | '}' | '|' | '^' | '`' | '\u{27e8}' | '\u{27e9}')
}

/// Case-insensitive scheme match at `chars[i..]`; returns the scheme length.
fn match_scheme(chars: &[char], i: usize) -> Option<usize> {
    'scheme: for s in SCHEMES {
        let s_chars: Vec<char> = s.chars().collect();
        if i + s_chars.len() > chars.len() {
            continue;
        }
        for (k, sc) in s_chars.iter().enumerate() {
            if !chars[i + k].eq_ignore_ascii_case(sc) {
                continue 'scheme;
            }
        }
        return Some(s_chars.len());
    }
    None
}

/// Scan one logical line for URLs. Trailing punctuation (`.,;:!?'"`) is trimmed,
/// and a trailing closer (`)]}`）is trimmed only when unbalanced within the URL -
/// so `https://en.wikipedia.org/wiki/Rust_(language)` keeps its paren while the
/// closing paren of `(see https://example.com)` is dropped.
pub fn scan_urls(line: &str) -> Vec<UrlMatch> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let Some(scheme_len) = match_scheme(&chars, i) else {
            i += 1;
            continue;
        };
        // Reject schemes glued to an ASCII word ("xhttps://..." is not a link).
        // Non-ASCII stays clickable: CJK prose legitimately abuts URLs unspaced.
        if i > 0 && (chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '.') {
            i += scheme_len;
            continue;
        }
        let body = i + scheme_len;
        let mut j = body;
        while j < n && is_url_char(chars[j]) {
            j += 1;
        }
        // Trim the tail: sentence punctuation always; closers only when unbalanced.
        let mut k = j;
        while k > body {
            let c = chars[k - 1];
            if matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"') {
                k -= 1;
                continue;
            }
            let opener = match c {
                ')' => Some('('),
                ']' => Some('['),
                _ => None,
            };
            if let Some(open) = opener {
                let inner = &chars[body..k - 1];
                let opens = inner.iter().filter(|&&x| x == open).count();
                let closes = inner.iter().filter(|&&x| x == c).count();
                if closes >= opens {
                    k -= 1;
                    continue;
                }
            }
            break;
        }
        if k > body {
            out.push(UrlMatch { start: i, end: k, url: chars[i..k].iter().collect() });
            i = k;
        } else {
            i += scheme_len;
        }
    }
    out
}

/// Build the regex pattern for a literal find-in-terminal query, alacritty-style
/// smart case: all-lowercase queries match case-insensitively (`(?i)` prefix),
/// any uppercase character makes the search case-sensitive. Every ASCII
/// punctuation char is backslash-escaped so the query is always a literal.
pub fn search_pattern(query: &str) -> String {
    let mut pat = String::with_capacity(query.len() + 8);
    if !query.chars().any(|c| c.is_uppercase()) {
        pat.push_str("(?i)");
    }
    for c in query.chars() {
        if c.is_ascii() && c.is_ascii_punctuation() {
            pat.push('\\');
        }
        pat.push(c);
    }
    pat
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(line: &str) -> Vec<String> {
        scan_urls(line).into_iter().map(|m| m.url).collect()
    }

    #[test]
    fn finds_a_plain_url_with_char_indices() {
        let m = scan_urls("see https://example.com/x for docs");
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].url, "https://example.com/x");
        assert_eq!(m[0].start, 4);
        assert_eq!(m[0].end, 25);
    }

    #[test]
    fn finds_multiple_urls_and_localhost() {
        assert_eq!(
            urls("a http://localhost:5173/app and https://x.co/y"),
            vec!["http://localhost:5173/app", "https://x.co/y"]
        );
    }

    #[test]
    fn trims_sentence_punctuation() {
        assert_eq!(urls("Deployed to https://foo.bar."), vec!["https://foo.bar"]);
        assert_eq!(urls("at http://localhost:3000: then"), vec!["http://localhost:3000"]);
        assert_eq!(urls("really? https://a.b/c?!"), vec!["https://a.b/c"]);
    }

    #[test]
    fn balanced_parens_are_kept_unbalanced_trimmed() {
        assert_eq!(
            urls("(https://en.wikipedia.org/wiki/Rust_(language))"),
            vec!["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
        assert_eq!(urls("(see https://example.com)"), vec!["https://example.com"]);
    }

    #[test]
    fn scheme_is_case_insensitive_and_needs_a_body() {
        assert_eq!(urls("HTTPS://EXAMPLE.COM"), vec!["HTTPS://EXAMPLE.COM"]);
        assert!(urls("empty http:// scheme").is_empty());
    }

    #[test]
    fn glued_schemes_and_quotes_stop_the_scan() {
        assert!(urls("xhttps://not.a.link").is_empty());
        assert_eq!(urls("\"https://q.uo/ted\""), vec!["https://q.uo/ted"]);
        assert_eq!(urls("<https://angle.br/ackets>"), vec!["https://angle.br/ackets"]);
    }

    #[test]
    fn search_pattern_is_smart_case() {
        assert_eq!(search_pattern("foo"), "(?i)foo");
        assert_eq!(search_pattern("Foo"), "Foo");
    }

    #[test]
    fn search_pattern_escapes_regex_metachars() {
        assert_eq!(search_pattern("a.b*c"), "(?i)a\\.b\\*c");
        assert_eq!(search_pattern("x(y)[z]"), "(?i)x\\(y\\)\\[z\\]");
        assert_eq!(search_pattern("$1|^2"), "(?i)\\$1\\|\\^2");
    }
}
