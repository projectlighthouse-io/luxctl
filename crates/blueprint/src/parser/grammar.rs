use super::ast::*;
use super::error::ParseError;
use super::lexer::{tokenize, LocatedToken, Token};

/// parse a .bp file into an AST
pub fn parse(input: &str) -> Result<Ast, ParseError> {
    let tokens = tokenize(input)?;
    let mut parser = Parser::new(tokens);
    parser.parse_blueprint()
}

struct Parser {
    tokens: Vec<LocatedToken>,
    pos: usize,
}

impl Parser {
    fn new(tokens: Vec<LocatedToken>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn current(&self) -> Option<&LocatedToken> {
        self.tokens.get(self.pos)
    }

    fn peek_token(&self) -> Option<&Token> {
        self.current().map(|t| &t.value)
    }

    fn current_line(&self) -> usize {
        self.current().map_or(0, |t| t.line)
    }

    fn advance(&mut self) -> Option<&LocatedToken> {
        if self.pos < self.tokens.len() {
            let tok = &self.tokens[self.pos];
            self.pos += 1;
            Some(tok)
        } else {
            None
        }
    }

    fn skip_newlines(&mut self) {
        while let Some(Token::Newline) = self.peek_token() {
            self.pos += 1;
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn parse_blueprint(&mut self) -> Result<Ast, ParseError> {
        self.skip_newlines();

        let line = self.current_line();
        match self.peek_token() {
            Some(Token::Ident(s)) if s == "blueprint" => {
                self.advance();
            }
            _ => return Err(ParseError::new("expected 'blueprint' keyword", line, 1)),
        }

        let name = self.expect_string_value()?;
        self.expect_token_type("{")?;
        let items = self.parse_block_items()?;
        self.expect_token_type("}")?;

        Ok(Ast {
            blueprint: BlueprintBlock { name, items },
        })
    }

    fn expect_string_value(&mut self) -> Result<String, ParseError> {
        self.skip_newlines();
        let line = self.current_line();
        match self.advance() {
            Some(LocatedToken {
                value: Token::QuotedString(s),
                ..
            }) => Ok(s.clone()),
            Some(LocatedToken {
                value: Token::SingleQuotedString(s),
                ..
            }) => Ok(s.clone()),
            Some(LocatedToken {
                value: Token::Ident(s),
                ..
            }) => Ok(s.clone()),
            _ => Err(ParseError::new("expected string value", line, 1)),
        }
    }

    fn expect_token_type(&mut self, expected: &str) -> Result<(), ParseError> {
        self.skip_newlines();
        let line = self.current_line();
        let col = self.current().map_or(1, |t| t.col);
        match self.advance() {
            Some(tok) => {
                let ok = match (&tok.value, expected) {
                    (Token::OpenBrace, "{") => true,
                    (Token::CloseBrace, "}") => true,
                    (Token::Colon, ":") => true,
                    _ => false,
                };
                if ok {
                    Ok(())
                } else {
                    Err(ParseError::new(
                        format!("expected '{expected}', got {:?}", tok.value),
                        line,
                        col,
                    ))
                }
            }
            None => Err(ParseError::new(
                format!("expected '{expected}', got end of input"),
                line,
                col,
            )),
        }
    }

    fn parse_block_items(&mut self) -> Result<Vec<AstItem>, ParseError> {
        let mut items = Vec::new();

        loop {
            self.skip_newlines();

            if self.at_end() {
                break;
            }

            if matches!(self.peek_token(), Some(Token::CloseBrace)) {
                break;
            }

            let item = self.parse_item()?;
            items.push(item);
        }

        Ok(items)
    }

    fn parse_item(&mut self) -> Result<AstItem, ParseError> {
        self.skip_newlines();
        let line = self.current_line();

        if matches!(self.peek_token(), Some(Token::Dash)) {
            return self.parse_list_item();
        }

        let first_token = match self.peek_token() {
            Some(t) => t.clone(),
            None => return Err(ParseError::new("unexpected end of input", line, 1)),
        };

        match &first_token {
            Token::Ident(ident) => {
                if self.is_block_keyword(ident) {
                    return self.parse_block();
                }

                if ident == "probe" || ident == "capture" {
                    return self.parse_raw_line();
                }

                let next = self.look_ahead_skip_strings();
                match next {
                    Some(Token::Colon) => self.parse_property(),
                    Some(Token::OpenBrace) => self.parse_block(),
                    // operator keywords and comparison tokens signal a property
                    Some(Token::Ident(ref s)) if is_operator_keyword(s) => self.parse_property(),
                    Some(Token::Gt | Token::Lt | Token::Gte | Token::Lte) => self.parse_property(),
                    _ => self.parse_raw_line(),
                }
            }
            Token::Variable(_) => self.parse_property(),
            _ => self.parse_raw_line(),
        }
    }

    fn is_block_keyword(&self, ident: &str) -> bool {
        matches!(
            ident,
            "phase"
                | "step"
                | "config"
                | "expect"
                | "input"
                | "hints"
                | "hint"
                | "features"
                | "headers"
                | "prepare"
                | "teardown"
                | "blueprint"
        )
    }

    fn look_ahead_skip_strings(&self) -> Option<Token> {
        let mut p = self.pos + 1;
        while p < self.tokens.len() && matches!(self.tokens[p].value, Token::Newline) {
            p += 1;
        }
        if p < self.tokens.len()
            && matches!(
                self.tokens[p].value,
                Token::QuotedString(_) | Token::SingleQuotedString(_)
            )
        {
            p += 1;
            while p < self.tokens.len() && matches!(self.tokens[p].value, Token::Newline) {
                p += 1;
            }
        }
        self.tokens.get(p).map(|t| t.value.clone())
    }

    fn parse_block(&mut self) -> Result<AstItem, ParseError> {
        let line = self.current_line();
        let block_type = match self.advance() {
            Some(LocatedToken {
                value: Token::Ident(s),
                ..
            }) => s.clone(),
            _ => return Err(ParseError::new("expected block type identifier", line, 1)),
        };

        self.skip_newlines();
        let name = match self.peek_token() {
            Some(Token::QuotedString(_) | Token::SingleQuotedString(_)) => {
                Some(self.expect_string_value()?)
            }
            _ => None,
        };

        self.expect_token_type("{")?;
        let items = self.parse_block_items()?;
        self.expect_token_type("}")?;

        Ok(AstItem::Block(Block {
            block_type,
            name,
            items,
            line,
        }))
    }

    fn parse_property(&mut self) -> Result<AstItem, ParseError> {
        let line = self.current_line();

        let key = match self.advance() {
            Some(LocatedToken {
                value: Token::Ident(s),
                ..
            }) => s.clone(),
            Some(LocatedToken {
                value: Token::Variable(s),
                ..
            }) => format!("${s}"),
            _ => return Err(ParseError::new("expected property key", line, 1)),
        };

        let next = self.peek_token().cloned();

        match next {
            Some(Token::Colon) => {
                self.advance();
                let value = self.parse_property_value()?;
                Ok(AstItem::Property(Property { key, value, line }))
            }
            Some(Token::Ident(ref op)) if is_operator_keyword(op) => {
                let op_word = op.clone();
                self.advance();

                if &op_word == "present" || &op_word == "absent" {
                    let combined_key = format!("{key} {op_word}");
                    Ok(AstItem::Property(Property {
                        key: combined_key,
                        value: PropertyValue::Bool(true),
                        line,
                    }))
                } else {
                    self.expect_token_type(":")?;
                    let value = self.parse_property_value()?;
                    let combined_key = format!("{key} {op_word}");
                    Ok(AstItem::Property(Property {
                        key: combined_key,
                        value,
                        line,
                    }))
                }
            }
            Some(Token::Gt) => {
                self.advance();
                let value = self.parse_property_value()?;
                Ok(AstItem::Property(Property {
                    key: format!("{key} >"),
                    value,
                    line,
                }))
            }
            Some(Token::Lt) => {
                self.advance();
                let value = self.parse_property_value()?;
                Ok(AstItem::Property(Property {
                    key: format!("{key} <"),
                    value,
                    line,
                }))
            }
            Some(Token::Gte) => {
                self.advance();
                let value = self.parse_property_value()?;
                Ok(AstItem::Property(Property {
                    key: format!("{key} >="),
                    value,
                    line,
                }))
            }
            Some(Token::Lte) => {
                self.advance();
                let value = self.parse_property_value()?;
                Ok(AstItem::Property(Property {
                    key: format!("{key} <="),
                    value,
                    line,
                }))
            }
            _ => {
                let value = self.collect_rest_of_line_as_string();
                if value.is_empty() {
                    Ok(AstItem::Property(Property {
                        key,
                        value: PropertyValue::Bool(true),
                        line,
                    }))
                } else {
                    Ok(AstItem::Line(RawLine {
                        content: format!("{key} {value}"),
                        line,
                    }))
                }
            }
        }
    }

    fn parse_property_value(&mut self) -> Result<PropertyValue, ParseError> {
        let line = self.current_line();

        match self.peek_token() {
            Some(Token::Pipe) => {
                self.advance();
                self.parse_multiline_string()
            }
            Some(Token::QuotedString(_)) => {
                if let Some(LocatedToken {
                    value: Token::QuotedString(s),
                    ..
                }) = self.advance()
                {
                    Ok(PropertyValue::String(s.clone()))
                } else {
                    Err(ParseError::new("expected string value", line, 1))
                }
            }
            Some(Token::SingleQuotedString(_)) => {
                if let Some(LocatedToken {
                    value: Token::SingleQuotedString(s),
                    ..
                }) = self.advance()
                {
                    Ok(PropertyValue::String(s.clone()))
                } else {
                    Err(ParseError::new("expected string value", line, 1))
                }
            }
            Some(Token::Int(n)) => {
                let n = *n;
                self.advance();
                Ok(PropertyValue::Int(n))
            }
            Some(Token::Float(f)) => {
                let f = *f;
                self.advance();
                Ok(PropertyValue::Float(f))
            }
            Some(Token::Bool(b)) => {
                let b = *b;
                self.advance();
                Ok(PropertyValue::Bool(b))
            }
            Some(Token::Regex(_)) => {
                if let Some(LocatedToken {
                    value: Token::Regex(s),
                    ..
                }) = self.advance()
                {
                    Ok(PropertyValue::String(format!("/{s}/")))
                } else {
                    Err(ParseError::new("expected regex value", line, 1))
                }
            }
            Some(Token::Variable(_)) => {
                if let Some(LocatedToken {
                    value: Token::Variable(s),
                    ..
                }) = self.advance()
                {
                    Ok(PropertyValue::String(format!("${s}")))
                } else {
                    Err(ParseError::new("expected variable value", line, 1))
                }
            }
            _ => {
                let rest = self.collect_rest_of_line_as_string();
                if rest.is_empty() {
                    Err(ParseError::new("expected property value", line, 1))
                } else {
                    Ok(PropertyValue::String(rest))
                }
            }
        }
    }

    fn parse_multiline_string(&mut self) -> Result<PropertyValue, ParseError> {
        let mut lines = Vec::new();
        self.skip_newlines();

        loop {
            if self.at_end() {
                break;
            }
            match self.peek_token() {
                Some(Token::CloseBrace) => break,
                Some(Token::Newline) => {
                    lines.push(String::new());
                    self.advance();
                }
                _ => {
                    if self.is_start_of_property_or_block() {
                        break;
                    }
                    let line_content = self.collect_rest_of_line_as_string();
                    lines.push(line_content);
                }
            }
        }

        while lines.last().map_or(false, |l| l.is_empty()) {
            lines.pop();
        }

        Ok(PropertyValue::MultiLine(lines.join("\n")))
    }

    fn is_start_of_property_or_block(&self) -> bool {
        if let Some(Token::Ident(ident)) = self.peek_token() {
            if self.is_block_keyword(ident) {
                return true;
            }
            let next = self.look_ahead_skip_strings();
            matches!(next, Some(Token::Colon))
        } else {
            false
        }
    }

    fn parse_raw_line(&mut self) -> Result<AstItem, ParseError> {
        let line = self.current_line();
        let content = self.collect_rest_of_line_including_current();
        Ok(AstItem::Line(RawLine { content, line }))
    }

    fn parse_list_item(&mut self) -> Result<AstItem, ParseError> {
        let line = self.current_line();
        self.advance(); // consume dash
        let content = self.collect_rest_of_line_as_string();
        Ok(AstItem::Line(RawLine {
            content: format!("- {content}"),
            line,
        }))
    }

    fn collect_rest_of_line_as_string(&mut self) -> String {
        let mut parts = Vec::new();
        let current_line = self.current_line();

        while let Some(tok) = self.current() {
            if matches!(tok.value, Token::Newline) || tok.line != current_line {
                break;
            }
            // stop at close brace — it belongs to the enclosing block
            if matches!(tok.value, Token::CloseBrace) {
                break;
            }
            // stop at open brace — might be the start of a sub-block
            if matches!(tok.value, Token::OpenBrace) {
                if parts.is_empty() {
                    // avoid infinite loop: consume lone brace as content
                    parts.push(token_to_string(&tok.value));
                    self.pos += 1;
                    continue;
                }
                // if the last collected word is a block keyword (e.g. "expect"),
                // rewind so the block parser can handle it
                if let Some(last) = parts.last() {
                    if is_block_kw(last) {
                        parts.pop();
                        self.pos -= 1;
                    }
                }
                break;
            }
            parts.push(token_to_string(&tok.value));
            self.pos += 1;
        }

        parts.join(" ")
    }

    fn collect_rest_of_line_including_current(&mut self) -> String {
        self.collect_rest_of_line_as_string()
    }
}

fn is_block_kw(s: &str) -> bool {
    matches!(
        s,
        "phase"
            | "step"
            | "config"
            | "expect"
            | "input"
            | "hints"
            | "hint"
            | "features"
            | "headers"
            | "prepare"
            | "teardown"
            | "blueprint"
    )
}

fn is_operator_keyword(s: &str) -> bool {
    matches!(
        s,
        "contains" | "starts-with" | "matches" | "matches-file" | "present" | "absent"
    )
}

/// Put back the escapes the lexer took out of a double-quoted string.
///
/// `collect_rest_of_line_as_string` rebuilds a probe line out of its tokens,
/// and by then `read_double_quoted_string` has already turned `\"` into `"`
/// and `\t` into a tab. Re-wrapping without re-escaping does not give back
/// the line the author wrote: the quotes that were *content* become
/// indistinguishable from the ones that were *delimiters*, and `shell_split`
/// drops them. That is how `awk -F: '{print $1 \"\t\" $6}'` reached the shell
/// as `{print $1 <tab> $6}`, which concatenates the two fields instead of
/// separating them.
fn escape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

fn token_to_string(tok: &Token) -> String {
    match tok {
        Token::Ident(s) => s.clone(),
        Token::QuotedString(s) => format!("\"{}\"", escape_double_quoted(s)),
        Token::SingleQuotedString(s) => format!("'{s}'"),
        Token::Int(n) => n.to_string(),
        Token::Float(f) => f.to_string(),
        Token::Bool(b) => b.to_string(),
        Token::Regex(s) => format!("/{s}/"),
        Token::Colon => ":".to_string(),
        Token::OpenBrace => "{".to_string(),
        Token::CloseBrace => "}".to_string(),
        Token::Pipe => "|".to_string(),
        Token::Dash => "-".to_string(),
        Token::Variable(s) => format!("${s}"),
        Token::Gt => ">".to_string(),
        Token::Lt => "<".to_string(),
        Token::Gte => ">=".to_string(),
        Token::Lte => "<=".to_string(),
        Token::Newline => String::new(),
        Token::Raw(s) => s.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_minimal_blueprint() {
        let input = r#"
blueprint "Test" {
    config {
        timeout: 10s
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("parse failed: {e}"));
        assert_eq!(ast.blueprint.name, "Test");
        assert_eq!(ast.blueprint.items.len(), 1);
    }

    #[test]
    fn test_phase_with_step() {
        let input = r#"
blueprint "Test" {
    phase "basics" {
        step "port is listening" {
            probe tcp 4221
            expect {
                connected: true
            }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("parse failed: {e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => {
                assert_eq!(b.block_type, "phase");
                assert_eq!(b.name.as_deref(), Some("basics"));
                b
            }
            other => panic!("expected phase Block, got {other:?}"),
        };

        let step = match &phase.items[0] {
            AstItem::Block(b) => {
                assert_eq!(b.block_type, "step");
                assert_eq!(b.name.as_deref(), Some("port is listening"));
                b
            }
            other => panic!("expected step Block, got {other:?}"),
        };

        assert_eq!(step.items.len(), 2);
        assert!(matches!(&step.items[0], AstItem::Line(l) if l.content.starts_with("probe")));
        assert!(matches!(&step.items[1], AstItem::Block(b) if b.block_type == "expect"));
    }

    #[test]
    fn test_property_with_operators() {
        let input = r#"
blueprint "Test" {
    phase "test" {
        step "check" {
            probe http GET /
            expect {
                status: 200
                body contains: "hello"
                header.Server present
                body.json.count > 5
                duration < 10s
            }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("parse failed: {e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            other => panic!("expected Block, got {other:?}"),
        };
        let step = match &phase.items[0] {
            AstItem::Block(b) => b,
            other => panic!("expected Block, got {other:?}"),
        };
        let expect = match &step.items[1] {
            AstItem::Block(b) => b,
            other => panic!("expected expect Block, got {other:?}"),
        };

        let props: Vec<&Property> = expect
            .items
            .iter()
            .filter_map(|i| {
                if let AstItem::Property(p) = i {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();

        assert!(props
            .iter()
            .any(|p| p.key == "status" && p.value == PropertyValue::Int(200)));
        assert!(props.iter().any(|p| p.key == "body contains"));
        assert!(props.iter().any(|p| p.key == "header.Server present"));
        assert!(props.iter().any(|p| p.key == "body.json.count >"));
        assert!(props.iter().any(|p| p.key == "duration <"));
    }

    #[test]
    fn test_input_block() {
        let input = r#"
blueprint "Test" {
    phase "test" {
        step "confirm id" {
            input {
                container-id: string
            }
            probe docker inspect $container-id
            expect {
                stdout: "created"
            }
        }
    }
}
"#;
        assert!(parse(input).is_ok());
    }

    #[test]
    fn test_depends_on() {
        let input = r#"
blueprint "Test" {
    phase "first" {
        step "s1" {
            probe tcp 4221
            expect { connected: true }
        }
    }
    phase "second" {
        depends-on: "first"
        step "s2" {
            probe tcp 4222
            expect { connected: true }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let phase2 = match &ast.blueprint.items[1] {
            AstItem::Block(b) => b,
            other => panic!("expected Block, got {other:?}"),
        };

        let dep = phase2.items.iter().find_map(|i| {
            if let AstItem::Property(p) = i {
                if p.key == "depends-on" {
                    return Some(p);
                }
            }
            None
        });
        assert!(dep.is_some());
    }

    #[test]
    fn test_capture_line() {
        let input = r#"
blueprint "Test" {
    phase "test" {
        step "capture test" {
            probe docker inspect nginx-1
            expect {
                exit: 0
                capture stdout as $container_id
            }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no phase"),
        };
        let step = match &phase.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no step"),
        };
        let expect = match &step.items[1] {
            AstItem::Block(b) => b,
            _ => panic!("no expect"),
        };

        let has_capture = expect
            .items
            .iter()
            .any(|i| matches!(i, AstItem::Line(l) if l.content.starts_with("capture")));
        assert!(has_capture);
    }

    #[test]
    fn test_container_lifecycle_full() {
        let input = r#"
blueprint "Container Lifecycle" {
    config {
        timeout: 10s
    }

    phase "create" {
        step "container exists in created state" {
            probe docker inspect nginx-1 --format '{{.State.Status}}'
            expect {
                exit: 0
                stdout: "created"
            }
        }

        step "capture container ID" {
            probe docker inspect nginx-1 --format '{{.ID}}'
            expect {
                stdout matches: /^[a-f0-9]{64}$/
                capture stdout as $container_id
            }
        }

        step "confirm container ID" {
            input { container-id: string }
            probe docker inspect nginx-1 --format '{{.ID}}'
            expect {
                capture stdout as $real_id
                $container-id matches: /^[a-f0-9]{64}$/
                $container-id: $real_id
            }
        }
    }

    phase "running" {
        depends-on: "create"
        step "container is running" {
            probe docker inspect nginx-1 --format '{{.State.Status}}'
            expect { stdout: "running" }
        }
    }

    phase "stopped" {
        depends-on: "running"
        step "container is exited" {
            probe docker inspect nginx-1 --format '{{.State.Status}}'
            expect { stdout: "exited" }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ast.blueprint.name, "Container Lifecycle");

        let blocks: Vec<&Block> = ast
            .blueprint
            .items
            .iter()
            .filter_map(|i| {
                if let AstItem::Block(b) = i {
                    Some(b)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(blocks.len(), 4);
        assert_eq!(blocks[0].block_type, "config");
        assert_eq!(blocks[1].name.as_deref(), Some("create"));
        assert_eq!(blocks[2].name.as_deref(), Some("running"));
        assert_eq!(blocks[3].name.as_deref(), Some("stopped"));
    }

    #[test]
    fn test_http_server_blueprint() {
        let input = r#"
blueprint "Build Your Own HTTP Server" {
    config {
        host: localhost
        port: 4221
        timeout: 10s
    }
    phase "tcp foundations" {
        step "port is listening" {
            probe tcp 4221
            expect { connected: true }
        }
    }
    phase "basic routing" {
        depends-on: "tcp foundations"
        step "root returns 200" {
            probe http GET /
            expect { status: 200 }
        }
        step "unknown path returns 404" {
            probe http GET /nonexistent-path-for-testing
            expect { status: 404 }
        }
    }
    phase "echo endpoint" {
        depends-on: "basic routing"
        step "echoes hello" {
            probe http GET /echo/hello
            expect {
                status: 200
                body: "hello"
            }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(ast.blueprint.name, "Build Your Own HTTP Server");
    }

    #[test]
    fn test_step_metadata() {
        let input = r#"
blueprint "Test" {
    phase "test" {
        step "my step" {
            slug: create-container
            description: "Create a container named nginx-1"
            points: 50
            is_free: true
            probe tcp 4221
            expect { connected: true }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no phase"),
        };
        let step = match &phase.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no step"),
        };

        let props: Vec<&Property> = step
            .items
            .iter()
            .filter_map(|i| {
                if let AstItem::Property(p) = i {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();

        assert!(props.iter().any(|p| p.key == "slug"));
        assert!(props
            .iter()
            .any(|p| p.key == "points" && p.value == PropertyValue::Int(50)));
    }

    #[test]
    fn test_hints_block() {
        let input = r#"
blueprint "Test" {
    phase "test" {
        step "my step" {
            hints {
                - "Use net.Listen to create a listener"
            }
            probe tcp 4221
            expect { connected: true }
        }
    }
}
"#;
        assert!(parse(input).is_ok());
    }

    #[test]
    fn test_features_block() {
        let input = r#"
blueprint "HTTP Server" {
    features {
        - "Raw TCP Sockets | Start at the bottom | plug"
        - "Protocol Parsing | Parse by hand | code"
    }
    phase "test" {
        step "s1" {
            probe tcp 4221
            expect { connected: true }
        }
    }
}
"#;
        assert!(parse(input).is_ok());
    }

    #[test]
    fn test_all_operator() {
        let input = r#"
blueprint "Test" {
    phase "concurrent" {
        step "handles 10 concurrent" {
            probe http GET / concurrent 10
            expect { all status: 200 }
        }
    }
}
"#;
        assert!(parse(input).is_ok());
    }

    #[test]
    fn test_headers_block() {
        let input = r#"
blueprint "Test" {
    phase "test" {
        step "user agent" {
            probe http GET /user-agent
            headers { User-Agent: test-agent/1.0 }
            expect {
                status: 200
                body: "test-agent/1.0"
            }
        }
    }
}
"#;
        assert!(parse(input).is_ok());
    }

    #[test]
    fn test_retry_directive() {
        let input = r#"
blueprint "Test" {
    phase "test" {
        step "with retry" {
            timeout: 60s
            requires: $job_id
            probe http GET /jobs/$job_id
            expect {
                status: 200
            }
        }
    }
}
"#;
        assert!(parse(input).is_ok());
    }

    // --- regression tests for brace-stopping, block-keyword rewind, operator dispatch ---

    #[test]
    fn test_inline_config_block() {
        // collect_rest_of_line_as_string must stop at CloseBrace
        // so "10s" isn't consumed as "10s }"
        let input = r#"
blueprint "T" {
    config { timeout: 10s }
    phase "t" {
        step "s" {
            probe tcp 80
            expect { connected: true }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let config = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            other => panic!("expected config Block, got {other:?}"),
        };
        let timeout_prop = config.items.iter().find_map(|i| {
            if let AstItem::Property(p) = i {
                if p.key == "timeout" {
                    return Some(p);
                }
            }
            None
        });
        assert!(timeout_prop.is_some());
        assert_eq!(
            timeout_prop.map(|p| &p.value),
            Some(&PropertyValue::String("10s".into()))
        );
    }

    #[test]
    fn test_inline_step_with_probe_and_expect() {
        // raw line collection must stop before "expect {" block
        // by rewinding the block keyword when it sees OpenBrace
        let input = r#"
blueprint "T" {
    phase "t" {
        step "s" { probe tcp 80 expect { connected: true } }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            other => panic!("expected phase, got {other:?}"),
        };
        let step = match &phase.items[0] {
            AstItem::Block(b) => b,
            other => panic!("expected step, got {other:?}"),
        };
        // step should have a raw line for probe and a block for expect
        let has_probe = step
            .items
            .iter()
            .any(|i| matches!(i, AstItem::Line(l) if l.content.starts_with("probe")));
        let has_expect = step
            .items
            .iter()
            .any(|i| matches!(i, AstItem::Block(b) if b.block_type == "expect"));
        assert!(has_probe, "step must have a probe raw line");
        assert!(has_expect, "step must have an expect block");
    }

    #[test]
    fn test_operator_keyword_dispatch() {
        // "body contains:" must be parsed as a property, not a raw line.
        // the parser dispatch needs to check for operator keywords after ident.
        let input = r#"
blueprint "T" {
    phase "t" {
        step "s" {
            probe exec echo "hello world"
            expect {
                stdout contains: "world"
                stdout starts-with: "hello"
            }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no phase"),
        };
        let step = match &phase.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no step"),
        };
        let expect = step.items.iter().find_map(|i| {
            if let AstItem::Block(b) = i {
                if b.block_type == "expect" {
                    return Some(b);
                }
            }
            None
        });
        let expect = expect.unwrap_or_else(|| panic!("no expect block"));
        let props: Vec<&Property> = expect
            .items
            .iter()
            .filter_map(|i| {
                if let AstItem::Property(p) = i {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        assert!(
            props.iter().any(|p| p.key == "stdout contains"),
            "should parse 'stdout contains:' as property with key 'stdout contains'"
        );
        assert!(
            props.iter().any(|p| p.key == "stdout starts-with"),
            "should parse 'stdout starts-with:' as property with key 'stdout starts-with'"
        );
    }

    #[test]
    fn test_comparison_operator_dispatch() {
        // "duration < 10s" must be parsed as a property, not a raw line
        let input = r#"
blueprint "T" {
    phase "t" {
        step "s" {
            probe exec echo "fast"
            expect {
                exit: 0
                duration < 10s
                body.json.count > 5
                body.json.count >= 1
                body.json.count <= 100
            }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no phase"),
        };
        let step = match &phase.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no step"),
        };
        let expect = step.items.iter().find_map(|i| {
            if let AstItem::Block(b) = i {
                if b.block_type == "expect" {
                    return Some(b);
                }
            }
            None
        });
        let expect = expect.unwrap_or_else(|| panic!("no expect block"));
        let props: Vec<&Property> = expect
            .items
            .iter()
            .filter_map(|i| {
                if let AstItem::Property(p) = i {
                    Some(p)
                } else {
                    None
                }
            })
            .collect();
        assert!(props.iter().any(|p| p.key == "duration <"));
        assert!(props.iter().any(|p| p.key == "body.json.count >"));
        assert!(props.iter().any(|p| p.key == "body.json.count >="));
        assert!(props.iter().any(|p| p.key == "body.json.count <="));
    }

    #[test]
    fn test_url_path_not_regex() {
        // "/echo/hello" in probe line must be tokenized as an ident path, not regex
        let input = r#"
blueprint "T" {
    phase "t" {
        step "s" {
            probe http GET /echo/hello
            expect { status: 200 }
        }
    }
}
"#;
        let ast = parse(input).unwrap_or_else(|e| panic!("{e}"));
        let phase = match &ast.blueprint.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no phase"),
        };
        let step = match &phase.items[0] {
            AstItem::Block(b) => b,
            _ => panic!("no step"),
        };
        let probe_line = step.items.iter().find_map(|i| {
            if let AstItem::Line(l) = i {
                Some(&l.content)
            } else {
                None
            }
        });
        let probe_line = probe_line.unwrap_or_else(|| panic!("no probe line"));
        assert!(
            probe_line.contains("/echo/hello"),
            "probe line should contain /echo/hello as a path, got: {probe_line}"
        );
    }
}
