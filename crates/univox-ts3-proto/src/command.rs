//! TS3 command/parameter serialization shared by ServerQuery (TCP) and the
//! UDP control channel.
//!
//! Wire syntax (single line): `name pos1 k1=v1 k2=v2|-opt1|-opt2`, multiple
//! data rows separated by an unescaped `|`.

use std::fmt::Write as _;

use crate::error::{Error, Result};

/// Escape a parameter value using the TS3 escape table (see
/// `doc/serverquery/serverquery.html` in the server distribution).
pub fn escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for c in input.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '/' => out.push_str("\\/"),
            ' ' => out.push_str("\\s"),
            '|' => out.push_str("\\p"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x0b' => out.push_str("\\v"),
            _ => out.push(c),
        }
    }
    out
}

/// Unescape a parameter value. Unknown escapes degrade to the escaped char.
pub fn unescape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            Some('s') => out.push(' '),
            Some('p') => out.push('|'),
            Some('a') => out.push('\x07'),
            Some('b') => out.push('\x08'),
            Some('f') => out.push('\x0c'),
            Some('n') => out.push('\n'),
            Some('r') => out.push('\r'),
            Some('t') => out.push('\t'),
            Some('v') => out.push('\x0b'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Split on `sep`, ignoring separators inside `\x` escape sequences.
fn split_unescaped(s: &str, sep: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if c == '\\' {
            escaped = true;
        } else if c == sep {
            parts.push(&s[start..i]);
            start = i + 1;
        }
    }
    parts.push(&s[start..]);
    parts
}

/// One response row: an ordered list of key/value pairs.
pub type Row = Vec<(String, String)>;

/// Ergonomic key lookup for [`Row`].
pub trait RowExt {
    fn get(&self, key: &str) -> Option<&str>;
    fn contains_key(&self, key: &str) -> bool;
    fn has(&self, key: &str, value: &str) -> bool;
}

impl RowExt for Vec<(String, String)> {
    fn get(&self, key: &str) -> Option<&str> {
        self.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
    fn contains_key(&self, key: &str) -> bool {
        self.iter().any(|(k, _)| k == key)
    }
    fn has(&self, key: &str, value: &str) -> bool {
        self.get(key) == Some(value)
    }
}

/// A parsed or to-be-sent TS3 command.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Command {
    pub name: String,
    /// Positional arguments (`use 1`).
    pub positional: Vec<String>,
    /// Parameter rows; only the first row is used when building a request,
    /// responses may contain many rows (multi-item lists).
    pub params: Vec<Vec<(String, String)>>,
    /// Flag options (`-topic`).
    pub options: Vec<String>,
}

impl Command {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Add a key=value parameter to the first row.
    pub fn param(mut self, key: impl Into<String>, value: impl ToString) -> Self {
        if self.params.is_empty() {
            self.params.push(Vec::new());
        }
        self.params[0].push((key.into(), value.to_string()));
        self
    }

    pub fn opt(mut self, option: impl Into<String>) -> Self {
        self.options.push(option.into());
        self
    }

    pub fn pos(mut self, value: impl ToString) -> Self {
        self.positional.push(value.to_string());
        self
    }

    pub fn is_empty_row(&self) -> bool {
        self.params.first().map_or(true, |r| r.is_empty())
    }

    /// First value for `key` in the first row.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.params.first().and_then(|row| row.get(key))
    }

    pub fn rows(&self) -> &[Row] {
        &self.params
    }

    /// Serialize into a single wire line (no trailing newline). Multi-row
    /// parameters are joined by `|` (`sid=1|sid=2`).
    pub fn serialize(&self) -> String {
        let mut out = String::new();
        out.push_str(&escape(&self.name));
        for p in &self.positional {
            out.push(' ');
            out.push_str(&escape(p));
        }
        for (i, row) in self.params.iter().enumerate() {
            if row.is_empty() {
                continue;
            }
            out.push(if i == 0 { ' ' } else { '|' });
            let mut first = true;
            for (k, v) in row {
                if !first {
                    out.push(' ');
                }
                first = false;
                let _ = write!(out, "{}={}", escape(k), escape(v));
            }
        }
        for o in &self.options {
            out.push_str(" -");
            out.push_str(&escape(o));
        }
        out
    }

    /// Parse a response/notification line (rows split by unescaped `|`).
    /// List responses (`cid=1 pid=0 ...|cid=2 ...`) have no command name.
    pub fn parse(line: &str) -> Result<Self> {
        if line.trim().is_empty() {
            return Err(Error::Parse("empty line".into()));
        }
        let rows = split_unescaped(line, '|');
        let mut cmd = Command::default();
        for (idx, row) in rows.iter().enumerate() {
            let mut params = Vec::new();
            for token in split_unescaped(row, ' ') {
                let token = token.trim_matches('\r');
                if token.is_empty() {
                    continue;
                }
                if idx == 0 && cmd.name.is_empty() && !token.contains('=') {
                    // First bare token is the command name (notifications,
                    // `error`, ...). List responses have no such token.
                    cmd.name = unescape(token.trim_start_matches('-'));
                    continue;
                }
                match token.split_once('=') {
                    Some((k, v)) => params.push((unescape(k), unescape(v))),
                    None => {
                        // Bare option like `-topic`.
                        cmd.options.push(unescape(token.trim_start_matches('-')));
                    }
                }
            }
            cmd.params.push(params);
        }
        Ok(cmd)
    }
}

/// Passwords are sent as `base64(sha1(password))` on both transports.
pub fn hash_password(password: &str) -> String {
    use base64::Engine as _;
    use sha1::{Digest, Sha1};
    let mut hasher = Sha1::new();
    hasher.update(password.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        let cases = [
            ("plain", "plain"),
            ("hello world", "hello\\sworld"),
            ("a|b", "a\\pb"),
            ("a/b", "a\\/b"),
            ("back\\slash", "back\\\\slash"),
            ("tab\there", "tab\\there"),
            ("nl\nhere", "nl\\nhere"),
            ("bell\x07", "bell\\a"),
            ("你好 世界", "你好\\s世界"),
        ];
        for (raw, escaped) in cases {
            assert_eq!(escape(raw), escaped, "escaping {raw:?}");
            assert_eq!(unescape(escaped), raw, "unescaping {escaped:?}");
        }
    }

    #[test]
    fn serialize_command() {
        let cmd = Command::new("channellist")
            .opt("topic")
            .opt("flags")
            .param("cid", 1);
        assert_eq!(cmd.serialize(), "channellist cid=1 -topic -flags");

        let use_cmd = Command::new("use").pos("1").opt("virtual");
        assert_eq!(use_cmd.serialize(), "use 1 -virtual");

        let multi = Command::new("channelcreate")
            .param("channel_name", "a b|c")
            .param("cpid", 0);
        assert_eq!(
            multi.serialize(),
            "channelcreate channel_name=a\\sb\\pc cpid=0"
        );
    }

    #[test]
    fn parse_response_rows() {
        // List responses carry no command name.
        let list = Command::parse("cid=1 pid=0 name=Default\\sServer |cid=2 pid=0 name=Second")
            .unwrap();
        assert_eq!(list.name, "");
        assert_eq!(list.rows().len(), 2);
        assert_eq!(list.rows()[0][0], ("cid".into(), "1".into()));
        assert_eq!(list.rows()[1].first().map(|(k, _)| k.as_str()), Some("cid"));

        // Proper notification-style parse.
        let notify = Command::parse("notifytextmessage targetmode=1 msg=Hello\\sWorld! extra=1")
            .unwrap();
        assert_eq!(notify.name, "notifytextmessage");
        assert_eq!(notify.get("msg"), Some("Hello World!"));
        assert_eq!(notify.get("extra"), Some("1"));

        let err = Command::parse("error id=0 msg=ok").unwrap();
        assert_eq!(err.name, "error");
        assert_eq!(err.get("id"), Some("0"));

        let empty_val = Command::parse("clientlist clid=1 nickname= |clid=2").unwrap();
        assert_eq!(empty_val.get("nickname"), Some(""));
        assert_eq!(empty_val.rows().len(), 2);
    }

    #[test]
    fn parse_errors() {
        assert!(Command::parse("").is_err());
    }

    #[test]
    fn password_hash() {
        // sha1("abc") = a9993e364706816aba3e25717850c26c9cd0d89d
        assert_eq!(hash_password("abc"), "qZk+NkcGgWq6PiVxeFDCbJzQ2J0=");
    }
}
