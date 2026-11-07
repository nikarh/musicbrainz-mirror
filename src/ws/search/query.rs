//! The Lucene query syntax of MusicBrainz's search, parsed into a tree: terms, `"phrases"`, `field:value`,
//! `field:(groups)`, `+must`, `-must_not`, `AND` / `OR` / `NOT` / `&&` / `||` / `!`, `[a TO b]` ranges, `*`
//! prefixes, escapes. Boosts (`^2`) and fuzziness (`~`) are accepted and ignored.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Occur {
    Should,
    Must,
    MustNot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Term {
        field: Option<String>,
        text: String,
        prefix: bool,
    },
    Phrase {
        field: Option<String>,
        text: String,
    },
    Range {
        field: Option<String>,
        from: Option<String>,
        to: Option<String>,
    },
    Group {
        field: Option<String>,
        children: Vec<(Occur, Node)>,
    },
}

impl Node {
    #[must_use]
    pub fn field(&self) -> Option<&str> {
        match self {
            Self::Term { field, .. }
            | Self::Phrase { field, .. }
            | Self::Range { field, .. }
            | Self::Group { field, .. } => field.as_deref(),
        }
    }

    /// The words of a term, phrase or group, for fields compared as a whole (ids, codes, names).
    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Self::Term { text, .. } | Self::Phrase { text, .. } => text.clone(),
            Self::Range { from, to, .. } => format!(
                "{} {}",
                from.as_deref().unwrap_or(""),
                to.as_deref().unwrap_or("")
            ),
            Self::Group { children, .. } => children
                .iter()
                .map(|(_, n)| n.text())
                .filter(|t| !t.is_empty())
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    Colon,
    Plus,
    Minus,
    Not,
    And,
    Or,
    Word(String, bool),
    Phrase(String),
    Range(Option<String>, Option<String>),
}

#[allow(clippy::too_many_lines)]
fn tokenize(query: &str) -> Vec<Token> {
    let chars: Vec<char> = query.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while let Some(&c) = chars.get(i) {
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                tokens.push(Token::Open);
                i += 1;
            }
            ')' => {
                tokens.push(Token::Close);
                i += 1;
            }
            ':' => {
                tokens.push(Token::Colon);
                i += 1;
            }
            '+' => {
                tokens.push(Token::Plus);
                i += 1;
            }
            '-' => {
                tokens.push(Token::Minus);
                i += 1;
            }
            '!' => {
                tokens.push(Token::Not);
                i += 1;
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                tokens.push(Token::And);
                i += 2;
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                tokens.push(Token::Or);
                i += 2;
            }
            '"' => {
                let mut text = String::new();
                i += 1;
                while let Some(&c) = chars.get(i) {
                    i += 1;
                    match c {
                        '\\' => {
                            if let Some(&n) = chars.get(i) {
                                text.push(n);
                                i += 1;
                            }
                        }
                        '"' => break,
                        c => text.push(c),
                    }
                }
                tokens.push(Token::Phrase(text));
                skip_modifiers(&chars, &mut i);
            }
            '[' | '{' => {
                let mut body = String::new();
                i += 1;
                while let Some(&c) = chars.get(i) {
                    i += 1;
                    if c == ']' || c == '}' {
                        break;
                    }
                    body.push(c);
                }
                let mut parts = body.splitn(2, " TO ");
                let bound = |s: Option<&str>| {
                    s.map(str::trim)
                        .filter(|s| !s.is_empty() && *s != "*")
                        .map(|s| s.trim_matches('"').to_owned())
                };
                let from = bound(parts.next());
                let to = bound(parts.next());
                tokens.push(Token::Range(from, to));
            }
            _ => {
                let mut text = String::new();
                while let Some(&c) = chars.get(i) {
                    if c.is_whitespace() || "():\"^~[]{}".contains(c) {
                        break;
                    }
                    if (c == '+' || c == '-' || c == '!') && text.is_empty() {
                        break;
                    }
                    i += 1;
                    if c == '\\' {
                        if let Some(&n) = chars.get(i) {
                            text.push(n);
                            i += 1;
                        }
                    } else {
                        text.push(c);
                    }
                }
                skip_modifiers(&chars, &mut i);
                match text.as_str() {
                    "AND" => tokens.push(Token::And),
                    "OR" => tokens.push(Token::Or),
                    "NOT" => tokens.push(Token::Not),
                    "" => {}
                    _ => {
                        let prefix = text.ends_with('*');
                        let text = text.trim_end_matches(['*', '?']).to_owned();
                        tokens.push(Token::Word(text, prefix));
                    }
                }
            }
        }
    }
    tokens
}

/// Skips `^boost` and `~fuzziness` after a term.
fn skip_modifiers(chars: &[char], i: &mut usize) {
    while let Some(&c) = chars.get(*i) {
        if c != '^' && c != '~' {
            break;
        }
        *i += 1;
        while chars
            .get(*i)
            .is_some_and(|c| c.is_ascii_digit() || *c == '.')
        {
            *i += 1;
        }
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    /// Clauses until `)` or the end.
    fn clauses(&mut self, field: Option<&str>) -> Vec<(Occur, Node)> {
        let mut out: Vec<(Occur, Node)> = Vec::new();
        let mut pending_and = false;
        loop {
            match self.peek() {
                None => break,
                Some(Token::Close) => {
                    self.pos += 1;
                    break;
                }
                Some(Token::And) => {
                    self.pos += 1;
                    if let Some(last) = out.last_mut()
                        && last.0 == Occur::Should
                    {
                        last.0 = Occur::Must;
                    }
                    pending_and = true;
                    continue;
                }
                Some(Token::Or) => {
                    self.pos += 1;
                    continue;
                }
                _ => {}
            }
            let mut occur = if pending_and {
                Occur::Must
            } else {
                Occur::Should
            };
            pending_and = false;
            match self.peek() {
                Some(Token::Plus) => {
                    self.pos += 1;
                    occur = Occur::Must;
                }
                Some(Token::Minus | Token::Not) => {
                    self.pos += 1;
                    occur = Occur::MustNot;
                }
                _ => {}
            }
            if let Some(node) = self.clause(field) {
                out.push((occur, node));
            }
        }
        out
    }

    fn clause(&mut self, field: Option<&str>) -> Option<Node> {
        // field:value
        if let (Some(Token::Word(name, false)), Some(Token::Colon)) = (
            self.tokens.get(self.pos).cloned(),
            self.tokens.get(self.pos + 1),
        ) {
            self.pos += 2;
            let field = name.to_lowercase();
            return self.atom(Some(&field));
        }
        self.atom(field)
    }

    fn atom(&mut self, field: Option<&str>) -> Option<Node> {
        let field_owned = field.map(str::to_owned);
        match self.next()? {
            Token::Open => Some(Node::Group {
                field: field_owned,
                children: self.clauses(field),
            }),
            Token::Phrase(text) => Some(Node::Phrase {
                field: field_owned,
                text,
            }),
            Token::Range(from, to) => Some(Node::Range {
                field: field_owned,
                from,
                to,
            }),
            Token::Word(text, prefix) => Some(Node::Term {
                field: field_owned,
                text,
                prefix,
            }),
            // stray operators
            _ => None,
        }
    }
}

/// Parses a query; the top level is a group without a field.
#[must_use]
pub fn parse(query: &str) -> Vec<(Occur, Node)> {
    let mut parser = Parser {
        tokens: tokenize(query),
        pos: 0,
    };
    let mut out = Vec::new();
    while parser.peek().is_some() {
        out.extend(parser.clauses(None));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn term(field: Option<&str>, text: &str) -> Node {
        Node::Term {
            field: field.map(str::to_owned),
            text: text.into(),
            prefix: false,
        }
    }

    #[test]
    fn fields_groups_and_operators() {
        let q = parse(
            r#"release:(the downward\:spiral) AND artist:"nine inch nails" -status:bootleg tracks:14"#,
        );
        assert_eq!(q.len(), 4);
        assert_eq!(q[0].0, Occur::Must);
        assert_eq!(
            q[0].1,
            Node::Group {
                field: Some("release".into()),
                children: vec![
                    (Occur::Should, term(Some("release"), "the")),
                    (Occur::Should, term(Some("release"), "downward:spiral")),
                ]
            }
        );
        assert_eq!(
            q[1],
            (
                Occur::Must,
                Node::Phrase {
                    field: Some("artist".into()),
                    text: "nine inch nails".into()
                }
            )
        );
        assert_eq!(q[2], (Occur::MustNot, term(Some("status"), "bootleg")));
        assert_eq!(q[3], (Occur::Should, term(Some("tracks"), "14")));
    }

    #[test]
    fn ranges_prefixes_boosts() {
        let q = parse("date:[1990 TO *] closer* love^2 hate~0.8 +x");
        assert_eq!(
            q[0].1,
            Node::Range {
                field: Some("date".into()),
                from: Some("1990".into()),
                to: None
            }
        );
        assert_eq!(
            q[1].1,
            Node::Term {
                field: None,
                text: "closer".into(),
                prefix: true
            }
        );
        assert_eq!(q[2].1, term(None, "love"));
        assert_eq!(q[3].1, term(None, "hate"));
        assert_eq!(q[4].0, Occur::Must);
    }

    #[test]
    fn junk_does_not_panic() {
        for q in [
            "",
            "(((",
            ")))",
            "a:",
            ":b",
            "\"open",
            "[1 TO",
            "AND OR NOT",
            "\\",
            "a:(b:(c))",
            "++--!!",
        ] {
            let _ = parse(q);
        }
    }
}
