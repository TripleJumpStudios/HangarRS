//! Hand-written lexer and recursive-descent parser for Steam's VDF dialect.
//!
//! ```text
//! "root_key"
//! {
//!     "nested_key"
//!     {
//!         "leaf_key"    "leaf_value"
//!     }
//! }
//! ```
//!
//! No crate covers Steam's full dialect, hence this. Rules:
//!
//! * Keys and string values are double-quoted; `\` escapes the next character.
//! * `{` opens a map, `}` closes it.
//! * `//` starts a comment **outside a quoted string only**.
//! * Keys are lowercased; values are left exactly as written.
//! * Duplicate keys: last one wins, matching Steam.
//! * [`IndexMap`] preserves file order, so a re-serialised file reads the same.

use indexmap::IndexMap;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Str(String),
    Open,
    Close,
}

#[derive(Debug, Clone, PartialEq)]
pub enum VdfNode {
    Value(String),
    Map(IndexMap<String, VdfNode>),
}

impl VdfNode {
    /// Fetch a nested node by dot-separated path. Keys are matched lowercased.
    ///
    /// ```
    /// # use hangar_steam::vdf;
    /// let node = vdf::parse(r#""a" { "b" { "c" "1" } }"#).unwrap();
    /// assert_eq!(node.get_path("a.b.c").and_then(|n| n.as_str()), Some("1"));
    /// ```
    pub fn get_path(&self, path: &str) -> Option<&VdfNode> {
        let mut current = self;
        for key in path.split('.') {
            current = current.as_map()?.get(&key.to_lowercase())?;
        }
        Some(current)
    }

    /// The string at a dot-separated path, if it is a leaf.
    pub fn get_str(&self, path: &str) -> Option<&str> {
        self.get_path(path)?.as_str()
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            VdfNode::Value(s) => Some(s.as_str()),
            VdfNode::Map(_) => None,
        }
    }

    pub fn as_map(&self) -> Option<&IndexMap<String, VdfNode>> {
        match self {
            VdfNode::Map(m) => Some(m),
            VdfNode::Value(_) => None,
        }
    }
}

/// Parse a VDF document into a [`VdfNode::Map`] of its top-level keys.
pub fn parse(input: &str) -> Result<VdfNode, String> {
    let tokens = tokenize(input)?;
    let mut pos = 0;
    let mut map = IndexMap::new();

    while pos < tokens.len() {
        match &tokens[pos] {
            Token::Str(key) => {
                let key = key.to_lowercase();
                pos += 1;
                let value = parse_node(&tokens, &mut pos)?;
                map.insert(key, value);
            }
            other => return Err(format!("expected a key at top level, got {other:?}")),
        }
    }

    Ok(VdfNode::Map(map))
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&c) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '{' => {
                chars.next();
                tokens.push(Token::Open);
            }
            '}' => {
                chars.next();
                tokens.push(Token::Close);
            }
            // Comments are recognised here rather than by stripping the text
            // first. A pre-pass that deletes everything after `//` also
            // destroys it inside quoted values — a UNC path or a URL in
            // config.vdf would be silently truncated.
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                } else {
                    return Err("unexpected '/' outside a comment".into());
                }
            }
            '"' => {
                chars.next(); // opening quote
                let mut s = String::new();
                loop {
                    match chars.next() {
                        None => return Err("unterminated string".into()),
                        Some('\\') => match chars.next() {
                            None => return Err("unterminated escape".into()),
                            // Steam writes Windows paths as `C:\\foo`, so the
                            // common case is an escaped backslash. Anything
                            // else passes through as the literal character.
                            Some(esc) => s.push(esc),
                        },
                        Some('"') => break,
                        Some(ch) => s.push(ch),
                    }
                }
                tokens.push(Token::Str(s));
            }
            other => return Err(format!("unexpected character {other:?}")),
        }
    }

    Ok(tokens)
}

fn parse_node(tokens: &[Token], pos: &mut usize) -> Result<VdfNode, String> {
    match tokens.get(*pos) {
        Some(Token::Open) => {
            *pos += 1;
            Ok(VdfNode::Map(parse_map(tokens, pos)?))
        }
        Some(Token::Str(val)) => {
            let val = val.clone();
            *pos += 1;
            Ok(VdfNode::Value(val))
        }
        other => Err(format!("expected a value or '{{', got {other:?}")),
    }
}

fn parse_map(tokens: &[Token], pos: &mut usize) -> Result<IndexMap<String, VdfNode>, String> {
    let mut map: IndexMap<String, VdfNode> = IndexMap::new();

    loop {
        match tokens.get(*pos) {
            Some(Token::Close) => {
                *pos += 1;
                return Ok(map);
            }
            Some(Token::Str(key)) => {
                let key = key.to_lowercase();
                *pos += 1;
                let value = parse_node(tokens, pos)?;
                map.insert(key, value);
            }
            None => return Err("unexpected end of input inside a map".into()),
            Some(other) => return Err(format!("expected a key or '}}', got {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_key_values() {
        let node = parse(
            r#"
"root"
{
    "key1" "value1"
    "Key2" "Value2"
}
"#,
        )
        .unwrap();
        assert_eq!(node.get_str("root.key1"), Some("value1"));
        // Keys are lowercased; values are not.
        assert_eq!(node.get_str("root.key2"), Some("Value2"));
    }

    #[test]
    fn parses_nested_maps() {
        let node = parse(r#""outer" { "inner" { "leaf" "hello" } }"#).unwrap();
        assert_eq!(node.get_str("outer.inner.leaf"), Some("hello"));
        assert!(node.get_path("outer.inner").unwrap().as_map().is_some());
        assert!(node.get_path("outer.missing").is_none());
        // Descending through a leaf is not an error, just absent.
        assert!(node.get_path("outer.inner.leaf.deeper").is_none());
    }

    #[test]
    fn strips_whole_line_and_trailing_comments() {
        let node = parse(
            r#"
"root"
{
    // a comment line
    "key" "val" // a trailing comment
}
"#,
        )
        .unwrap();
        assert_eq!(node.get_str("root.key"), Some("val"));
    }

    /// Stripping comments by deleting everything after `//` on each line,
    /// before tokenizing, would truncate any value *containing* `//` — a URL
    /// or a UNC path.
    #[test]
    fn a_double_slash_inside_a_quoted_value_survives() {
        let node =
            parse(r#""root" { "url" "https://example.com/x" "unc" "//server/share" }"#).unwrap();
        assert_eq!(node.get_str("root.url"), Some("https://example.com/x"));
        assert_eq!(node.get_str("root.unc"), Some("//server/share"));
    }

    /// Steam writes Windows paths with doubled backslashes.
    #[test]
    fn escaped_backslashes_collapse() {
        let node = parse(r#""root" { "p" "C:\\users\\steamuser" }"#).unwrap();
        assert_eq!(node.get_str("root.p"), Some(r"C:\users\steamuser"));
    }

    #[test]
    fn later_duplicate_keys_win() {
        let node = parse(r#""root" { "k" "first" "k" "second" }"#).unwrap();
        assert_eq!(node.get_str("root.k"), Some("second"));
    }

    #[test]
    fn file_order_is_preserved() {
        let node = parse(r#""root" { "z" "1" "a" "2" "m" "3" }"#).unwrap();
        let keys: Vec<_> = node
            .get_path("root")
            .unwrap()
            .as_map()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(keys, ["z", "a", "m"]);
    }

    #[test]
    fn several_top_level_blocks_parse() {
        let node = parse(r#""a" { "x" "1" } "b" { "y" "2" }"#).unwrap();
        assert_eq!(node.get_str("a.x"), Some("1"));
        assert_eq!(node.get_str("b.y"), Some("2"));
    }

    #[test]
    fn empty_input_is_an_empty_map() {
        let node = parse("").unwrap();
        assert!(node.as_map().unwrap().is_empty());
        let node = parse("   \n\t // just a comment\n").unwrap();
        assert!(node.as_map().unwrap().is_empty());
    }

    #[test]
    fn malformed_input_is_an_error_not_a_panic() {
        for bad in [
            r#""root" {"#,           // unclosed map
            r#""root" { "k" }"#,     // key with no value
            r#""unterminated"#,      // unterminated string
            r#""root" } "#,          // stray close
            r#"root "x""#,           // unquoted key
            r#""root" { "k" "v" \"#, // unterminated escape
        ] {
            assert!(parse(bad).is_err(), "expected an error for {bad:?}");
        }
    }
}
