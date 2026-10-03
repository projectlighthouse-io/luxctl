use super::error::TranspileError;
use super::ir::*;
use std::collections::HashMap;

pub fn transpile(ast: &crate::parser::ast::Ast) -> Result<Blueprint, TranspileError> {
    let mut meta = ProjectMeta::default();
    meta.unlock_mode = "sequential".to_string();
    let mut config = Config::default();
    let mut phases = Vec::new();
    let mut requires_env = None;

    for item in &ast.blueprint.items {
        match item {
            crate::parser::ast::AstItem::Block(block) => match block.block_type.as_str() {
                "config" => config = resolve_config(block)?,
                "phase" => phases.push(resolve_phase(block)?),
                "features" => meta.features = resolve_features(block)?,
                "requires" => requires_env = Some(resolve_requires_block(block)),
                _ => {}
            },
            crate::parser::ast::AstItem::Property(prop) => {
                apply_project_meta_property(&mut meta, prop)?;
            }
            crate::parser::ast::AstItem::Line(_) => {}
        }
    }

    Ok(Blueprint {
        name: ast.blueprint.name.clone(),
        meta,
        config,
        phases,
        requires_env,
    })
}

/// resolve a `requires { }` block.
///
/// every value is a plain scalar property — capabilities are a comma-separated
/// string rather than a list literal, which keeps the lexer untouched.
///
/// ```text
/// requires {
///   kernel: ">=6.1"
///   cgroup: "v2"
///   userns: true
///   btf: true
///   cap: "SYS_ADMIN,BPF"
///   host: "linux"
/// }
/// ```
fn resolve_requires_block(block: &crate::parser::ast::Block) -> Requirements {
    let mut reqs = Requirements::default();

    for item in &block.items {
        let prop = match item {
            crate::parser::ast::AstItem::Property(p) => p,
            _ => continue,
        };
        match prop.key.as_str() {
            "kernel" => {
                if let Some(s) = prop.value.as_str() {
                    reqs.kernel = parse_version_triple(s);
                }
            }
            "cgroup" => {
                // accepts `cgroup: "v2"` or `cgroup: 2`
                let v2 = prop
                    .value
                    .as_str()
                    .map(|s| s.trim().trim_start_matches('v') == "2")
                    .unwrap_or_else(|| prop.value.as_i64() == Some(2));
                reqs.cgroup_v2 = v2;
            }
            "userns" => reqs.userns = prop.value.as_bool().unwrap_or(false),
            "btf" => reqs.btf = prop.value.as_bool().unwrap_or(false),
            "privileged" => reqs.privileged = prop.value.as_bool().unwrap_or(false),
            "pid" => reqs.pid_host = prop.value.as_str() == Some("host"),
            "cap" | "caps" => {
                if let Some(s) = prop.value.as_str() {
                    reqs.caps = s
                        .split(',')
                        .map(|c| c.trim().to_uppercase())
                        .filter(|c| !c.is_empty())
                        .collect();
                }
            }
            "host" => reqs.host = prop.value.as_str().map(|s| s.trim().to_lowercase()),
            _ => {}
        }
    }

    reqs
}

/// parse the leading numeric triple out of a version constraint.
///
/// handles `">=6.1"`, `"6.1.0"`, and kernel strings like `"5.15.0-91-generic"`
/// or `"6.1.0-rc1"`, none of which the semver parser in `upgrade.rs` accepts.
pub(crate) fn parse_version_triple(s: &str) -> Option<(u64, u64, u64)> {
    let digits = s
        .trim()
        .trim_start_matches(['>', '=', '<', '^', '~', 'v', ' ']);
    let mut parts = digits.split(['.', '-', '_', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

fn resolve_config(block: &crate::parser::ast::Block) -> Result<Config, TranspileError> {
    let mut config = Config::default();
    for item in &block.items {
        match item {
            crate::parser::ast::AstItem::Property(prop) => match prop.key.as_str() {
                "host" => {
                    config.host = prop
                        .value
                        .as_str()
                        .ok_or_else(|| TranspileError::new("config.host must be a string"))?
                        .to_string();
                }
                "port" => {
                    let n = prop
                        .value
                        .as_i64()
                        .ok_or_else(|| TranspileError::new("config.port must be an integer"))?;
                    config.port = Some(n as u16);
                }
                "timeout" => {
                    let s = prop.value.as_str().ok_or_else(|| {
                        TranspileError::new("config.timeout must be a duration string")
                    })?;
                    config.timeout = parse_duration(s)
                        .ok_or_else(|| TranspileError::new(format!("invalid timeout: {s}")))?;
                }
                "bin" => {
                    config.bin = Some(
                        prop.value
                            .as_str()
                            .ok_or_else(|| TranspileError::new("config.bin must be a string"))?
                            .to_string(),
                    );
                }
                _ => {
                    if let Some(v) = prop.value.as_str() {
                        config.env.insert(prop.key.clone(), v.to_string());
                    }
                }
            },
            crate::parser::ast::AstItem::Block(b) if b.block_type == "bin_path" => {
                for sub in &b.items {
                    if let crate::parser::ast::AstItem::Property(p) = sub {
                        if let Some(v) = p.value.as_str() {
                            config.bin_path.insert(p.key.clone(), v.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(config)
}

fn resolve_phase(block: &crate::parser::ast::Block) -> Result<Phase, TranspileError> {
    let name = block
        .name
        .clone()
        .ok_or_else(|| TranspileError::new("phase must have a name"))?;
    let mut depends_on = Vec::new();
    let mut steps = Vec::new();
    let mut meta = PhaseMeta::default();

    for item in &block.items {
        match item {
            crate::parser::ast::AstItem::Block(b) => match b.block_type.as_str() {
                "step" => steps.push(resolve_step(b)?),
                "hints" => meta.hints = resolve_hints_block(b)?,
                _ => {}
            },
            crate::parser::ast::AstItem::Property(p) => match p.key.as_str() {
                "depends-on" => {
                    if let Some(dep) = p.value.as_str() {
                        depends_on.push(dep.to_string());
                    }
                }
                "title" => meta.title = p.value.as_str().map(|s| s.to_string()),
                "slug" => meta.slug = p.value.as_str().map(|s| s.to_string()),
                "description" => meta.description = p.value.as_str().map(|s| s.to_string()),
                "points" => meta.points = p.value.as_i64().unwrap_or(0) as u32,
                "scores" => meta.scores = p.value.as_str().map(|s| s.to_string()),
                "is_free" => meta.is_free = p.value.as_bool().unwrap_or(false),
                "visibility_level" => meta.visibility_level = p.value.as_i64().unwrap_or(0) as u8,
                "abandoned_deduction" => {
                    meta.abandoned_deduction = p.value.as_i64().unwrap_or(0) as u32
                }
                _ => {}
            },
            _ => {}
        }
    }

    Ok(Phase {
        name,
        meta,
        depends_on,
        steps,
    })
}

fn resolve_step(block: &crate::parser::ast::Block) -> Result<Step, TranspileError> {
    let name = block
        .name
        .clone()
        .ok_or_else(|| TranspileError::new("step must have a name"))?;

    let mut meta = StepMeta::default();
    let mut requires = Vec::new();
    let mut requires_env = None;
    let mut timeout = None;
    let mut retry = None;
    let mut inputs = Vec::new();
    let mut probe = None;
    let mut expectations = Vec::new();
    let mut captures = Vec::new();
    let mut headers = HashMap::new();

    for item in &block.items {
        match item {
            crate::parser::ast::AstItem::Property(prop) => match prop.key.as_str() {
                "slug" => meta.slug = prop.value.as_str().map(|s| s.to_string()),
                "description" => meta.description = prop.value.as_str().map(|s| s.to_string()),
                "requires" => {
                    if let Some(s) = prop.value.as_str() {
                        requires.push(s.to_string());
                    }
                }
                "timeout" => {
                    if let Some(s) = prop.value.as_str() {
                        timeout = parse_duration(s);
                    }
                }
                key if key.starts_with("retry") => {
                    retry = parse_retry_from_property(prop)?;
                }
                _ => {}
            },
            crate::parser::ast::AstItem::Block(b) => match b.block_type.as_str() {
                "expect" => {
                    let (exps, caps) = resolve_expect_block(b)?;
                    expectations = exps;
                    captures = caps;
                }
                "input" => {
                    inputs = resolve_input_block(b)?;
                }
                "headers" => {
                    headers = resolve_headers_block(b)?;
                }
                "requires" => {
                    requires_env = Some(resolve_requires_block(b));
                }
                _ => {}
            },
            crate::parser::ast::AstItem::Line(line) => {
                let trimmed = line.content.trim();
                if trimmed.starts_with("probe") {
                    probe = Some(resolve_probe_line(trimmed)?);
                } else if trimmed.starts_with("retry") {
                    retry = parse_retry_from_line(trimmed)?;
                } else if trimmed.starts_with("timeout") {
                    timeout = parse_timeout_from_line(trimmed);
                } else if trimmed.starts_with("requires") {
                    if let Some(var) = trimmed.strip_prefix("requires") {
                        requires.push(var.trim().trim_start_matches(':').trim().to_string());
                    }
                }
            }
        }
    }

    let mut probe =
        probe.ok_or_else(|| TranspileError::new(format!("step '{name}' has no probe")))?;

    if let Probe::Http(http_probe) = &mut probe {
        http_probe.headers = std::mem::take(&mut headers);
    }

    Ok(Step {
        name,
        meta,
        requires,
        requires_env,
        timeout,
        retry,
        inputs,
        probe,
        expectations,
        captures,
        headers,
    })
}

fn resolve_probe_line(line: &str) -> Result<Probe, TranspileError> {
    let rest = line
        .strip_prefix("probe ")
        .ok_or_else(|| TranspileError::new("expected 'probe' keyword"))?
        .trim();

    if rest.starts_with("tcp ") || rest == "tcp" {
        let port_str = rest.strip_prefix("tcp").unwrap_or("").trim();
        let port: u16 = port_str
            .parse()
            .map_err(|_| TranspileError::new(format!("invalid tcp port: '{port_str}'")))?;
        return Ok(Probe::Tcp(TcpProbe { port }));
    }

    if rest.starts_with("udp ") {
        let addr = rest.strip_prefix("udp").unwrap_or("").trim();
        if let Some((host, port_str)) = addr.rsplit_once(':') {
            let port: u16 = port_str
                .parse()
                .map_err(|_| TranspileError::new(format!("invalid udp port: '{port_str}'")))?;
            return Ok(Probe::Udp(UdpProbe {
                host: host.to_string(),
                port,
            }));
        }
        return Err(TranspileError::new(format!(
            "invalid udp address: '{addr}'"
        )));
    }

    if rest.starts_with("http ") {
        return resolve_http_probe(rest.strip_prefix("http ").unwrap_or("").trim());
    }

    if rest.starts_with("docker ") {
        let args = rest.strip_prefix("docker ").unwrap_or("").trim();
        let parts = shell_split(args);
        return Ok(Probe::Exec(ExecProbe {
            command: "docker".to_string(),
            args: parts,
        }));
    }

    if rest.starts_with("exec ") {
        let exec_rest = rest.strip_prefix("exec ").unwrap_or("").trim();
        let parts = shell_split(exec_rest);
        if parts.is_empty() {
            return Err(TranspileError::new("probe exec requires a command"));
        }
        return Ok(Probe::Exec(ExecProbe {
            command: parts[0].clone(),
            args: parts[1..].to_vec(),
        }));
    }

    if rest.starts_with("file ") {
        let path = rest.strip_prefix("file ").unwrap_or("").trim().to_string();
        return Ok(Probe::File(FileProbe { path }));
    }

    if rest.starts_with("process ") {
        let name = rest
            .strip_prefix("process ")
            .unwrap_or("")
            .trim()
            .to_string();
        return Ok(Probe::Process(ProcessProbe { name }));
    }

    Err(TranspileError::new(format!("unknown probe type: '{rest}'")))
}

fn resolve_http_probe(rest: &str) -> Result<Probe, TranspileError> {
    let parts: Vec<&str> = rest.splitn(2, ' ').collect();
    if parts.is_empty() {
        return Err(TranspileError::new("http probe needs METHOD and path"));
    }

    let method = HttpMethod::from_str(parts[0])
        .ok_or_else(|| TranspileError::new(format!("unknown HTTP method: {}", parts[0])))?;

    if parts.len() < 2 {
        return Err(TranspileError::new("http probe needs a path"));
    }

    let rest = parts[1].trim();
    let tokens: Vec<&str> = rest.split_whitespace().collect();
    if tokens.is_empty() {
        return Err(TranspileError::new("http probe needs a path"));
    }

    let path = tokens[0].to_string();
    let mut body = None;
    let mut mode = HttpMode::Single;
    let mut idx = 1;

    // look for JSON body
    if idx < tokens.len() {
        let maybe_body = tokens[idx..].join(" ");
        if maybe_body.starts_with('{') {
            if let Some(end) = find_matching_brace(&maybe_body) {
                body = Some(maybe_body[..=end].to_string());
                let body_str = &maybe_body[..=end];
                idx += body_str.split_whitespace().count();
            }
        } else if maybe_body.starts_with('"') {
            if let Some(end) = maybe_body[1..].find('"') {
                body = Some(maybe_body[1..=end].to_string());
                idx += 1;
            }
        }
    }

    // mode keywords
    while idx < tokens.len() {
        match tokens[idx] {
            "concurrent" => {
                idx += 1;
                if idx < tokens.len() {
                    if let Ok(n) = tokens[idx].parse::<u32>() {
                        mode = HttpMode::Concurrent { clients: n };
                        idx += 1;
                    }
                }
            }
            "keepalive" => {
                idx += 1;
                if idx < tokens.len() {
                    if let Ok(n) = tokens[idx].parse::<u32>() {
                        mode = HttpMode::Keepalive { requests: n };
                        idx += 1;
                    }
                }
            }
            "pipelined" => {
                idx += 1;
                if idx < tokens.len() {
                    if let Ok(n) = tokens[idx].parse::<u32>() {
                        mode = HttpMode::Pipelined { requests: n };
                        idx += 1;
                    }
                }
            }
            "burst" => {
                idx += 1;
                if idx < tokens.len() {
                    if let Ok(n) = tokens[idx].parse::<u32>() {
                        idx += 1;
                        let mut window_ms = 1000;
                        if idx < tokens.len() && tokens[idx] == "window" {
                            idx += 1;
                            if idx < tokens.len() {
                                if let Some(d) = parse_duration(tokens[idx]) {
                                    window_ms = d.as_millis() as u64;
                                    idx += 1;
                                }
                            }
                        }
                        mode = HttpMode::Burst {
                            count: n,
                            window_ms,
                        };
                    }
                }
            }
            "chunked" => {
                mode = HttpMode::Chunked;
                idx += 1;
            }
            _ => {
                if body.is_none() {
                    body = Some(tokens[idx..].join(" "));
                }
                break;
            }
        }
    }

    Ok(Probe::Http(HttpProbe {
        method,
        path,
        body,
        headers: HashMap::new(),
        mode,
    }))
}

fn resolve_expect_block(
    block: &crate::parser::ast::Block,
) -> Result<(Vec<Expectation>, Vec<Capture>), TranspileError> {
    let mut expectations = Vec::new();
    let mut captures = Vec::new();

    for item in &block.items {
        match item {
            crate::parser::ast::AstItem::Property(prop) => {
                let (field_str, op) = parse_field_and_op(&prop.key);
                let expected = property_value_to_expected(&prop.value, &op)?;
                expectations.push(Expectation {
                    field: FieldPath::from_dotted(&field_str),
                    op,
                    expected,
                });
            }
            crate::parser::ast::AstItem::Line(line) => {
                let trimmed = line.content.trim();
                if trimmed.starts_with("capture") {
                    if let Some(cap) = parse_capture_line(trimmed)? {
                        captures.push(cap);
                    }
                } else if trimmed.starts_with("all ") {
                    let rest = trimmed.strip_prefix("all ").unwrap_or("").trim();
                    if let Some((key, val)) = rest.split_once(':') {
                        let expected = parse_string_to_expected(val.trim())?;
                        expectations.push(Expectation {
                            field: FieldPath::simple(key.trim()),
                            op: Op::All,
                            expected,
                        });
                    }
                }
            }
            crate::parser::ast::AstItem::Block(_) => {}
        }
    }

    Ok((expectations, captures))
}

fn parse_field_and_op(key: &str) -> (String, Op) {
    if let Some(f) = key.strip_suffix(" contains") {
        return (f.to_string(), Op::Contains);
    }
    if let Some(f) = key.strip_suffix(" starts-with") {
        return (f.to_string(), Op::StartsWith);
    }
    if let Some(f) = key.strip_suffix(" matches-file") {
        return (f.to_string(), Op::MatchesFile);
    }
    if let Some(f) = key.strip_suffix(" matches") {
        return (f.to_string(), Op::Matches);
    }
    if let Some(f) = key.strip_suffix(" present") {
        return (f.to_string(), Op::Present);
    }
    if let Some(f) = key.strip_suffix(" absent") {
        return (f.to_string(), Op::Absent);
    }
    if let Some(f) = key.strip_suffix(" >=") {
        return (f.to_string(), Op::Gte);
    }
    if let Some(f) = key.strip_suffix(" <=") {
        return (f.to_string(), Op::Lte);
    }
    if let Some(f) = key.strip_suffix(" >") {
        return (f.to_string(), Op::Gt);
    }
    if let Some(f) = key.strip_suffix(" <") {
        return (f.to_string(), Op::Lt);
    }
    (key.to_string(), Op::Eq)
}

fn property_value_to_expected(
    val: &crate::parser::ast::PropertyValue,
    op: &Op,
) -> Result<ExpectedValue, TranspileError> {
    use crate::parser::ast::PropertyValue;
    match op {
        Op::Present | Op::Absent => Ok(ExpectedValue::Bool(true)),
        Op::Matches => {
            if let Some(s) = val.as_str() {
                let pattern = s
                    .strip_prefix('/')
                    .and_then(|s| s.strip_suffix('/'))
                    .unwrap_or(s);
                Ok(ExpectedValue::Regex(pattern.to_string()))
            } else {
                Err(TranspileError::new(
                    "matches operator requires a regex value",
                ))
            }
        }
        Op::MatchesFile => {
            if let Some(s) = val.as_str() {
                Ok(ExpectedValue::FilePath(s.to_string()))
            } else {
                Err(TranspileError::new("matches-file requires a file path"))
            }
        }
        _ => match val {
            PropertyValue::Int(n) => Ok(ExpectedValue::Int(*n)),
            PropertyValue::Bool(b) => Ok(ExpectedValue::Bool(*b)),
            PropertyValue::String(s) => {
                if s.starts_with('$') {
                    Ok(ExpectedValue::Variable(s.clone()))
                } else if s.ends_with('s') || s.ends_with("ms") {
                    if let Some(d) = parse_duration(s) {
                        Ok(ExpectedValue::DurationMs(d.as_millis() as u64))
                    } else {
                        Ok(ExpectedValue::Str(s.clone()))
                    }
                } else {
                    Ok(ExpectedValue::Str(s.clone()))
                }
            }
            PropertyValue::Float(f) => Ok(ExpectedValue::Int(*f as i64)),
            PropertyValue::MultiLine(s) => Ok(ExpectedValue::Str(s.clone())),
        },
    }
}

fn parse_string_to_expected(val: &str) -> Result<ExpectedValue, TranspileError> {
    if let Ok(n) = val.parse::<i64>() {
        return Ok(ExpectedValue::Int(n));
    }
    if val == "true" {
        return Ok(ExpectedValue::Bool(true));
    }
    if val == "false" {
        return Ok(ExpectedValue::Bool(false));
    }
    Ok(ExpectedValue::Str(
        val.trim_matches('"').trim_matches('\'').to_string(),
    ))
}

fn parse_capture_line(line: &str) -> Result<Option<Capture>, TranspileError> {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 4 || parts[0] != "capture" {
        return Ok(None);
    }

    let as_idx = parts
        .iter()
        .position(|&p| p == "as")
        .ok_or_else(|| TranspileError::new(format!("capture line missing 'as': {line}")))?;

    if as_idx + 1 >= parts.len() {
        return Err(TranspileError::new(format!(
            "capture line missing variable: {line}"
        )));
    }

    let field_str = parts[1..as_idx].join(" ");
    let var = parts[as_idx + 1].to_string();

    let field = if field_str.contains(" line ") {
        let base = field_str.split(" line ").next().unwrap_or(&field_str);
        FieldPath::from_dotted(base)
    } else {
        FieldPath::from_dotted(&field_str)
    };

    Ok(Some(Capture {
        field,
        variable: var,
    }))
}

fn resolve_input_block(
    block: &crate::parser::ast::Block,
) -> Result<Vec<InputDecl>, TranspileError> {
    let mut inputs = Vec::new();
    for item in &block.items {
        if let crate::parser::ast::AstItem::Property(prop) = item {
            let value_type = match prop.value.as_str().unwrap_or("string") {
                "number" => InputType::Number,
                "bool" => InputType::Bool,
                other if other.starts_with("number") => InputType::Number,
                _ => InputType::Str,
            };
            let validation = if let Some(s) = prop.value.as_str() {
                if let Some(rest) = s.strip_prefix("string matches ") {
                    Some(InputValidation::Matches(rest.trim_matches('/').to_string()))
                } else {
                    None
                }
            } else {
                None
            };

            inputs.push(InputDecl {
                name: prop.key.clone(),
                value_type,
                validation,
            });
        }
    }
    Ok(inputs)
}

fn resolve_hints_block(block: &crate::parser::ast::Block) -> Result<Vec<Hint>, TranspileError> {
    let mut hints = Vec::new();
    let mut sort_counter = 0u32;

    for item in &block.items {
        match item {
            // structured: hint { text: ... unlock_criteria: ... points_deduction: ... }
            crate::parser::ast::AstItem::Block(b) if b.block_type == "hint" => {
                sort_counter += 1;
                let mut text = String::new();
                let mut unlock_criteria = None;
                let mut points_deduction = 0u32;

                for sub in &b.items {
                    if let crate::parser::ast::AstItem::Property(prop) = sub {
                        match prop.key.as_str() {
                            "text" => {
                                text = prop.value.as_str().unwrap_or("").to_string();
                            }
                            "unlock_criteria" => {
                                unlock_criteria = prop.value.as_str().map(|s| s.to_string());
                            }
                            "points_deduction" => {
                                points_deduction = prop.value.as_i64().unwrap_or(0) as u32;
                            }
                            _ => {}
                        }
                    }
                }

                if !text.is_empty() {
                    hints.push(Hint {
                        text,
                        unlock_criteria,
                        points_deduction,
                        sort_order: sort_counter,
                    });
                }
            }
            // simple list format: - "hint text" (backward compat)
            crate::parser::ast::AstItem::Line(line) => {
                let text = line
                    .content
                    .trim()
                    .trim_start_matches('-')
                    .trim()
                    .trim_matches('"')
                    .trim_matches('\'')
                    .to_string();
                if !text.is_empty() {
                    sort_counter += 1;
                    hints.push(Hint {
                        text,
                        unlock_criteria: None,
                        points_deduction: 0,
                        sort_order: sort_counter,
                    });
                }
            }
            _ => {}
        }
    }
    Ok(hints)
}

fn resolve_headers_block(
    block: &crate::parser::ast::Block,
) -> Result<HashMap<String, String>, TranspileError> {
    let mut headers = HashMap::new();
    for item in &block.items {
        if let crate::parser::ast::AstItem::Property(prop) = item {
            if let Some(v) = prop.value.as_str() {
                headers.insert(prop.key.clone(), v.to_string());
            }
        }
    }
    Ok(headers)
}

fn resolve_features(block: &crate::parser::ast::Block) -> Result<Vec<Feature>, TranspileError> {
    let mut features = Vec::new();
    for item in &block.items {
        if let crate::parser::ast::AstItem::Line(line) = item {
            let text = line
                .content
                .trim()
                .trim_start_matches('-')
                .trim()
                .trim_matches('"')
                .trim_matches('\'');
            let parts: Vec<&str> = text.splitn(3, '|').collect();
            if parts.len() >= 3 {
                features.push(Feature {
                    title: parts[0].trim().into(),
                    description: parts[1].trim().into(),
                    icon: parts[2].trim().into(),
                });
            } else if !text.is_empty() {
                features.push(Feature {
                    title: text.into(),
                    description: String::new(),
                    icon: String::new(),
                });
            }
        }
    }
    Ok(features)
}

fn apply_project_meta_property(
    meta: &mut ProjectMeta,
    prop: &crate::parser::ast::Property,
) -> Result<(), TranspileError> {
    match prop.key.as_str() {
        "slug" => meta.slug = prop.value.as_str().map(|s| s.to_string()),
        "short_description" => meta.short_description = prop.value.as_str().map(|s| s.to_string()),
        "long_description" => meta.long_description = prop.value.as_str().map(|s| s.to_string()),
        "headline" => meta.headline = prop.value.as_str().map(|s| s.to_string()),
        "runner_image" => meta.runner_image = prop.value.as_str().map(|s| s.to_string()),
        "markdown" => meta.markdown = prop.value.as_str().map(|s| s.to_string()),
        "is_published" => meta.is_published = prop.value.as_bool().unwrap_or(false),
        "is_featured" => meta.is_featured = prop.value.as_bool().unwrap_or(false),
        "is_challenge" => meta.is_challenge = prop.value.as_bool().unwrap_or(false),
        "unlock_mode" => meta.unlock_mode = prop.value.as_str().unwrap_or("sequential").to_string(),
        "featured_order" => meta.featured_order = prop.value.as_i64().map(|n| n as u32),
        "published_at" => meta.published_at = prop.value.as_str().map(|s| s.to_string()),
        "related_course_slug" => {
            meta.related_course_slug = prop.value.as_str().map(|s| s.to_string())
        }
        "difficulty" => meta.difficulty = prop.value.as_str().map(|s| s.to_string()),
        _ => {}
    }
    Ok(())
}

fn parse_retry_from_property(
    prop: &crate::parser::ast::Property,
) -> Result<Option<RetryConfig>, TranspileError> {
    if let Some(n) = prop.value.as_i64() {
        return Ok(Some(RetryConfig {
            max_attempts: n as u32,
            delay: std::time::Duration::from_secs(1),
        }));
    }
    if let Some(s) = prop.value.as_str() {
        return parse_retry_string(s);
    }
    Ok(None)
}

fn parse_retry_from_line(line: &str) -> Result<Option<RetryConfig>, TranspileError> {
    let rest = line
        .strip_prefix("retry")
        .unwrap_or(line)
        .trim()
        .strip_prefix(':')
        .unwrap_or(line)
        .trim();
    parse_retry_string(rest)
}

fn parse_retry_string(s: &str) -> Result<Option<RetryConfig>, TranspileError> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.is_empty() {
        return Ok(None);
    }
    let max_attempts = parts[0]
        .parse::<u32>()
        .map_err(|_| TranspileError::new(format!("invalid retry count: {}", parts[0])))?;
    let mut delay = std::time::Duration::from_secs(1);
    if parts.len() >= 3 && parts[1] == "delay" {
        delay = parse_duration(parts[2])
            .ok_or_else(|| TranspileError::new(format!("invalid delay: {}", parts[2])))?;
    }
    Ok(Some(RetryConfig {
        max_attempts,
        delay,
    }))
}

fn parse_timeout_from_line(line: &str) -> Option<std::time::Duration> {
    let rest = line
        .strip_prefix("timeout")
        .unwrap_or(line)
        .trim()
        .strip_prefix(':')
        .unwrap_or(line)
        .trim();
    parse_duration(rest)
}

/// Split a probe's command into argv the way a shell would.
///
/// **A backslash escapes the next character.** Without that, a quote written
/// as `\"` in the blueprint — content, not a delimiter — is read as a
/// delimiter and dropped, which silently rewrites the command. It is what
/// turned `awk -F: '{print $1 \"\t\" $6}'` into `{print $1 <tab> $6}`, an awk
/// program that concatenates two fields rather than separating them, and it
/// corrupted the *expected* side of every probe that diffs against one.
///
/// The rules are a shell's, because the string is on its way to one. Inside
/// single quotes nothing is an escape. Inside double quotes a backslash only
/// escapes `"`, `\`, `$` and a backtick; before anything else *both*
/// characters stay, so `\t` and `\n` reach awk and printf as the two
/// characters they expand themselves — turning them into a real tab or newline
/// here would put a raw newline inside an awk string literal, which is a
/// syntax error. Unquoted, a backslash escapes whatever follows it.
///
/// An empty quoted string is an argument: `--format ''` passes one empty
/// argument rather than none, which is why emptiness alone cannot decide
/// whether there is an argument to push.
fn shell_split(input: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    // Distinguishes "no argument yet" from "an argument that is empty".
    let mut started = false;
    let mut chars = input.chars();

    while let Some(ch) = chars.next() {
        match ch {
            '\\' if !in_single => {
                match chars.next() {
                    // What a shell unescapes inside double quotes, and
                    // anything at all when unquoted.
                    Some(next) if !in_double || matches!(next, '"' | '\\' | '$' | '`') => {
                        current.push(next);
                    }
                    // Left for whoever runs the command to interpret.
                    Some(next) => {
                        current.push('\\');
                        current.push(next);
                    }
                    // A trailing backslash is not an escape of anything.
                    None => current.push('\\'),
                }
                started = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
                started = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                started = true;
            }
            _ if ch.is_whitespace() && !in_single && !in_double => {
                if started {
                    result.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            _ => {
                current.push(ch);
                started = true;
            }
        }
    }

    if started {
        result.push(current);
    }
    result
}

fn find_matching_brace(s: &str) -> Option<usize> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escape = false;
    for (i, ch) in s.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' => escape = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::grammar::parse as parse_bp;

    fn transpile_str(input: &str) -> Result<Blueprint, TranspileError> {
        let ast = parse_bp(input).map_err(|e| TranspileError::new(e.to_string()))?;
        transpile(&ast)
    }

    #[test]
    fn test_version_triple_handles_kernel_and_constraint_forms() {
        assert_eq!(parse_version_triple(">=6.1"), Some((6, 1, 0)));
        assert_eq!(parse_version_triple("6.1.0"), Some((6, 1, 0)));
        assert_eq!(parse_version_triple("5.15.0-91-generic"), Some((5, 15, 0)));
        assert_eq!(parse_version_triple("6.1.0-rc1"), Some((6, 1, 0)));
        assert_eq!(parse_version_triple("nonsense"), None);
    }

    #[test]
    fn test_top_level_requires_block() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    requires {
        kernel: ">=6.1"
        cgroup: "v2"
        userns: true
        btf: true
        cap: "SYS_ADMIN, bpf"
        host: "Linux"
    }
    phase "p" {
        step "s" {
            probe exec "true"
            expect { exit: 0 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("transpile: {e}"));

        let r = bp.requires_env.unwrap_or_else(|| panic!("no requires_env"));
        assert_eq!(r.kernel, Some((6, 1, 0)));
        assert!(r.cgroup_v2);
        assert!(r.userns);
        assert!(r.btf);
        assert_eq!(r.caps, vec!["SYS_ADMIN".to_string(), "BPF".to_string()]);
        assert_eq!(r.host.as_deref(), Some("linux"));
        assert!(!r.is_empty());
    }

    #[test]
    fn test_step_level_requires_block() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    phase "p" {
        step "s" {
            requires { privileged: true  pid: "host"  cgroup: 2 }
            probe exec "true"
            expect { exit: 0 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("transpile: {e}"));

        let step = &bp.phases[0].steps[0];
        let r = step
            .requires_env
            .as_ref()
            .unwrap_or_else(|| panic!("no requires_env"));
        assert!(r.privileged);
        assert!(r.pid_host);
        assert!(
            r.cgroup_v2,
            "cgroup: 2 should be accepted as well as \"v2\""
        );
        // the block form must not disturb the variable form
        assert!(step.requires.is_empty());
    }

    /// `requires` is overloaded: a block means host capabilities, a property or
    /// bare line means "skip unless this captured variable exists". both forms
    /// have to keep working, and a blueprint may use both at once.
    #[test]
    fn test_requires_variable_forms_still_resolve() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    phase "p" {
        step "property form" {
            requires: "$TOKEN"
            probe exec "true"
            expect { exit: 0 }
        }
        step "line form" {
            requires $SESSION
            probe exec "true"
            expect { exit: 0 }
        }
        step "both forms" {
            requires: "$TOKEN"
            requires { btf: true }
            probe exec "true"
            expect { exit: 0 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("transpile: {e}"));

        let steps = &bp.phases[0].steps;
        assert_eq!(steps[0].requires, vec!["$TOKEN".to_string()]);
        assert!(steps[0].requires_env.is_none());
        assert_eq!(steps[1].requires, vec!["$SESSION".to_string()]);
        assert!(steps[1].requires_env.is_none());

        assert_eq!(steps[2].requires, vec!["$TOKEN".to_string()]);
        assert!(steps[2]
            .requires_env
            .as_ref()
            .map(|r| r.btf)
            .unwrap_or(false));
    }

    #[test]
    fn test_no_requires_block_leaves_none() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    phase "p" {
        step "s" {
            probe exec "true"
            expect { exit: 0 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("transpile: {e}"));
        assert!(bp.requires_env.is_none());
        assert!(bp.phases[0].steps[0].requires_env.is_none());
    }

    #[test]
    fn test_minimal_transpile() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    config { timeout: 10s }
    phase "basics" {
        step "check port" {
            probe tcp 4221
            expect { connected: true }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.name, "Test");
        assert_eq!(bp.config.timeout, std::time::Duration::from_secs(10));
        assert_eq!(bp.phases.len(), 1);
        assert!(matches!(
            &bp.phases[0].steps[0].probe,
            Probe::Tcp(TcpProbe { port: 4221 })
        ));
    }

    #[test]
    fn test_http_probe() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "r" {
        step "root" {
            probe http GET /
            expect { status: 200 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        match &bp.phases[0].steps[0].probe {
            Probe::Http(p) => {
                assert_eq!(p.method, HttpMethod::GET);
                assert_eq!(p.path, "/");
            }
            other => panic!("expected HttpProbe, got {other:?}"),
        }
    }

    #[test]
    fn test_http_step_headers_are_moved_into_probe_headers() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "r" {
        step "root" {
            probe http GET /
            headers { User-Agent: test-agent/1.0 }
            expect { status: 200 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));

        let step = &bp.phases[0].steps[0];
        match &step.probe {
            Probe::Http(p) => {
                assert_eq!(
                    p.headers.get("User-Agent").map(|s| s.as_str()),
                    Some("test-agent/1.0")
                );
            }
            other => panic!("expected HttpProbe, got {other:?}"),
        }
        assert!(step.headers.is_empty());
    }

    #[test]
    fn test_exec_probe() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "e" {
        step "run" {
            probe exec ./your-docker run echo hello
            expect { stdout: "hello" exit: 0 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        match &bp.phases[0].steps[0].probe {
            Probe::Exec(p) => {
                assert_eq!(p.command, "./your-docker");
                assert_eq!(p.args, vec!["run", "echo", "hello"]);
            }
            other => panic!("expected ExecProbe, got {other:?}"),
        }
    }

    #[test]
    fn test_docker_probe_is_exec() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "d" {
        step "inspect" {
            probe docker inspect nginx-1 --format '{{.State.Status}}'
            expect { exit: 0 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        match &bp.phases[0].steps[0].probe {
            Probe::Exec(p) => {
                assert_eq!(p.command, "docker");
                assert!(p.args.contains(&"inspect".to_string()));
            }
            other => panic!("expected ExecProbe, got {other:?}"),
        }
    }

    #[test]
    fn test_capture() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "t" {
        step "cap" {
            probe docker inspect nginx-1
            expect {
                exit: 0
                capture stdout as $container_id
            }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.phases[0].steps[0].captures.len(), 1);
        assert_eq!(bp.phases[0].steps[0].captures[0].variable, "$container_id");
    }

    #[test]
    fn test_input() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "t" {
        step "confirm" {
            input { container-id: string }
            probe docker inspect nginx-1
            expect { stdout: "created" }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.phases[0].steps[0].inputs.len(), 1);
        assert_eq!(bp.phases[0].steps[0].inputs[0].name, "container-id");
    }

    #[test]
    fn test_depends_on() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "first" {
        step "s" { probe tcp 80 expect { connected: true } }
    }
    phase "second" {
        depends-on: "first"
        step "s" { probe tcp 81 expect { connected: true } }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.phases[1].depends_on, vec!["first"]);
    }

    #[test]
    fn test_operators() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "t" {
        step "ops" {
            probe http GET /
            expect {
                status: 200
                body contains: "hello"
                body.json.count > 5
                duration < 10s
                header.Server present
            }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let exps = &bp.phases[0].steps[0].expectations;
        assert!(exps.iter().any(|e| e.op == Op::Eq));
        assert!(exps.iter().any(|e| e.op == Op::Contains));
        assert!(exps.iter().any(|e| e.op == Op::Gt));
        assert!(exps.iter().any(|e| e.op == Op::Lt));
        assert!(exps.iter().any(|e| e.op == Op::Present));
    }

    #[test]
    fn test_container_lifecycle_full() {
        let bp = transpile_str(
            r#"
blueprint "Container Lifecycle" {
    config { timeout: 10s }
    phase "create" {
        step "exists" {
            probe docker inspect nginx-1 --format '{{.State.Status}}'
            expect { exit: 0 stdout: "created" }
        }
        step "capture id" {
            probe docker inspect nginx-1 --format '{{.ID}}'
            expect {
                stdout matches: /^[a-f0-9]{64}$/
                capture stdout as $container_id
            }
        }
        step "confirm id" {
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
        step "is running" {
            probe docker inspect nginx-1 --format '{{.State.Status}}'
            expect { stdout: "running" }
        }
    }
    phase "stopped" {
        depends-on: "running"
        step "is exited" {
            probe docker inspect nginx-1 --format '{{.State.Status}}'
            expect { stdout: "exited" }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.name, "Container Lifecycle");
        assert_eq!(bp.phases.len(), 3);
        assert_eq!(bp.phases[0].steps.len(), 3);
        assert_eq!(bp.phases[0].steps[2].inputs.len(), 1);
        assert_eq!(bp.phases[0].steps[1].captures.len(), 1);
    }

    #[test]
    fn test_project_metadata() {
        let bp = transpile_str(
            r#"
blueprint "HTTP Server" {
    slug: build-your-own-http-server
    is_published: true
    is_featured: true
    runner_image: local|go|rust|c
    features {
        - "Raw TCP Sockets | Start at the bottom | plug"
        - "Protocol Parsing | Parse by hand | code"
    }
    phase "test" {
        step "s1" { probe tcp 4221 expect { connected: true } }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.meta.slug.as_deref(), Some("build-your-own-http-server"));
        assert!(bp.meta.is_published);
        assert_eq!(bp.meta.features.len(), 2);
        assert_eq!(bp.meta.features[0].title, "Raw TCP Sockets");
    }

    #[test]
    fn test_phase_metadata() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    phase "tcp" {
        title: "Listen on Port"
        slug: listen-on-port
        description: "Create a TCP server"
        points: 25
        scores: "5:10:25|10:20:15"
        visibility_level: 3
        is_free: false
        abandoned_deduction: 5

        step "port open" {
            slug: listen-on-port
            probe tcp 4221
            expect { connected: true }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let phase = &bp.phases[0];
        assert_eq!(phase.meta.title.as_deref(), Some("Listen on Port"));
        assert_eq!(phase.meta.slug.as_deref(), Some("listen-on-port"));
        assert_eq!(
            phase.meta.description.as_deref(),
            Some("Create a TCP server")
        );
        assert_eq!(phase.meta.points, 25);
        assert_eq!(phase.meta.scores.as_deref(), Some("5:10:25|10:20:15"));
        assert_eq!(phase.meta.visibility_level, 3);
        assert!(!phase.meta.is_free);
        assert_eq!(phase.meta.abandoned_deduction, 5);
    }

    #[test]
    fn test_phase_structured_hints() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    phase "tcp" {
        slug: listen-on-port

        hints {
            hint {
                text: "Use net.Listen in Go"
                unlock_criteria: "5:3:A"
                points_deduction: 5
            }
            hint {
                text: "Check for errors"
                unlock_criteria: "10:5:A"
                points_deduction: 10
            }
        }

        step "port open" {
            slug: listen-on-port
            probe tcp 4221
            expect { connected: true }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let hints = &bp.phases[0].meta.hints;
        assert_eq!(hints.len(), 2);
        assert_eq!(hints[0].text, "Use net.Listen in Go");
        assert_eq!(hints[0].unlock_criteria.as_deref(), Some("5:3:A"));
        assert_eq!(hints[0].points_deduction, 5);
        assert_eq!(hints[0].sort_order, 1);
        assert_eq!(hints[1].text, "Check for errors");
        assert_eq!(hints[1].sort_order, 2);
    }

    #[test]
    fn test_phase_multiline_description() {
        let bp = transpile_str(
            r#"
blueprint "Test" {
    phase "tcp" {
        slug: listen-on-port
        description: |
            Create a TCP server that listens on port 4221.
            Your server should accept incoming connections.

        step "port open" {
            slug: listen-on-port
            probe tcp 4221
            expect { connected: true }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let desc = bp.phases[0].meta.description.as_deref();
        assert!(desc.is_some());
        assert!(desc.unwrap_or("").contains("TCP server"));
        assert!(desc.unwrap_or("").contains("incoming connections"));
    }

    #[test]
    fn test_concurrent_mode() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    phase "t" {
        step "c" {
            probe http GET / concurrent 10
            expect { all status: 200 }
        }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        match &bp.phases[0].steps[0].probe {
            Probe::Http(p) => assert!(matches!(p.mode, HttpMode::Concurrent { clients: 10 })),
            other => panic!("expected HttpProbe, got {other:?}"),
        }
    }

    fn first_http_probe(input: &str) -> HttpProbe {
        let bp = transpile_str(input).unwrap_or_else(|e| panic!("{e}"));
        match &bp.phases[0].steps[0].probe {
            Probe::Http(p) => p.clone(),
            other => panic!("expected HttpProbe, got {other:?}"),
        }
    }

    #[test]
    fn test_json_body_and_the_modifiers_after_it_survive() {
        // the `{` of the body used to end the probe line: the body and
        // `concurrent 4` were dropped and the probe ran once, with no body
        let p = first_http_probe(
            r#"
blueprint "T" {
    phase "t" {
        step "s" {
            probe http POST /jobs {"type":"echo","payload":"hi"} concurrent 4
            expect { all status: 202 }
        }
    }
}
"#,
        );
        assert_eq!(p.path, "/jobs");
        assert_eq!(p.body.as_deref(), Some(r#"{"type":"echo","payload":"hi"}"#));
        assert!(matches!(p.mode, HttpMode::Concurrent { clients: 4 }));

        let p = first_http_probe(
            r#"
blueprint "T" {
    phase "t" {
        step "s" {
            probe http POST /jobs {"type": "echo"} burst 100 window 1s expect { rejected: > 0 }
        }
    }
}
"#,
        );
        assert_eq!(p.body.as_deref(), Some(r#"{"type": "echo"}"#));
        assert!(matches!(
            p.mode,
            HttpMode::Burst {
                count: 100,
                window_ms: 1000
            }
        ));
    }

    #[test]
    fn test_shell_split_keeps_an_escaped_quote_as_content() {
        // The quote is content, not a delimiter. Dropping it rewrote the awk
        // program into one that concatenated its two fields.
        let result = shell_split(r#"bash -c "awk '{print $1 \"\t\" $6}'""#);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0], "bash");
        assert_eq!(result[1], "-c");
        assert_eq!(result[2], r#"awk '{print $1 "\t" $6}'"#);
    }

    #[test]
    fn test_shell_split_keeps_runs_of_spaces_inside_quotes() {
        // A printf format's column padding is content too.
        let result = shell_split(r#"bash -c "printf 'a:  %s\n' x""#);

        assert_eq!(result.len(), 3);
        assert_eq!(result[2], r"printf 'a:  %s\n' x");
    }

    #[test]
    fn test_shell_split_leaves_a_backslash_alone_inside_single_quotes() {
        // A shell does not process escapes in single quotes, and neither does
        // this: `\.` is a regex, not an escaped dot.
        let result = shell_split(r"grep -E '10\.0\.[0-9]+'");

        assert_eq!(result, vec!["grep", "-E", r"10\.0\.[0-9]+"]);
    }

    #[test]
    fn test_shell_split_treats_an_empty_quoted_string_as_an_argument() {
        let result = shell_split("docker inspect --format ''");

        assert_eq!(result, vec!["docker", "inspect", "--format", ""]);
    }

    #[test]
    fn test_shell_split_splits_on_any_whitespace_outside_quotes() {
        let result = shell_split("echo\tone  two");

        assert_eq!(result, vec!["echo", "one", "two"]);
    }

    #[test]
    fn test_shell_split() {
        let result = shell_split("inspect nginx-1 --format '{{.State.Status}}'");
        assert_eq!(
            result,
            vec!["inspect", "nginx-1", "--format", "{{.State.Status}}"]
        );
    }

    #[test]
    fn test_config_bin_and_bin_path() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    config {
        timeout: 10s
        bin: seachart
        bin_path {
            go: "./seachart"
            rust: "./target/debug/seachart"
            c: "./seachart"
        }
    }
    phase "t" {
        step "s" { probe tcp 4221 expect { connected: true } }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.config.bin.as_deref(), Some("seachart"));
        assert_eq!(
            bp.config.bin_path.get("go").map(|s| s.as_str()),
            Some("./seachart")
        );
        assert_eq!(
            bp.config.bin_path.get("rust").map(|s| s.as_str()),
            Some("./target/debug/seachart")
        );
        assert_eq!(
            bp.config.bin_path.get("c").map(|s| s.as_str()),
            Some("./seachart")
        );
    }

    #[test]
    fn test_config_bin_without_bin_path() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    config {
        timeout: 5s
        bin: myapp
    }
    phase "t" {
        step "s" { probe tcp 80 expect { connected: true } }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bp.config.bin.as_deref(), Some("myapp"));
        assert!(bp.config.bin_path.is_empty());
    }

    #[test]
    fn test_config_defaults_without_bin() {
        let bp = transpile_str(
            r#"
blueprint "T" {
    config { timeout: 10s }
    phase "t" {
        step "s" { probe tcp 80 expect { connected: true } }
    }
}
"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(bp.config.bin.is_none());
        assert!(bp.config.bin_path.is_empty());
    }
}
