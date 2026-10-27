//! Catalog analyses: ports of `okf.py`'s `lint`, `links`, `oldest`, `sprawl`
//! and secret scan. Each is a pure function over a catalog snapshot
//! ([`crate::okf::snapshot::Snapshot`]) or a file's text (and an injected "now"),
//! returning report structs whose JSON (`serde_json::to_value`) equals
//! `okf.py`'s `--json` output, keys in the same order.
//!
//! Report structs are plain records with public fields, the documented
//! exception to the private-fields rule (like `yaml::YamlError`).

pub mod links;
pub mod oldest;
pub mod secrets;
pub mod sprawl;

/// One lint-style finding: `{"file", "line", "rule", "message"}` in `okf.py`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Finding {
    /// Catalog-relative path, without a leading `/`.
    pub file: String,
    /// 1-based line number.
    pub line: usize,
    /// Rule id, e.g. `broken-link` or `secret:github-token`.
    pub rule: String,
    /// Human-readable explanation, exactly as `okf.py` words it.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finding_serializes_with_okf_py_key_order() {
        let finding = Finding {
            file: "a.md".to_string(),
            line: 3,
            rule: "broken-link".to_string(),
            message: "m".to_string(),
        };
        let json = serde_json::to_string(&finding).expect("a Finding always serializes");
        assert_eq!(
            json,
            r#"{"file":"a.md","line":3,"rule":"broken-link","message":"m"}"#
        );
    }
}
