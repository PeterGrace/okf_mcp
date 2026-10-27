//! The secret scan: a port of `okf.py`'s `SECRET_RULES`, `ASSIGN_RE`,
//! `PLACEHOLDER_RE`, `_literal_secret`, `_mask` and `scan_secrets`.
//!
//! The `regex` crate has no lookaround. Python's lookbehinds
//! (`(?<![A-Za-z0-9_])`) and the one lookahead are emulated with a consumed
//! neighbour character and a capture group holding the real match (verified
//! fact 7 in the M2 plan), so every pattern stays linear-time. A `RegexSet`
//! over all patterns rejects the common line (no hit) in one pass.
//!
//! Python's `re.I` lets `i`/`I` match U+0130 and U+0131 and the `regex`
//! crate's `(?i)` does not, so the assignment keywords spell those letters
//! out as `[iI\u{130}\u{131}]` (verified fact 6).

use std::sync::LazyLock;

use regex::{Regex, RegexSet};

use crate::okf::analysis::Finding;

/// The catalog's own copy of `okf.py`, which `okf.py` skips (as itself)
/// because its rule table would match its own source.
pub const OKF_PY_PATH: &str = "tools/okf.py";

/// One compiled token rule.
struct TokenRule {
    /// Rule id without the `secret:` prefix, e.g. `github-token`.
    id: &'static str,
    /// The pattern; group 1 is Python's `m.group(0)`.
    pattern: Regex,
    /// The explanation that ends the finding's message.
    why: &'static str,
}

/// `SECRET_RULES` as `(id, pattern, why)`, in `okf.py`'s order. Group 1 of
/// each pattern is the token Python matches; the `(?:\A|[^...])` before it
/// stands in for the lookbehind and the `(?:[^A-Z0-9]|\z)` after the AWS
/// key for the lookahead.
const TOKEN_RULES: [(&str, &str, &str); 6] = [
    (
        "private-key",
        r"(-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----)",
        "PEM private-key header",
    ),
    (
        "github-token",
        r"(?:\A|[^A-Za-z0-9_])(gh[pousr]_[A-Za-z0-9]{36,})",
        "GitHub token prefix (ghp_/gho_/ghu_/ghs_/ghr_) followed by 36+ token characters",
    ),
    (
        "github-pat",
        r"(?:\A|[^A-Za-z0-9_])(github_pat_[A-Za-z0-9_]{22,})",
        "fine-grained GitHub PAT prefix github_pat_ followed by 22+ token characters",
    ),
    (
        "slack-token",
        r"(?:\A|[^A-Za-z0-9_-])(xox[bpa]-[A-Za-z0-9-]{10,})",
        "Slack token prefix xoxb-/xoxp-/xoxa- followed by 10+ token characters",
    ),
    (
        "sk-key",
        r"(?:\A|[^A-Za-z0-9_-])(sk-(?:[A-Za-z0-9]+-)*[A-Za-z0-9_]{20,})",
        "sk- API-key prefix (OpenAI/Anthropic/Stripe style) followed by 20+ key characters",
    ),
    (
        "aws-key-id",
        r"(?:\A|[^A-Z0-9])((?:AKIA|ASIA)[A-Z0-9]{16})(?:[^A-Z0-9]|\z)",
        "AWS access key id (AKIA/ASIA + 16 uppercase alphanumerics)",
    ),
];

/// `ASSIGN_RE`: group 1 is the keyword, group 2 the value (Python's group 3).
///
/// `(?<![\w-])` becomes a consumed `(?:\A|[^\w-])`, with Python's `\w` as
/// `[\p{L}\p{N}_]`. Python's `([\"']?)` group is a plain optional quote here,
/// and its trailing `\2?` (an optional closing quote) is dropped: the value
/// class already stops at a quote, so neither change alters a captured value
/// or where the next match starts (fact 7). Python's `\s` is `[\s\x1c-\x1f]`.
/// The `re.I` flag only matters for the keywords, so only they are inside
/// `(?i:...)`.
const ASSIGN: &str = concat!(
    r"(?:\A|[^\p{L}\p{N}_\-])",
    r"(?i:(password|passwd|passphrase|secret|cl[iI\u{130}\u{131}]ent[_-]?secret|token|",
    r"ap[iI\u{130}\u{131}][_-]?key|access[_-]?key))",
    r#"["']?[ \t]*[:=][ \t]*["']?([^\s\x1c-\x1f"'`,;<>]+)"#,
);

/// Every compiled pattern, built once on first use.
struct Rules {
    /// The token rules, in `TOKEN_RULES` order.
    tokens: Vec<TokenRule>,
    /// The compiled `ASSIGN` pattern.
    assign: Regex,
    /// Every token pattern, then `ASSIGN`, in that order, so set index `i`
    /// is `tokens[i]` and index `tokens.len()` is `assign`.
    any: RegexSet,
}

// `LazyLock` is Rust's thread-safe lazily initialised global, like a
// module-level `re.compile` that runs on first access instead of at import.
static RULES: LazyLock<Rules> = LazyLock::new(|| {
    let tokens = TOKEN_RULES
        .iter()
        .map(|&(id, pattern, why)| TokenRule {
            id,
            pattern: Regex::new(pattern).expect("token rule regex is valid"),
            why,
        })
        .collect();
    let patterns = TOKEN_RULES.iter().map(|&(_, pattern, _)| pattern);
    let any = RegexSet::new(patterns.chain([ASSIGN])).expect("secret rule set is valid");
    Rules {
        tokens,
        assign: Regex::new(ASSIGN).expect("ASSIGN regex is valid"),
        any,
    }
});

/// `PLACEHOLDER_RE`: values that are clearly not real credentials. None of
/// its keywords contains an `i`, so plain `(?i)` matches as `re.I` does.
/// Python's `re.match` with `^...$` is `\A...\z` here: a value never holds
/// a newline, so `$`'s "before a trailing newline" case cannot arise.
static PLACEHOLDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\A(?:x+|\*+|\.+|changeme|redacted|example[\p{L}\p{N}_]*|placeholder|none|null|",
        r"true|false|todo|tbd|your[_-].*|my[_-].*|<.*>|\$.*|\{\{.*|%.*|env|",
        r"secret[\p{L}\p{N}_]*|password[\p{L}\p{N}_]*|token[\p{L}\p{N}_]*)\z",
    ))
    .expect("PLACEHOLDER regex is valid")
});

/// Python's `re.search(r"\d", value)`: any Unicode decimal digit (`Nd`).
static DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\d").expect("DIGIT regex is valid"));

/// Python's `re.match(r"^\w+\(", value)`: a value that looks like a call.
static CALL_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[\p{L}\p{N}_]+\(").expect("CALL_LIKE regex is valid"));

/// `_literal_secret`: why an assigned value looks like a real credential,
/// or `None` for prose, references and placeholders.
fn literal_secret(value: &str) -> Option<&'static str> {
    // Python's `len` counts code points, as `chars().count()` does.
    let length = value.chars().count();
    if length < 8 || PLACEHOLDER.is_match(value) {
        return None;
    }
    let reference = ["/", "~", "http:", "https:", "file:"]
        .iter()
        .any(|prefix| value.starts_with(prefix));
    if reference || CALL_LIKE.is_match(value) {
        return None;
    }
    // The letter checks are ASCII (`[A-Za-z]`) in `okf.py` too.
    let has_digit = DIGIT.is_match(value);
    let has_alpha = value.bytes().any(|b| b.is_ascii_alphabetic());
    let mixed = value.bytes().any(|b| b.is_ascii_lowercase())
        && value.bytes().any(|b| b.is_ascii_uppercase());
    if has_digit && has_alpha {
        return Some("value mixes letters and digits");
    }
    if mixed && length >= 12 {
        return Some("value is 12+ mixed-case characters");
    }
    if length >= 20 && !value.contains(['-', '_', '.']) {
        return Some("value is a 20+ character unbroken token");
    }
    None
}

/// `_mask`: the first four characters and `...`, or just `...` for a value
/// of four characters or fewer.
fn mask(value: &str) -> String {
    // `char_indices().nth(4)` is the byte offset of the fifth character, so
    // slicing there never splits a multi-byte UTF-8 character.
    match value.char_indices().nth(4) {
        Some((cut, _)) => format!("{}...", &value[..cut]),
        None => "...".to_string(),
    }
}

/// Scans one file's text for likely secrets, as `okf.py`'s `scan_secrets`.
///
/// Callers skip [`OKF_PY_PATH`], as `okf.py` skips its own file.
///
/// # Arguments
/// * `path` - catalog-relative path, reported in each finding.
/// * `text` - the file's text, read with universal newlines.
///
/// # Returns
/// One [`Finding`] per hit, line by line: token rules in table order (the
/// first match of each), then assignments left to right, as `okf.py`
/// reports them.
///
/// # Examples
/// ```
/// use okf_mcp::okf::analysis::secrets::scan_secrets;
/// // Built at runtime so no source file holds a secret-shaped string.
/// let line = format!("password = {}", "Ab1".repeat(4));
/// let hits = scan_secrets("notes.md", &line);
/// assert_eq!(hits[0].rule, "secret:assignment");
/// assert!(scan_secrets("notes.md", "password = changeme").is_empty());
/// ```
pub fn scan_secrets(path: &str, text: &str) -> Vec<Finding> {
    // `&*RULES` forces the `LazyLock` and borrows the `Rules` inside it.
    let rules = &*RULES;
    let mut hits = Vec::new();
    // Mirrors `okf.py`'s `for n, line in enumerate(text.split("\n"), 1)`.
    // Not `splitlines()`: `\x0b`, `\x0c`, `\x1c`-`\x1e`, `\x85`, U+2028 and
    // U+2029 survive the universal-newline read and stay inside a line, so
    // line numbers count `\n` only.
    for (index, line) in text.split('\n').enumerate() {
        let candidates = rules.any.matches(line);
        if !candidates.matched_any() {
            continue;
        }
        let number = index + 1;
        for (rule_index, rule) in rules.tokens.iter().enumerate() {
            if !candidates.matched(rule_index) {
                continue;
            }
            // `and_then` chains two `Option`s: no match, or (impossibly) no
            // group 1, both give `None`.
            if let Some(token) = rule.pattern.captures(line).and_then(|caps| caps.get(1)) {
                hits.push(Finding {
                    file: path.to_string(),
                    line: number,
                    rule: format!("secret:{}", rule.id),
                    message: format!("possible secret {}: {}", mask(token.as_str()), rule.why),
                });
            }
        }
        if candidates.matched(rules.tokens.len()) {
            assignments(&rules.assign, path, number, line, &mut hits);
        }
    }
    hits
}

/// `ASSIGN_RE.finditer(line)` filtered by [`literal_secret`], appended to
/// `hits`.
fn assignments(assign: &Regex, path: &str, number: usize, line: &str, hits: &mut Vec<Finding>) {
    let mut pos = 0;
    // `captures_at` still sees the text before `pos`, so `\A` only matches
    // at the true line start. Resuming at the value's end lets the next
    // match consume the character after it as its "lookbehind" character,
    // which is the character Python's lookbehind would inspect.
    while let Some(caps) = assign.captures_at(line, pos) {
        // `let ... else` destructures or runs the `else` block, which must
        // leave the loop; both groups are mandatory, so it never runs.
        let (Some(keyword), Some(value)) = (caps.get(1), caps.get(2)) else {
            break;
        };
        if let Some(why) = literal_secret(value.as_str()) {
            hits.push(Finding {
                file: path.to_string(),
                line: number,
                rule: "secret:assignment".to_string(),
                message: format!(
                    "possible secret: '{}' followed by a literal value {} ({why})",
                    keyword.as_str(),
                    mask(value.as_str())
                ),
            });
        }
        pos = value.end();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds rule-matching strings at runtime so no source file holds one.
    fn fake(prefix: &str, body: &str, n: usize) -> String {
        format!("{prefix}{}", body.repeat(n))
    }

    fn rules(text: &str) -> Vec<String> {
        scan_secrets("x.md", text)
            .into_iter()
            .map(|f| f.rule)
            .collect()
    }

    #[test]
    fn token_rules_fire_with_lookbehind_and_lookahead() {
        let github = fake("gh", "p_", 1) + &"A1".repeat(18);
        assert_eq!(rules(&format!("token {github}")), ["secret:github-token"]);
        assert!(
            rules(&format!("x{github}")).is_empty(),
            "preceded by a token character"
        );
        let aws = fake("AK", "IA", 1) + &"Z".repeat(16);
        // `id` is not an ASSIGN_RE keyword, so only the token rule fires.
        assert_eq!(rules(&format!("id={aws}.")), ["secret:aws-key-id"]);
        assert!(
            rules(&format!("{aws}Q")).is_empty(),
            "followed by an id character"
        );
        let pem = format!("-----BEGIN {} PRIVATE KEY-----", "RSA");
        assert_eq!(rules(&pem), ["secret:private-key"]);
        let pat = fake("github", "_pat_", 1) + &"a".repeat(22);
        assert_eq!(rules(&format!(" {pat}")), ["secret:github-pat"]);
        assert!(
            rules(&format!("x{pat}")).is_empty(),
            "preceded by a token character"
        );
        // `sk-` takes `-`-separated segments before its 20+ key characters,
        // and `-` is in its lookbehind class.
        let sk = fake("s", "k-", 1) + "proj-" + &"a1".repeat(10);
        assert_eq!(rules(&sk), ["secret:sk-key"]);
        assert!(rules(&format!("-{sk}")).is_empty(), "preceded by a dash");
    }

    #[test]
    fn lines_split_on_newline_only() {
        // U+2028, form feed and U+0085 would end a line for `splitlines()`.
        let text = format!("a\u{2028}b\x0cc\u{85}d\npassword: {}", "Ab1".repeat(4));
        let lines: Vec<usize> = scan_secrets("x.md", &text).iter().map(|f| f.line).collect();
        assert_eq!(lines, [2]);
    }

    #[test]
    fn token_rules_report_in_table_order() {
        let github = fake("gh", "p_", 1) + &"A1".repeat(18);
        let slack = fake("xo", "xb-", 1) + &"1234567890".repeat(2);
        // Slack comes first on the line, but github-token is earlier in the table.
        assert_eq!(
            rules(&format!("{slack} {github}")),
            ["secret:github-token", "secret:slack-token"]
        );
    }

    #[test]
    fn messages_mask_the_value() {
        let slack = fake("xo", "xb-", 1) + &"1234567890".repeat(2);
        let finding = &scan_secrets("a/b.md", &format!("\n{slack}"))[0];
        assert_eq!((finding.file.as_str(), finding.line), ("a/b.md", 2));
        assert_eq!(
            finding.message,
            "possible secret xoxb...: Slack token prefix xoxb-/xoxp-/xoxa- followed by 10+ \
             token characters"
        );
    }

    #[test]
    fn assignments_need_a_literal_looking_value() {
        let value = fake("Ab", "c1", 4);
        let hits = scan_secrets("x.md", &format!("password: \"{value}\""));
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].message,
            "possible secret: 'password' followed by a literal value Abc1... (value mixes \
             letters and digits)"
        );
        for prose in [
            "password: changeme",
            "token: ${TOKEN_FROM_ENV}",
            "secret: /var/run/secrets/x",
            "api_key: lookup(name)",
            "my-token: abcdefgh1",
            "token: short1",
        ] {
            assert!(scan_secrets("x.md", prose).is_empty(), "{prose}");
        }
        assert_eq!(
            rules(&format!("Client_Secret={}", fake("", "aB", 6))),
            ["secret:assignment"]
        );
        let mixed_case = &scan_secrets("x.md", &format!("password: {}", "aBcDeF".repeat(2)))[0];
        assert!(mixed_case
            .message
            .ends_with("(value is 12+ mixed-case characters)"));
        let unbroken = &scan_secrets("x.md", &format!("API-KEY = {}", "q".repeat(20)))[0];
        assert!(unbroken
            .message
            .ends_with("(value is a 20+ character unbroken token)"));
    }

    #[test]
    fn assignment_keywords_ignore_case_like_python() {
        // Python's re.I lets `i` match U+0131 (dotless i) and U+0130; the
        // port must too. The `regex` crate already folds U+017F (long s) and
        // U+212A (Kelvin sign) like Python, and the message keeps the
        // keyword as written.
        let value = fake("Ab", "c1", 4);
        assert_eq!(
            rules(&format!("ap\u{131}_key: {value}")),
            ["secret:assignment"]
        );
        assert_eq!(
            rules(&format!("AP\u{130}_KEY: {value}")),
            ["secret:assignment"]
        );
        let keyword = "to\u{212a}en";
        let kelvin = &scan_secrets("x.md", &format!("{keyword}={value}"))[0];
        assert_eq!(
            kelvin.message,
            format!(
                "possible secret: '{keyword}' followed by a literal value Abc1... (value mixes \
                 letters and digits)"
            )
        );
        assert_eq!(
            rules(&format!("\u{17f}ecret={value}")),
            ["secret:assignment"]
        );
    }

    #[test]
    fn assignment_lookbehind_and_value_use_python_classes() {
        let value = fake("Ab", "c1", 4);
        // `\w` is Unicode: a letter or digit before the keyword blocks it.
        for blocked in ["\u{e9}", "_", "\u{663}"] {
            assert!(
                rules(&format!("{blocked}token={value}")).is_empty(),
                "{blocked}"
            );
        }
        // U+00A0 and U+001C are whitespace to Python, so they end the value.
        for end in ["\u{a0}", "\u{1c}"] {
            let hits = scan_secrets("x.md", &format!("token={value}{end}x"));
            assert!(hits[0].message.contains("Abc1..."), "{end:?}");
        }
        // `\d` is any Unicode decimal digit, as in Python.
        let digits = scan_secrets("x.md", &format!("token={}abcd", "\u{663}".repeat(4)));
        assert!(digits[0]
            .message
            .contains("(value mixes letters and digits)"));
    }

    #[test]
    fn several_assignments_on_one_line_are_all_reported() {
        let value = fake("Ab", "c1", 4);
        let line = format!("token='{value}' secret=\"{value}\"");
        assert_eq!(rules(&line), ["secret:assignment", "secret:assignment"]);
        // A keyword glued to the previous value is part of that value; one
        // after a closing quote starts a new match.
        assert_eq!(
            rules(&format!(
                "{keyword}={value}{keyword}={value}",
                keyword = "token"
            )),
            ["secret:assignment"]
        );
        assert_eq!(
            rules(&format!("token=\"{value}\"token={value}")),
            ["secret:assignment", "secret:assignment"]
        );
    }

    #[test]
    fn mask_keeps_four_characters() {
        assert_eq!(mask("abcdef"), "abcd...");
        assert_eq!(mask("abcd"), "...");
        assert_eq!(
            mask("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"),
            "\u{e9}\u{e9}\u{e9}\u{e9}..."
        );
    }
}
