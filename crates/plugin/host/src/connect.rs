//! Connections in the syntax of FutureSDR's `connect!` macro.
//!
//! ```text
//! src > head > snk              stream ports `output` -> `input`
//! src.out > in.filt.out > snk   named ports: `blk.out` as source,
//!                               `in.blk` / `in.blk.out` further on
//! sel.outputs[1] > snk          indexed ports
//! ctrl | sink                   message ports `out` -> `in`
//! ctrl.cmd | freq.radio         named message ports
//! ```
//!
//! Statements are separated by `;` or line breaks; a line starting or ending
//! with an operator continues the statement. `#` and `//` start comments.
//! `~>` (local-domain stream) is recognised but not supported here.

use std::fmt;

/// Kind of a connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `>`
    Stream,
    /// `|`
    Message,
}

/// One port-to-port connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// Stream or message.
    pub kind: Kind,
    /// Source block.
    pub src: String,
    /// Source port.
    pub src_port: String,
    /// Destination block.
    pub dst: String,
    /// Destination port.
    pub dst_port: String,
    /// Line of the statement, starting at 1.
    pub line: usize,
}

impl fmt::Display for Link {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op = match self.kind {
            Kind::Stream => ">",
            Kind::Message => "|",
        };
        write!(
            f,
            "{}.{} {op} {}.{}",
            self.src, self.src_port, self.dst_port, self.dst
        )
    }
}

/// Everything a connection text says.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Connections {
    /// Connections, in order.
    pub links: Vec<Link>,
    /// Every block named, in order of first mention.
    pub blocks: Vec<String>,
}

/// A syntax error, with its position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// Line, starting at 1.
    pub line: usize,
    /// Column, starting at 1.
    pub column: usize,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}: {}", self.line, self.column, self.message)
    }
}

impl std::error::Error for ParseError {}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Ident(String),
    Index(usize),
    Dot,
    Stream,
    LocalStream,
    Message,
    Semi,
    Newline,
}

#[derive(Debug, Clone)]
struct Token {
    tok: Tok,
    line: usize,
    column: usize,
}

fn tokenize(text: &str) -> Result<Vec<Token>, ParseError> {
    let mut tokens = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let line_no = line_no + 1;
        let chars: Vec<char> = line.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let column = i + 1;
            let push = |tokens: &mut Vec<Token>, tok| {
                tokens.push(Token {
                    tok,
                    line: line_no,
                    column,
                })
            };
            let c = chars[i];
            match c {
                ' ' | '\t' | '\r' => i += 1,
                '#' => break,
                '/' if chars.get(i + 1) == Some(&'/') => break,
                '.' => {
                    push(&mut tokens, Tok::Dot);
                    i += 1;
                }
                '>' => {
                    push(&mut tokens, Tok::Stream);
                    i += 1;
                }
                '~' if chars.get(i + 1) == Some(&'>') => {
                    push(&mut tokens, Tok::LocalStream);
                    i += 2;
                }
                '|' => {
                    push(&mut tokens, Tok::Message);
                    i += 1;
                }
                ';' => {
                    push(&mut tokens, Tok::Semi);
                    i += 1;
                }
                '[' => {
                    let end = chars[i..]
                        .iter()
                        .position(|&c| c == ']')
                        .map(|p| i + p)
                        .ok_or_else(|| ParseError {
                            line: line_no,
                            column,
                            message: "unclosed `[`".into(),
                        })?;
                    let digits: String = chars[i + 1..end].iter().collect();
                    let index = digits.trim().parse().map_err(|_| ParseError {
                        line: line_no,
                        column,
                        message: format!("port index must be a number, got `{digits}`"),
                    })?;
                    push(&mut tokens, Tok::Index(index));
                    i = end + 1;
                }
                c if c.is_alphabetic() || c == '_' => {
                    let start = i;
                    while i < chars.len()
                        && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '#')
                    {
                        i += 1;
                    }
                    let word: String = chars[start..i].iter().collect();
                    let word = word.strip_prefix("r#").unwrap_or(&word).to_string();
                    if word.contains('#') {
                        return Err(ParseError {
                            line: line_no,
                            column,
                            message: format!("invalid name `{word}`"),
                        });
                    }
                    push(&mut tokens, Tok::Ident(word));
                }
                other => {
                    return Err(ParseError {
                        line: line_no,
                        column,
                        message: format!("unexpected `{other}`"),
                    });
                }
            }
        }
        tokens.push(Token {
            tok: Tok::Newline,
            line: line_no,
            column: chars.len() + 1,
        });
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    end: (usize, usize),
}

/// A port name, with its index folded in (`outputs[1]`), as FutureSDR names
/// indexed ports.
struct Port(String);

struct Endpoint {
    block: String,
    input: Option<String>,
    output: Option<String>,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos).map(|t| &t.tok)
    }

    fn here(&self) -> (usize, usize) {
        self.tokens
            .get(self.pos)
            .map(|t| (t.line, t.column))
            .unwrap_or(self.end)
    }

    fn error<T>(&self, message: impl Into<String>) -> Result<T, ParseError> {
        let (line, column) = self.here();
        Err(ParseError {
            line,
            column,
            message: message.into(),
        })
    }

    fn skip_newlines(&mut self) {
        while self.peek() == Some(&Tok::Newline) {
            self.pos += 1;
        }
    }

    fn is_operator(tok: Option<&Tok>) -> bool {
        matches!(tok, Some(Tok::Stream | Tok::LocalStream | Tok::Message))
    }

    /// Next operator of the current statement, looking across line breaks.
    fn operator(&mut self) -> Result<Option<Kind>, ParseError> {
        let save = self.pos;
        self.skip_newlines();
        let kind = match self.peek() {
            Some(Tok::Stream) => Kind::Stream,
            Some(Tok::Message) => Kind::Message,
            Some(Tok::LocalStream) => {
                return self
                    .error("`~>` local-domain connections are not supported in descriptions");
            }
            _ => {
                self.pos = save;
                return Ok(None);
            }
        };
        self.pos += 1;
        self.skip_newlines();
        Ok(Some(kind))
    }

    fn ident(&mut self, what: &str) -> Result<String, ParseError> {
        match self.peek() {
            Some(Tok::Ident(name)) => {
                let name = name.clone();
                self.pos += 1;
                Ok(name)
            }
            _ => self.error(format!("expected {what}")),
        }
    }

    fn port(&mut self, what: &str) -> Result<Port, ParseError> {
        let name = self.ident(what)?;
        if let Some(Tok::Index(i)) = self.peek() {
            let i = *i;
            self.pos += 1;
            return Ok(Port(format!("{name}[{i}]")));
        }
        Ok(Port(name))
    }

    fn eat_dot(&mut self) -> bool {
        if self.peek() == Some(&Tok::Dot) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// `blk` or `blk.out`
    fn source(&mut self) -> Result<Endpoint, ParseError> {
        let block = self.ident("a block name")?;
        let output = if self.eat_dot() {
            Some(self.port("an output port after `.`")?.0)
        } else {
            None
        };
        Ok(Endpoint {
            block,
            input: None,
            output,
        })
    }

    /// `blk`, `in.blk` or `in.blk.out`
    fn endpoint(&mut self) -> Result<Endpoint, ParseError> {
        let first = self.port("a block or input port")?;
        if !self.eat_dot() {
            if first.0.contains('[') {
                return self.error("expected `.block` after an indexed input port");
            }
            return Ok(Endpoint {
                block: first.0,
                input: None,
                output: None,
            });
        }
        let block = self.ident("a block name after `.`")?;
        let output = if self.eat_dot() {
            Some(self.port("an output port after `.`")?.0)
        } else {
            None
        };
        Ok(Endpoint {
            block,
            input: Some(first.0),
            output,
        })
    }

    fn statement(&mut self, out: &mut Connections) -> Result<(), ParseError> {
        let line = self.here().0;
        let mut src = self.source()?;
        mention(out, &src.block);
        while let Some(kind) = self.operator()? {
            let dst = self.endpoint()?;
            mention(out, &dst.block);
            let (default_out, default_in) = match kind {
                Kind::Stream => ("output", "input"),
                Kind::Message => ("out", "in"),
            };
            out.links.push(Link {
                kind,
                src: src.block.clone(),
                src_port: src.output.clone().unwrap_or_else(|| default_out.into()),
                dst: dst.block.clone(),
                dst_port: dst.input.clone().unwrap_or_else(|| default_in.into()),
                line,
            });
            src = dst;
        }
        match self.peek() {
            None | Some(Tok::Semi | Tok::Newline) => Ok(()),
            Some(Tok::Dot) => {
                self.error("unexpected `.`: input ports go before the block (`in.blk`)")
            }
            _ => self.error("expected `>`, `|`, `;` or a line break"),
        }
    }
}

fn mention(out: &mut Connections, block: &str) {
    if !out.blocks.iter().any(|b| b == block) {
        out.blocks.push(block.to_string());
    }
}

/// Parse a connection text.
pub fn parse(text: &str) -> Result<Connections, ParseError> {
    let tokens = tokenize(text)?;
    let end = tokens.last().map(|t| (t.line, t.column)).unwrap_or((1, 1));
    let mut p = Parser {
        tokens,
        pos: 0,
        end,
    };
    let mut out = Connections::default();
    loop {
        while matches!(p.peek(), Some(Tok::Semi | Tok::Newline)) {
            p.pos += 1;
        }
        if p.peek().is_none() {
            break;
        }
        if Parser::is_operator(p.peek()) {
            return p.error("a statement cannot start with an operator");
        }
        p.statement(&mut out)?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn links(text: &str) -> Vec<String> {
        parse(text)
            .unwrap()
            .links
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn default_stream_ports() {
        assert_eq!(
            links("src > head > snk"),
            ["src.output > input.head", "head.output > input.snk"]
        );
    }

    #[test]
    fn named_and_indexed_ports() {
        assert_eq!(
            links("src.out > in.filt.taps > snk; sel.outputs[1] > inputs[0].mix"),
            [
                "src.out > in.filt",
                "filt.taps > input.snk",
                "sel.outputs[1] > inputs[0].mix"
            ]
        );
    }

    #[test]
    fn messages_and_raw_identifiers() {
        assert_eq!(
            links("ctrl | sink\nctrl.cmd | r#in.fwd | log"),
            [
                "ctrl.out | in.sink",
                "ctrl.cmd | in.fwd",
                "fwd.out | in.log"
            ]
        );
    }

    #[test]
    fn statements_span_lines_at_operators() {
        let text = "
            src >
                head   # first half
                > snk  // second half
            other
        ";
        let parsed = parse(text).unwrap();
        assert_eq!(parsed.links.len(), 2);
        assert_eq!(parsed.links[1].line, 2);
        assert_eq!(parsed.blocks, ["src", "head", "snk", "other"]);
    }

    #[test]
    fn middle_endpoints_are_in_block_out() {
        // after an operator, `x.y` is input `x` of block `y`
        assert_eq!(links("a > b.x"), ["a.output > b.x"]);
        assert_eq!(
            links("a > input.b.cmd | y.c"),
            ["a.output > input.b", "b.cmd | y.c"]
        );
    }

    #[test]
    fn errors_have_positions() {
        let err = parse("a > b\nc >> d").unwrap_err();
        assert_eq!((err.line, err.column), (2, 4));
        let err = parse("a ~> b").unwrap_err();
        assert!(err.message.contains("local-domain"), "{err}");
        let err = parse("a > in[0]").unwrap_err();
        assert!(err.message.contains("indexed input"), "{err}");
        let err = parse("a b").unwrap_err();
        assert_eq!((err.line, err.column), (1, 3));
        assert!(parse("> a").is_err());
        assert!(parse("a > ").is_err());
    }
}
