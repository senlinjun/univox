//! Message content abstraction (FEATURES.md §7.2): a common rich subset
//! mapped per platform, with automatic degradation for superset content.

/// Message content variants.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageContent {
    Plain(String),
    /// Public rich-text subset → mapped to KMarkdown / markdown / TS3
    /// BBCode-ish. Content beyond the platform subset degrades to plain text
    /// (callers detect via [`MessageContent::degraded`]).
    Rich(RichText),
    /// @all or a member list.
    Mentions { all: bool, members: Vec<crate::id::MemberId>, prefix: Option<String> },
    /// Quote/reply (KOOK/OOPZ; TS3 ✗).
    Reference {
        reference: crate::id::MessageId,
        content: Box<MessageContent>,
    },
    /// File/image attachments (platform upload channels, §7.2).
    Attachments { files: Vec<Attachment>, caption: Option<String> },
}

impl MessageContent {
    /// Content that lost information when degraded to plain text.
    pub fn degraded(&self) -> bool {
        matches!(self, MessageContent::Rich(r) if r.degrades)
    }

    /// Plain-text rendering (fallback for platforms without the feature).
    pub fn plain_text(&self) -> String {
        match self {
            MessageContent::Plain(s) => s.clone(),
            MessageContent::Rich(r) => r.to_plain(),
            MessageContent::Mentions { all, members, prefix } => {
                let prefix = prefix.clone().unwrap_or_default();
                if *all {
                    format!("{prefix}@all")
                } else {
                    members.iter().map(|m| format!("{prefix}@{m}")).collect::<Vec<_>>().join(" ")
                }
            }
            MessageContent::Reference { reference, content } => {
                format!("[reply to {}] {}", reference, content.plain_text())
            }
            MessageContent::Attachments { caption, .. } => {
                caption.clone().unwrap_or_default()
            }
        }
    }
}

/// Rich text spans (FEATURES.md §7.2 公共富文本子集).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RichText {
    pub spans: Vec<Span>,
    /// Set when content exceeded the target platform's subset and was
    /// downgraded during mapping.
    pub degrades: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Span {
    Text(String),
    Bold(String),
    Italic(String),
    Underline(String),
    Link { url: String, label: Option<String> },
    InlineCode(String),
    CodeBlock { lang: Option<String>, code: String },
    Quote(String),
    Colored { color: String, text: String },
}

impl RichText {
    pub fn push(&mut self, span: Span) {
        self.spans.push(span);
    }

    pub fn to_plain(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            match span {
                Span::Text(s)
                | Span::Bold(s)
                | Span::Italic(s)
                | Span::Underline(s)
                | Span::InlineCode(s)
                | Span::Quote(s) => out.push_str(s),
                Span::Link { label, .. } => {
                    out.push_str(label.as_deref().unwrap_or("(link)"))
                }
                Span::CodeBlock { code, .. } => out.push_str(code),
                Span::Colored { text, .. } => out.push_str(text),
            }
        }
        out
    }

    /// TS3 BBCode-style rendering (b/i/u/url/code/quote), the subset the
    /// official client renders in chat.
    pub fn to_ts3_bbcode(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            match span {
                Span::Text(s) => out.push_str(s),
                Span::Bold(s) => out.push_str(&format!("[B]{s}[/B]")),
                Span::Italic(s) => out.push_str(&format!("[I]{s}[/I]")),
                Span::Underline(s) => out.push_str(&format!("[U]{s}[/U]")),
                Span::Link { url, label } => match label {
                    Some(l) => out.push_str(&format!("[URL={url}]{l}[/URL]")),
                    None => out.push_str(&format!("[URL]{url}[/URL]")),
                },
                Span::InlineCode(s) => out.push_str(&format!("[CODE]{s}[/CODE]")),
                Span::CodeBlock { code, .. } => out.push_str(&format!("[CODE]{code}[/CODE]")),
                Span::Quote(s) => out.push_str(&format!("[QUOTE]{s}[/QUOTE]")),
                Span::Colored { text, .. } => {
                    // TS3 has no color: degrade to plain.
                    out.push_str(text);
                }
            }
        }
        out
    }

    /// KOOK KMarkdown rendering (subset).
    pub fn to_kmarkdown(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            match span {
                Span::Text(s) => out.push_str(s),
                Span::Bold(s) => out.push_str(&format!("**{s}**")),
                Span::Italic(s) => out.push_str(&format!("*{s}*")),
                Span::Underline(s) => out.push_str(&format!("(ins){s}(ins)")),
                Span::Link { url, label } => match label {
                    Some(l) => out.push_str(&format!("[{l}]({url})")),
                    None => out.push_str(url),
                },
                Span::InlineCode(s) => out.push_str(&format!("`{s}`")),
                Span::CodeBlock { lang, code } => match lang {
                    Some(l) => out.push_str(&format!("```{l}\n{code}\n```")),
                    None => out.push_str(&format!("```\n{code}\n```")),
                },
                Span::Quote(s) => out.push_str(&format!("> {s}\n")),
                // KMarkdown has no colored text: degrade to plain.
                Span::Colored { text, .. } => out.push_str(text),
            }
        }
        out
    }
}

/// File attachment (FEATURES.md §7.2 Attachments).
#[derive(Debug, Clone, PartialEq)]
pub struct Attachment {
    pub name: String,
    /// MIME type hint.
    pub mime: Option<String>,
    /// In-memory bytes (uploads route through platform asset channels).
    pub data: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbcode_and_kmarkdown_rendering() {
        let mut rich = RichText::default();
        rich.push(Span::Text("hello ".into()));
        rich.push(Span::Bold("world".into()));
        rich.push(Span::Link {
            url: "https://example.com".into(),
            label: Some("link".into()),
        });
        assert_eq!(rich.to_plain(), "hello worldlink");
        assert_eq!(rich.to_ts3_bbcode(), "hello [B]world[/B][URL=https://example.com]link[/URL]");
        assert_eq!(rich.to_kmarkdown(), "hello **world**[link](https://example.com)");
        assert!(!rich.degrades);
    }

    #[test]
    fn mentions_plain() {
        let m = MessageContent::Mentions {
            all: true,
            members: vec![],
            prefix: None,
        };
        assert_eq!(m.plain_text(), "@all");
    }
}
