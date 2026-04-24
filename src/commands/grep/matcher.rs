//! Pattern matching: literal (case-sensitive or ASCII-CI via memchr) or regex.

/// Matches a pattern against an event's text. `find` returns (byte_pos, match_len)
/// on the first hit — regex matches have variable length, literals are fixed.
pub(super) enum Matcher {
    Literal {
        needle: String,
        /// Lowercased needle, retained once so CI search avoids re-allocating.
        needle_lower: String,
        case_sensitive: bool,
    },
    Regex(regex::Regex),
}

impl Matcher {
    pub(super) fn build(pattern: &str, case_sensitive: bool, regex: bool) -> anyhow::Result<Self> {
        if regex {
            // Unicode mode so `.` matches a codepoint (not a raw byte) — required
            // for our &str haystacks, and lets patterns like `keywords.*(Haiku|YAKE)`
            // compile without the "pattern can match invalid UTF-8" rejection.
            let re = regex::RegexBuilder::new(pattern)
                .case_insensitive(!case_sensitive)
                .build()
                .map_err(|e| anyhow::anyhow!("invalid regex {pattern:?}: {e}"))?;
            Ok(Matcher::Regex(re))
        } else {
            Ok(Matcher::Literal {
                needle: pattern.to_string(),
                needle_lower: pattern.to_ascii_lowercase(),
                case_sensitive,
            })
        }
    }

    pub(super) fn find(&self, hay: &str) -> Option<(usize, usize)> {
        match self {
            Matcher::Literal {
                needle,
                needle_lower,
                case_sensitive,
            } => {
                let pos = if *case_sensitive {
                    hay.find(needle.as_str())?
                } else {
                    find_ascii_ci(hay.as_bytes(), needle_lower.as_bytes())?
                };
                Some((pos, needle.len()))
            }
            Matcher::Regex(re) => {
                let m = re.find(hay)?;
                Some((m.start(), m.end() - m.start()))
            }
        }
    }
}

/// Case-insensitive ASCII substring search. No allocation; uses `memchr` to
/// jump to candidate positions based on either case of the needle's first byte.
fn find_ascii_ci(haystack: &[u8], needle_lower: &[u8]) -> Option<usize> {
    if needle_lower.is_empty() {
        return Some(0);
    }
    let n = needle_lower.len();
    if haystack.len() < n {
        return None;
    }
    let first_lo = needle_lower[0];
    let first_up = if first_lo.is_ascii_lowercase() {
        first_lo - 32
    } else {
        first_lo
    };

    let limit = haystack.len() - n;
    let mut start = 0;
    while start <= limit {
        let window = &haystack[start..=limit];
        let p1 = memchr::memchr(first_lo, window);
        let p = if first_lo == first_up {
            p1
        } else {
            let p2 = memchr::memchr(first_up, window);
            match (p1, p2) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (x, y) => x.or(y),
            }
        };
        let off = p?;
        let i = start + off;
        if eq_ci(&haystack[i..i + n], needle_lower) {
            return Some(i);
        }
        start = i + 1;
    }
    None
}

fn eq_ci(a: &[u8], b_lower: &[u8]) -> bool {
    a.iter().zip(b_lower).all(|(x, y)| {
        let xl = if x.is_ascii_uppercase() { x + 32 } else { *x };
        xl == *y
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_literal(hay: &str, needle: &str, case_sensitive: bool) -> Option<(usize, usize)> {
        Matcher::build(needle, case_sensitive, false)
            .unwrap()
            .find(hay)
    }

    #[test]
    fn find_case_insensitive() {
        assert_eq!(find_literal("Hello World", "world", false), Some((6, 5)));
        assert_eq!(find_literal("Hello World", "world", true), None);
    }

    #[test]
    fn find_case_insensitive_handles_all_upper() {
        assert_eq!(find_literal("xxxHELLOyyy", "hello", false), Some((3, 5)));
    }

    #[test]
    fn find_case_insensitive_empty_needle() {
        assert_eq!(find_literal("abc", "", false), Some((0, 0)));
    }

    #[test]
    fn find_ci_no_match_returns_none() {
        assert!(find_literal("needle_not_here", "zzz", false).is_none());
    }

    #[test]
    fn find_ci_haystack_shorter_than_needle() {
        assert!(find_ascii_ci(b"ab", b"abcd").is_none());
    }

    #[test]
    fn regex_matches_alternation_and_escapes() {
        let m = Matcher::build(r"P2\.?5", true, true).unwrap();
        assert_eq!(m.find("about P25 here").map(|(_, l)| l), Some(3));
        assert_eq!(m.find("see P2.5 today").map(|(_, l)| l), Some(4));
        assert!(m.find("P3.5 mismatch").is_none());

        let m = Matcher::build("choreographer|budget", false, true).unwrap();
        assert!(m.find("the Budget plan").is_some());
        assert!(m.find("Choreographer led").is_some());
        assert!(m.find("nothing here").is_none());
    }

    #[test]
    fn regex_dot_with_alternation_and_quantifier() {
        // Regression: `.*` + alternation previously failed to compile under
        // `unicode(false)` with "pattern can match invalid UTF-8".
        let m = Matcher::build("keywords.*(Haiku|Sonnet|YAKE)", true, true).unwrap();
        assert!(m.find("keywords include Haiku today").is_some());
        assert!(m.find("keywords with YAKE extraction").is_some());
        assert!(m.find("keywords only").is_none());

        let m = Matcher::build("keyword.+Haiku|Haiku.+keyword", true, true).unwrap();
        assert!(m.find("keyword then Haiku").is_some());
        assert!(m.find("Haiku before keyword").is_some());
    }

    #[test]
    fn regex_invalid_pattern_errors() {
        assert!(Matcher::build("(", true, true).is_err());
    }
}
