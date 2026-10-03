use super::error::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// identifier or keyword (blueprint, phase, step, probe, expect, etc.)
    Ident(String),
    /// quoted string: "hello world"
    QuotedString(String),
    /// single-quoted string: 'hello world'
    SingleQuotedString(String),
    /// integer literal
    Int(i64),
    /// float literal
    Float(f64),
    /// boolean literal
    Bool(bool),
    /// regex literal: /pattern/
    Regex(String),
    /// colon :
    Colon,
    /// open brace {
    OpenBrace,
    /// close brace }
    CloseBrace,
    /// pipe | (multi-line string marker)
    Pipe,
    /// dash - (list item)
    Dash,
    /// dollar sign + ident: $variable
    Variable(String),
    /// comparison operators
    Gt,
    Lt,
    Gte,
    Lte,
    /// newline
    Newline,
    /// everything else on a line (used for raw content like probe args)
    Raw(String),
}

#[derive(Debug, Clone)]
pub struct Located<T> {
    pub value: T,
    pub line: usize,
    pub col: usize,
}

pub type LocatedToken = Located<Token>;

pub fn tokenize(input: &str) -> Result<Vec<LocatedToken>, ParseError> {
    let mut tokens = Vec::new();
    let lines: Vec<&str> = input.lines().collect();

    // when we see "key: |", we enter raw multiline mode.
    // all subsequent lines indented deeper than the key line
    // are emitted as Raw tokens instead of being tokenized.
    let mut in_multiline = false;
    let mut multiline_base_indent: usize = 0;

    for (line_idx, line) in lines.iter().enumerate() {
        let line_num = line_idx + 1;
        let trimmed = line.trim();

        // skip empty lines and comments
        if trimmed.is_empty() || trimmed.starts_with('#') {
            tokens.push(LocatedToken {
                value: Token::Newline,
                line: line_num,
                col: 1,
            });
            continue;
        }

        // inside a multiline block: emit as raw text if still indented
        if in_multiline {
            let indent = line.len() - line.trim_start().len();
            if indent > multiline_base_indent {
                tokens.push(LocatedToken {
                    value: Token::Raw(trimmed.to_string()),
                    line: line_num,
                    col: indent + 1,
                });
                tokens.push(LocatedToken {
                    value: Token::Newline,
                    line: line_num,
                    col: line.len() + 1,
                });
                continue;
            }
            // back to normal indentation — exit multiline mode
            in_multiline = false;
        }

        let line_tokens = tokenize_line(trimmed, line_num)?;

        // detect "key: |" pattern — last meaningful token is Pipe
        let has_pipe = line_tokens
            .iter()
            .rev()
            .any(|t| matches!(t.value, Token::Pipe));
        if has_pipe {
            in_multiline = true;
            multiline_base_indent = line.len() - line.trim_start().len();
        }

        tokens.extend(line_tokens);

        tokens.push(LocatedToken {
            value: Token::Newline,
            line: line_num,
            col: line.len() + 1,
        });
    }

    Ok(tokens)
}

fn tokenize_line(line: &str, line_num: usize) -> Result<Vec<LocatedToken>, ParseError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    // a probe's arguments are kept as the author wrote them. Tokenizing them
    // loses the text: `{` reads as a block opener, so a JSON body and every
    // modifier after it (`concurrent 4`) vanished, and `?` falls to the Raw
    // arm, so `/search?q=hello` came back as `/search ?q=hello`. Whatever
    // follows the arguments — an inline `expect { ... }`, the enclosing
    // block's `}`, a comment — is tokenized as usual below.
    if let Some(args_start) = probe_args_start(&chars) {
        tokens.push(LocatedToken {
            value: Token::Ident("probe".to_string()),
            line: line_num,
            col: 1,
        });
        let args_end = probe_args_end(&chars, args_start);
        let args: String = chars[args_start..args_end].iter().collect();
        let args = args.trim();
        if !args.is_empty() {
            tokens.push(LocatedToken {
                value: Token::Raw(args.to_string()),
                line: line_num,
                col: args_start + 1,
            });
        }
        i = args_end;
    }

    while i < chars.len() {
        // skip whitespace
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }

        // comment — skip rest of line
        if chars[i] == '#' {
            break;
        }

        let col = i + 1;

        match chars[i] {
            '{' => {
                tokens.push(LocatedToken {
                    value: Token::OpenBrace,
                    line: line_num,
                    col,
                });
                i += 1;
            }
            '}' => {
                tokens.push(LocatedToken {
                    value: Token::CloseBrace,
                    line: line_num,
                    col,
                });
                i += 1;
            }
            '|' => {
                tokens.push(LocatedToken {
                    value: Token::Pipe,
                    line: line_num,
                    col,
                });
                i += 1;
            }
            ':' => {
                tokens.push(LocatedToken {
                    value: Token::Colon,
                    line: line_num,
                    col,
                });
                i += 1;
            }
            '>' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(LocatedToken {
                        value: Token::Gte,
                        line: line_num,
                        col,
                    });
                    i += 2;
                } else {
                    tokens.push(LocatedToken {
                        value: Token::Gt,
                        line: line_num,
                        col,
                    });
                    i += 1;
                }
            }
            '<' => {
                if i + 1 < chars.len() && chars[i + 1] == '=' {
                    tokens.push(LocatedToken {
                        value: Token::Lte,
                        line: line_num,
                        col,
                    });
                    i += 2;
                } else {
                    tokens.push(LocatedToken {
                        value: Token::Lt,
                        line: line_num,
                        col,
                    });
                    i += 1;
                }
            }
            '-' => {
                // could be list item dash or part of identifier (e.g. depends-on)
                // if it's at the start or preceded by whitespace and followed by space, it's a list dash
                if tokens.is_empty() || (i + 1 < chars.len() && chars[i + 1] == ' ') {
                    // check if next non-space is a quote (list item)
                    let rest: String = chars[i + 1..].iter().collect();
                    let rest_trimmed = rest.trim_start();
                    if rest_trimmed.starts_with('"') || rest_trimmed.starts_with('\'') {
                        tokens.push(LocatedToken {
                            value: Token::Dash,
                            line: line_num,
                            col,
                        });
                        i += 1;
                        continue;
                    }
                    // it's a list item with unquoted content
                    tokens.push(LocatedToken {
                        value: Token::Dash,
                        line: line_num,
                        col,
                    });
                    i += 1;
                    continue;
                }
                // otherwise it's part of an ident — fall through to ident parsing
                let word = read_word(&chars, &mut i);
                tokens.push(classify_word(word, line_num, col));
            }
            '"' => {
                let s = read_double_quoted_string(&chars, &mut i, line_num)?;
                tokens.push(LocatedToken {
                    value: Token::QuotedString(s),
                    line: line_num,
                    col,
                });
            }
            '\'' => {
                // treat as string delimiter only if not mid-word (e.g. don't match "doesn't")
                let prev_is_alpha = i > 0 && chars[i - 1].is_alphanumeric();
                if prev_is_alpha {
                    let word = read_word(&chars, &mut i);
                    let combined = format!("'{}", word);
                    tokens.push(LocatedToken {
                        value: Token::Ident(combined),
                        line: line_num,
                        col,
                    });
                } else {
                    let s = read_single_quoted_string(&chars, &mut i, line_num)?;
                    tokens.push(LocatedToken {
                        value: Token::SingleQuotedString(s),
                        line: line_num,
                        col,
                    });
                }
            }
            '/' => {
                // only treat / as regex when in value position (after colon)
                let is_regex_ctx = tokens
                    .last()
                    .map_or(false, |t| matches!(t.value, Token::Colon));
                if is_regex_ctx {
                    let s = read_regex(&chars, &mut i, line_num)?;
                    tokens.push(LocatedToken {
                        value: Token::Regex(s),
                        line: line_num,
                        col,
                    });
                } else {
                    let word = read_word(&chars, &mut i);
                    tokens.push(classify_word(word, line_num, col));
                }
            }
            '$' => {
                i += 1;
                let name = read_ident_chars(&chars, &mut i);
                if name.is_empty() {
                    return Err(ParseError::new(
                        "expected variable name after $",
                        line_num,
                        col,
                    ));
                }
                tokens.push(LocatedToken {
                    value: Token::Variable(name),
                    line: line_num,
                    col,
                });
            }
            _ if chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '.' => {
                let word = read_word(&chars, &mut i);
                tokens.push(classify_word(word, line_num, col));
            }
            _ => {
                // collect remaining chars as raw content
                let remaining: String = chars[i..].iter().collect();
                tokens.push(LocatedToken {
                    value: Token::Raw(remaining.trim().to_string()),
                    line: line_num,
                    col,
                });
                break;
            }
        }
    }

    Ok(tokens)
}

/// Where a probe's arguments begin, if this line is a probe line.
fn probe_args_start(chars: &[char]) -> Option<usize> {
    let keyword = ['p', 'r', 'o', 'b', 'e'];
    let after = chars.get(keyword.len())?;
    (chars.starts_with(&keyword) && after.is_whitespace()).then_some(keyword.len())
}

/// Where a probe's arguments end: at a `#` comment, at a `}` that closes the
/// enclosing block, or at a block keyword opening an inline block
/// (`expect {`). Quotes and the braces of a JSON body are skipped over, so
/// none of those inside them count.
fn probe_args_end(chars: &[char], start: usize) -> usize {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut i = start;

    while let Some(&ch) = chars.get(i) {
        let prev = i.checked_sub(1).and_then(|p| chars.get(p));
        let after_space = i == start || prev.is_some_and(|c| c.is_whitespace());
        if let Some(q) = quote {
            if ch == '\\' && q == '"' {
                i += 1;
            } else if ch == q {
                quote = None;
            }
            i += 1;
            continue;
        }
        match ch {
            '"' => quote = Some('"'),
            // same rule as the tokenizer: `doesn't` is not a quote
            '\'' if !prev.is_some_and(|c| c.is_alphanumeric()) => quote = Some('\''),
            '{' => depth += 1,
            '}' if depth == 0 => return i,
            '}' => depth -= 1,
            '#' if after_space => return i,
            _ if depth == 0 && after_space && ch.is_alphabetic() => {
                let mut end = i;
                while chars
                    .get(end)
                    .is_some_and(|c| c.is_alphanumeric() || *c == '-')
                {
                    end += 1;
                }
                let mut next = end;
                while chars.get(next).is_some_and(|c| c.is_whitespace()) {
                    next += 1;
                }
                let word: String = chars[i..end].iter().collect();
                if chars.get(next) == Some(&'{') && super::grammar::is_block_kw(&word) {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    chars.len()
}

fn read_word(chars: &[char], i: &mut usize) -> String {
    let mut word = String::new();
    while *i < chars.len()
        && (chars[*i].is_alphanumeric()
            || chars[*i] == '_'
            || chars[*i] == '-'
            || chars[*i] == '.'
            || chars[*i] == '/')
    {
        word.push(chars[*i]);
        *i += 1;
    }
    word
}

fn read_ident_chars(chars: &[char], i: &mut usize) -> String {
    let mut name = String::new();
    while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '_' || chars[*i] == '-')
    {
        name.push(chars[*i]);
        *i += 1;
    }
    name
}

fn classify_word(word: String, line: usize, col: usize) -> LocatedToken {
    let value = match word.as_str() {
        "true" => Token::Bool(true),
        "false" => Token::Bool(false),
        _ => {
            // try parsing as int
            if let Ok(n) = word.parse::<i64>() {
                Token::Int(n)
            } else if let Ok(f) = word.parse::<f64>() {
                Token::Float(f)
            } else {
                Token::Ident(word)
            }
        }
    };
    LocatedToken { value, line, col }
}

fn read_double_quoted_string(
    chars: &[char],
    i: &mut usize,
    line: usize,
) -> Result<String, ParseError> {
    *i += 1; // skip opening "
    let mut s = String::new();
    while *i < chars.len() {
        if chars[*i] == '\\' && *i + 1 < chars.len() {
            match chars[*i + 1] {
                'n' => s.push('\n'),
                't' => s.push('\t'),
                '"' => s.push('"'),
                '\\' => s.push('\\'),
                other => {
                    s.push('\\');
                    s.push(other);
                }
            }
            *i += 2;
            continue;
        }
        if chars[*i] == '"' {
            *i += 1;
            return Ok(s);
        }
        s.push(chars[*i]);
        *i += 1;
    }
    Err(ParseError::new("unterminated string", line, *i + 1))
}

fn read_single_quoted_string(
    chars: &[char],
    i: &mut usize,
    line: usize,
) -> Result<String, ParseError> {
    *i += 1; // skip opening '
    let mut s = String::new();
    while *i < chars.len() {
        if chars[*i] == '\'' {
            *i += 1;
            return Ok(s);
        }
        s.push(chars[*i]);
        *i += 1;
    }
    Err(ParseError::new("unterminated string", line, *i + 1))
}

fn read_regex(chars: &[char], i: &mut usize, line: usize) -> Result<String, ParseError> {
    *i += 1; // skip opening /
    let mut s = String::new();
    while *i < chars.len() {
        if chars[*i] == '\\' && *i + 1 < chars.len() && chars[*i + 1] == '/' {
            s.push('/');
            *i += 2;
            continue;
        }
        if chars[*i] == '/' {
            *i += 1;
            return Ok(s);
        }
        s.push(chars[*i]);
        *i += 1;
    }
    Err(ParseError::new("unterminated regex", line, *i + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_tokens() {
        let input = r#"blueprint "Test" {"#;
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let values: Vec<&Token> = tokens.iter().map(|t| &t.value).collect();
        assert!(matches!(values[0], Token::Ident(s) if s == "blueprint"));
        assert!(matches!(values[1], Token::QuotedString(s) if s == "Test"));
        assert!(matches!(values[2], Token::OpenBrace));
    }

    #[test]
    fn test_property_line() {
        let input = "slug: build-http-server";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let values: Vec<&Token> = tokens.iter().map(|t| &t.value).collect();
        assert!(matches!(values[0], Token::Ident(s) if s == "slug"));
        assert!(matches!(values[1], Token::Colon));
        assert!(matches!(values[2], Token::Ident(s) if s == "build-http-server"));
    }

    #[test]
    fn test_variable_token() {
        let input = "$container_id";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        assert!(matches!(&tokens[0].value, Token::Variable(s) if s == "container_id"));
    }

    #[test]
    fn test_regex_token() {
        // regex is only recognized in value position (after colon)
        let input = r"matches: /^[a-f0-9]{64}$/";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let non_nl: Vec<&Token> = tokens
            .iter()
            .filter(|t| !matches!(t.value, Token::Newline))
            .map(|t| &t.value)
            .collect();
        assert!(matches!(non_nl[0], Token::Ident(s) if s == "matches"));
        assert!(matches!(non_nl[1], Token::Colon));
        assert!(matches!(non_nl[2], Token::Regex(s) if s == "^[a-f0-9]{64}$"));
    }

    #[test]
    fn test_slash_as_path() {
        // bare / without preceding colon is treated as a word, not regex
        let input = "GET /echo/hello";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let non_nl: Vec<&Token> = tokens
            .iter()
            .filter(|t| !matches!(t.value, Token::Newline))
            .map(|t| &t.value)
            .collect();
        assert!(matches!(non_nl[0], Token::Ident(s) if s == "GET"));
        assert!(matches!(non_nl[1], Token::Ident(s) if s == "/echo/hello"));
    }

    #[test]
    fn test_comparison_operators() {
        let input = "> < >= <=";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let values: Vec<&Token> = tokens
            .iter()
            .filter(|t| !matches!(t.value, Token::Newline))
            .map(|t| &t.value)
            .collect();
        assert_eq!(values.len(), 4);
        assert!(matches!(values[0], Token::Gt));
        assert!(matches!(values[1], Token::Lt));
        assert!(matches!(values[2], Token::Gte));
        assert!(matches!(values[3], Token::Lte));
    }

    #[test]
    fn test_comment_skipped() {
        let input = "# this is a comment\nslug: test";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let non_newline: Vec<&Token> = tokens
            .iter()
            .filter(|t| !matches!(t.value, Token::Newline))
            .map(|t| &t.value)
            .collect();
        assert!(matches!(non_newline[0], Token::Ident(s) if s == "slug"));
    }

    #[test]
    fn test_numeric_tokens() {
        let input = "port: 4221";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let values: Vec<&Token> = tokens
            .iter()
            .filter(|t| !matches!(t.value, Token::Newline))
            .map(|t| &t.value)
            .collect();
        assert!(matches!(values[2], Token::Int(4221)));
    }

    #[test]
    fn test_bool_tokens() {
        let input = "is_published: true";
        let tokens = tokenize(input).unwrap_or_else(|e| panic!("parse error: {e}"));
        let values: Vec<&Token> = tokens
            .iter()
            .filter(|t| !matches!(t.value, Token::Newline))
            .map(|t| &t.value)
            .collect();
        assert!(matches!(values[2], Token::Bool(true)));
    }
}
