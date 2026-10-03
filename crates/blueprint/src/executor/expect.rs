use crate::executor::context::Context;
use crate::transpiler::ir::*;

pub fn evaluate_expectations(
    expectations: &[Expectation],
    probe_result: &ProbeResult,
    ctx: &Context,
) -> Vec<ExpectResult> {
    expectations
        .iter()
        .map(|exp| evaluate_one(exp, probe_result, ctx))
        .collect()
}

fn evaluate_one(exp: &Expectation, probe_result: &ProbeResult, ctx: &Context) -> ExpectResult {
    let field_str = exp.field.to_string();
    let actual = resolve_field(&exp.field, probe_result);

    match exp.op {
        Op::Present => {
            let present = actual.is_some();
            return ExpectResult {
                field: field_str,
                op: Op::Present,
                status: if present {
                    Status::Passed
                } else {
                    Status::Failed
                },
                actual: actual.cloned(),
                expected_display: "present".to_string(),
                message: if present {
                    None
                } else {
                    Some(format!(
                        "expected '{}' to be present",
                        exp.field.to_string()
                    ))
                },
            };
        }
        Op::Absent => {
            let absent = actual.is_none();
            return ExpectResult {
                field: field_str,
                op: Op::Absent,
                status: if absent {
                    Status::Passed
                } else {
                    Status::Failed
                },
                actual: actual.cloned(),
                expected_display: "absent".to_string(),
                message: if absent {
                    None
                } else {
                    Some(format!("expected '{}' to be absent", exp.field.to_string()))
                },
            };
        }
        Op::All => return evaluate_all(exp, probe_result, &field_str),
        _ => {}
    }

    let actual = match actual {
        Some(v) => v,
        None => {
            return ExpectResult {
                field: field_str,
                op: exp.op.clone(),
                status: Status::Failed,
                actual: None,
                expected_display: format_expected(&exp.expected),
                message: Some(format!(
                    "field '{}' not found in probe result",
                    exp.field.to_string()
                )),
            };
        }
    };

    let expected_resolved = resolve_expected(&exp.expected, ctx);
    let passed = match &exp.op {
        Op::Eq => value_eq(actual, &expected_resolved),
        Op::Contains => value_contains(actual, &expected_resolved),
        Op::StartsWith => value_starts_with(actual, &expected_resolved),
        Op::Matches => value_matches(actual, &expected_resolved),
        Op::MatchesFile => value_matches_file(actual, &expected_resolved),
        Op::Gt => value_cmp(actual, &expected_resolved, |a, b| a > b),
        Op::Lt => value_cmp(actual, &expected_resolved, |a, b| a < b),
        Op::Gte => value_cmp(actual, &expected_resolved, |a, b| a >= b),
        Op::Lte => value_cmp(actual, &expected_resolved, |a, b| a <= b),
        Op::Present | Op::Absent | Op::All => unreachable!(),
    };

    ExpectResult {
        field: field_str.clone(),
        op: exp.op.clone(),
        status: if passed {
            Status::Passed
        } else {
            Status::Failed
        },
        actual: Some(actual.clone()),
        expected_display: format_expected(&exp.expected),
        message: if passed {
            None
        } else {
            Some(format!(
                "expected {} {} {}, got {}",
                field_str,
                op_display(&exp.op),
                format_expected(&exp.expected),
                actual
            ))
        },
    }
}

fn evaluate_all(exp: &Expectation, probe_result: &ProbeResult, field_str: &str) -> ExpectResult {
    let inner_field = &exp.field.to_string();
    let all_key = format!("all_{inner_field}");
    let all_same = probe_result
        .fields
        .get("all_status_same")
        .and_then(|v| {
            if let Value::Bool(b) = v {
                Some(*b)
            } else {
                None
            }
        })
        .unwrap_or(false);

    if !all_same {
        return ExpectResult {
            field: field_str.to_string(),
            op: Op::All,
            status: Status::Failed,
            actual: None,
            expected_display: format!("all {}", format_expected(&exp.expected)),
            message: Some(format!("not all responses had the same {inner_field}")),
        };
    }

    let all_value = probe_result.fields.get(&all_key);
    let passed = match (&exp.expected, all_value) {
        (ExpectedValue::Int(expected), Some(Value::Int(actual))) => actual == expected,
        (ExpectedValue::Str(expected), Some(Value::String(actual))) => actual == expected,
        _ => false,
    };

    ExpectResult {
        field: field_str.to_string(),
        op: Op::All,
        status: if passed {
            Status::Passed
        } else {
            Status::Failed
        },
        actual: all_value.cloned(),
        expected_display: format!("all {}", format_expected(&exp.expected)),
        message: if passed {
            None
        } else {
            Some("not all responses matched".to_string())
        },
    }
}

fn resolve_field<'a>(path: &FieldPath, result: &'a ProbeResult) -> Option<&'a Value> {
    if let Some(v) = result.get(path) {
        return Some(v);
    }
    let key = path.to_string();
    if let Some(v) = result.fields.get(&key) {
        return Some(v);
    }
    // header names are case-insensitive, and the http probe stores them as
    // reqwest gives them: lowercase. `header.Content-Type` has to find
    // `header.content-type`, or no blueprint written the usual way can pass.
    let name = key.strip_prefix("header.")?;
    result
        .fields
        .get(&format!("header.{}", name.to_ascii_lowercase()))
}

fn resolve_expected(expected: &ExpectedValue, ctx: &Context) -> ExpectedValue {
    match expected {
        ExpectedValue::Variable(var) => {
            if let Some(value) = ctx.get_variable(var) {
                match value {
                    Value::String(s) => ExpectedValue::Str(s.clone()),
                    Value::Int(n) => ExpectedValue::Int(*n),
                    Value::Bool(b) => ExpectedValue::Bool(*b),
                    _ => expected.clone(),
                }
            } else {
                expected.clone()
            }
        }
        ExpectedValue::Str(s) if s.starts_with('$') => {
            if let Some(value) = ctx.get_variable(s) {
                match value {
                    Value::String(sv) => ExpectedValue::Str(sv.clone()),
                    Value::Int(n) => ExpectedValue::Int(*n),
                    _ => expected.clone(),
                }
            } else {
                expected.clone()
            }
        }
        _ => expected.clone(),
    }
}

fn value_eq(actual: &Value, expected: &ExpectedValue) -> bool {
    match (actual, expected) {
        (Value::Int(a), ExpectedValue::Int(b)) => a == b,
        (Value::String(a), ExpectedValue::Str(b)) => a.trim() == b.trim(),
        (Value::String(a), ExpectedValue::Int(b)) => a.trim().parse::<i64>() == Ok(*b),
        (Value::Int(a), ExpectedValue::Str(b)) => b.trim().parse::<i64>() == Ok(*a),
        (Value::Bool(a), ExpectedValue::Bool(b)) => a == b,
        (Value::String(a), ExpectedValue::Bool(b)) => match a.trim() {
            "true" => *b,
            "false" => !*b,
            _ => false,
        },
        _ => false,
    }
}

fn value_contains(actual: &Value, expected: &ExpectedValue) -> bool {
    match (actual, expected) {
        (Value::String(a), ExpectedValue::Str(b)) => a.contains(b.as_str()),
        _ => false,
    }
}

fn value_starts_with(actual: &Value, expected: &ExpectedValue) -> bool {
    match (actual, expected) {
        (Value::String(a), ExpectedValue::Str(b)) => a.starts_with(b.as_str()),
        _ => false,
    }
}

fn value_matches(actual: &Value, expected: &ExpectedValue) -> bool {
    let actual_str = match actual {
        Value::String(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        _ => return false,
    };
    let pattern = match expected {
        ExpectedValue::Regex(r) => r.as_str(),
        ExpectedValue::Str(s) => s
            .strip_prefix('/')
            .and_then(|s| s.strip_suffix('/'))
            .unwrap_or(s.as_str()),
        _ => return false,
    };
    regex::Regex::new(pattern).is_ok_and(|re| re.is_match(actual_str.trim()))
}

fn value_matches_file(actual: &Value, expected: &ExpectedValue) -> bool {
    let actual_str = match actual {
        Value::String(s) => s.as_str(),
        _ => return false,
    };
    let file_path = match expected {
        ExpectedValue::FilePath(p) => p.as_str(),
        ExpectedValue::Str(s) => s.as_str(),
        _ => return false,
    };
    std::fs::read_to_string(file_path).map_or(false, |c| actual_str.trim() == c.trim())
}

fn value_cmp(actual: &Value, expected: &ExpectedValue, cmp: fn(f64, f64) -> bool) -> bool {
    let a = match actual {
        Value::Int(n) => *n as f64,
        Value::Float(f) => *f,
        Value::String(s) => s.parse::<f64>().unwrap_or(f64::NAN),
        _ => return false,
    };
    let b = match expected {
        ExpectedValue::Int(n) => *n as f64,
        ExpectedValue::DurationMs(ms) => *ms as f64,
        ExpectedValue::Str(s) => {
            if let Some(d) = crate::transpiler::ir::parse_duration(s) {
                d.as_millis() as f64
            } else {
                s.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        _ => return false,
    };
    cmp(a, b)
}

fn format_expected(expected: &ExpectedValue) -> String {
    match expected {
        ExpectedValue::Int(n) => n.to_string(),
        ExpectedValue::Str(s) => format!("\"{s}\""),
        ExpectedValue::Bool(b) => b.to_string(),
        ExpectedValue::DurationMs(ms) => format!("{ms}ms"),
        ExpectedValue::Regex(r) => format!("/{r}/"),
        ExpectedValue::FilePath(p) => format!("file:{p}"),
        ExpectedValue::Variable(v) => v.clone(),
    }
}

fn op_display(op: &Op) -> &str {
    match op {
        Op::Eq => "==",
        Op::Contains => "contains",
        Op::StartsWith => "starts-with",
        Op::Matches => "matches",
        Op::MatchesFile => "matches-file",
        Op::Present => "present",
        Op::Absent => "absent",
        Op::Gt => ">",
        Op::Lt => "<",
        Op::Gte => ">=",
        Op::Lte => "<=",
        Op::All => "all",
    }
}

pub fn process_captures(
    captures: &[Capture],
    probe_result: &ProbeResult,
    ctx: &mut Context,
) -> Vec<(String, Value)> {
    let mut captured = Vec::new();
    for cap in captures {
        if let Some(value) = resolve_field(&cap.field, probe_result) {
            ctx.set_variable(&cap.variable, value.clone());
            captured.push((cap.variable.clone(), value.clone()));
        }
    }
    captured
}

pub fn evaluate_input(
    input_name: &str,
    user_value: &str,
    expectations: &[Expectation],
    probe_result: &ProbeResult,
    ctx: &Context,
) -> (bool, Vec<ExpectResult>) {
    let mut results = Vec::new();
    let mut all_passed = true;
    let input_var = format!("${input_name}");

    for exp in expectations {
        let field_str = exp.field.to_string();
        if field_str == input_var {
            let expected_resolved = resolve_expected(&exp.expected, ctx);
            let user_val = Value::String(user_value.to_string());
            let passed = match &exp.op {
                Op::Eq => value_eq(&user_val, &expected_resolved),
                Op::Matches => value_matches(&user_val, &expected_resolved),
                Op::Contains => value_contains(&user_val, &expected_resolved),
                _ => false,
            };
            if !passed {
                all_passed = false;
            }
            results.push(ExpectResult {
                field: field_str,
                op: exp.op.clone(),
                status: if passed {
                    Status::Passed
                } else {
                    Status::Failed
                },
                actual: Some(user_val),
                expected_display: format_expected(&exp.expected),
                message: if passed {
                    None
                } else {
                    Some(format!("input mismatch for {input_name}"))
                },
            });
        } else {
            let r = evaluate_one(exp, probe_result, ctx);
            if r.status != Status::Passed {
                all_passed = false;
            }
            results.push(r);
        }
    }
    (all_passed, results)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pr(fields: Vec<(&str, Value)>) -> ProbeResult {
        ProbeResult {
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
            raw_stdout: None,
            duration_ms: 0,
        }
    }
    fn ctx() -> Context {
        Context::new(
            Config::default(),
            crate::executor::context::ExecutionMode::Validate,
        )
    }

    #[test]
    fn test_eq_int() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("status"),
                op: Op::Eq,
                expected: ExpectedValue::Int(200),
            }],
            &pr(vec![("status", Value::Int(200))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_eq_string() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("body"),
                op: Op::Eq,
                expected: ExpectedValue::Str("hello".into()),
            }],
            &pr(vec![("body", Value::String("hello".into()))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_eq_fails() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("status"),
                op: Op::Eq,
                expected: ExpectedValue::Int(200),
            }],
            &pr(vec![("status", Value::Int(404))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Failed);
    }

    #[test]
    fn test_contains() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("body"),
                op: Op::Contains,
                expected: ExpectedValue::Str("world".into()),
            }],
            &pr(vec![("body", Value::String("hello world".into()))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_matches_regex() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("stdout"),
                op: Op::Matches,
                expected: ExpectedValue::Regex("^[a-f0-9]{64}$".into()),
            }],
            &pr(vec![(
                "stdout",
                Value::String(
                    "a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2".into(),
                ),
            )]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_present() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::from_dotted("header.Server"),
                op: Op::Present,
                expected: ExpectedValue::Bool(true),
            }],
            &pr(vec![("header.Server", Value::String("nginx".into()))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_header_lookup_ignores_case() {
        // the http probe stores header names lowercase, as reqwest gives them
        let probe = pr(vec![
            ("header.server", Value::String("lux".into())),
            ("header.content-type", Value::String("text/plain".into())),
        ]);
        for field in ["header.Server", "header.server", "header.SERVER"] {
            let r = evaluate_expectations(
                &[Expectation {
                    field: FieldPath::from_dotted(field),
                    op: Op::Eq,
                    expected: ExpectedValue::Str("lux".into()),
                }],
                &probe,
                &ctx(),
            );
            assert_eq!(r[0].status, Status::Passed, "{field}");
        }
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::from_dotted("header.Content-Type"),
                op: Op::Contains,
                expected: ExpectedValue::Str("text/plain".into()),
            }],
            &probe,
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_absent() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::from_dotted("header.X-Debug"),
                op: Op::Absent,
                expected: ExpectedValue::Bool(true),
            }],
            &pr(vec![("status", Value::Int(200))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_gt() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::from_dotted("body.json.count"),
                op: Op::Gt,
                expected: ExpectedValue::Int(5),
            }],
            &pr(vec![("body.json.count", Value::Int(10))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_lt_duration() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("duration"),
                op: Op::Lt,
                expected: ExpectedValue::DurationMs(10000),
            }],
            &pr(vec![("duration", Value::Int(5000))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_variable_resolution() {
        let mut c = ctx();
        c.set_variable("$real_id", Value::String("abc123".into()));
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("stdout"),
                op: Op::Eq,
                expected: ExpectedValue::Variable("$real_id".into()),
            }],
            &pr(vec![("stdout", Value::String("abc123".into()))]),
            &c,
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_captures() {
        let probe_result = pr(vec![("stdout", Value::String("container123".into()))]);
        let mut c = ctx();
        let captured = process_captures(
            &[Capture {
                field: FieldPath::simple("stdout"),
                variable: "$cid".to_string(),
            }],
            &probe_result,
            &mut c,
        );
        assert_eq!(captured.len(), 1);
        assert!(c.has_variable("$cid"));
    }

    #[test]
    fn test_string_int_coercion() {
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("stdout"),
                op: Op::Eq,
                expected: ExpectedValue::Int(42),
            }],
            &pr(vec![("stdout", Value::String("42".into()))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_matches_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap_or_else(|e| panic!("{e}"));
        use std::io::Write;
        write!(tmp, "expected output\n").unwrap_or_else(|e| panic!("{e}"));
        let r = evaluate_expectations(
            &[Expectation {
                field: FieldPath::simple("stdout"),
                op: Op::MatchesFile,
                expected: ExpectedValue::FilePath(tmp.path().to_string_lossy().to_string()),
            }],
            &pr(vec![("stdout", Value::String("expected output".into()))]),
            &ctx(),
        );
        assert_eq!(r[0].status, Status::Passed);
    }

    #[test]
    fn test_input_evaluation() {
        let mut c = ctx();
        c.set_variable("$real_id", Value::String("abc123".into()));
        let (passed, _) = evaluate_input(
            "container-id",
            "abc123",
            &[Expectation {
                field: FieldPath::simple("$container-id"),
                op: Op::Eq,
                expected: ExpectedValue::Variable("$real_id".into()),
            }],
            &pr(vec![]),
            &c,
        );
        assert!(passed);
    }
}
