use crate::executor::context::Context;
use crate::executor::error::ExecutionError;
use crate::transpiler::ir::{ExecProbe, ProbeResult, Value};
use log::debug;
use std::collections::HashMap;
use std::process::Stdio;
use std::time::{Duration, Instant};
use tokio::process::Command;

pub async fn execute(probe: &ExecProbe, ctx: &Context) -> Result<ProbeResult, ExecutionError> {
    execute_with_timeout(probe, ctx, None).await
}

pub async fn execute_with_timeout(
    probe: &ExecProbe,
    ctx: &Context,
    timeout: Option<Duration>,
) -> Result<ProbeResult, ExecutionError> {
    let interpolated_cmd = ctx.interpolate(&probe.command);
    let explicit_args: Vec<String> = probe.args.iter().map(|a| ctx.interpolate(a)).collect();

    // when a variable like $BUILD expands to "go build .", the whole string ends
    // up as the command with no args. Command::new("go build .") would fail
    // because there's no binary named "go build .". re-split when the original
    // probe had no args but the interpolated command contains spaces.
    let (command, args) = if explicit_args.is_empty() && interpolated_cmd.contains(' ') {
        let parts: Vec<&str> = interpolated_cmd.split_whitespace().collect();
        (
            parts[0].to_string(),
            parts[1..].iter().map(|s| s.to_string()).collect(),
        )
    } else {
        (interpolated_cmd, explicit_args)
    };

    let start = Instant::now();

    // in linux| mode this becomes `docker exec -w /workspace <container> <cmd>`;
    // locally it is the command itself
    let (program, wrapped_args, apply_cwd) = ctx.runner.wrap(&command, &args);

    let mut cmd = Command::new(&program);
    cmd.args(&wrapped_args);
    cmd.envs(ctx.runner.env().iter().cloned());
    if apply_cwd {
        if let Some(ref ws) = ctx.workspace {
            cmd.current_dir(ws);
        }
    }

    // use step timeout, fall back to config timeout, or default 30s
    let deadline = timeout.unwrap_or(ctx.config.timeout);

    debug!(
        "exec: {} {} (cwd: {:?}, timeout: {}s)",
        command,
        args.join(" "),
        ctx.workspace,
        deadline.as_secs()
    );

    // what `output()` would set up, done by hand so the child is ours to kill
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // its own process group, so a timeout can take down whatever the command
    // started as well. A server the probe launched, or one a `sh -c` script
    // backgrounded, otherwise outlived the timeout and kept its port. The
    // cost: Ctrl-C at the terminal no longer reaches it. In linux| mode this
    // only reaches the `docker exec` client, not the process in the container.
    #[cfg(unix)]
    cmd.process_group(0);

    let child = cmd
        .spawn()
        .map_err(|e| ExecutionError::new(format!("failed to execute '{}': {}", command, e)))?;
    let pid = child.id();

    let output = match tokio::time::timeout(deadline, child.wait_with_output()).await {
        Ok(output) => output
            .map_err(|e| ExecutionError::new(format!("failed to execute '{}': {}", command, e)))?,
        Err(_) => {
            // the dropped future took the child with it (kill_on_drop); this
            // gets the rest of its group
            kill_process_group(pid);
            return Err(ExecutionError::new(format!(
                "'{}' timed out after {}s",
                command,
                deadline.as_secs()
            )));
        }
    };

    let duration = start.elapsed();

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    let mut fields = HashMap::new();
    fields.insert("stdout".to_string(), Value::String(stdout.clone()));
    fields.insert("stderr".to_string(), Value::String(stderr));
    fields.insert("exit".to_string(), Value::Int(exit_code as i64));
    fields.insert(
        "duration".to_string(),
        Value::Int(duration.as_millis() as i64),
    );

    Ok(ProbeResult {
        fields,
        raw_stdout: Some(stdout),
        duration_ms: duration.as_millis() as u64,
    })
}

#[cfg(unix)]
fn kill_process_group(pid: Option<u32>) {
    use nix::sys::signal::{killpg, Signal};
    use nix::unistd::Pid;

    let Some(pgid) = pid.and_then(|p| i32::try_from(p).ok()) else {
        return;
    };
    // ESRCH just means everything in it has already exited
    if let Err(e) = killpg(Pid::from_raw(pgid), Signal::SIGKILL) {
        debug!("exec: killing process group {pgid}: {e}");
    }
}

#[cfg(not(unix))]
fn kill_process_group(_pid: Option<u32>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::executor::context::ExecutionMode;
    use crate::transpiler::ir::Config;

    #[tokio::test]
    async fn test_exec_echo() {
        let probe = ExecProbe {
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
        };
        let ctx = Context::new(Config::default(), ExecutionMode::Validate);

        let result = execute(&probe, &ctx).await;
        assert!(result.is_ok());
        let result = result.unwrap_or_else(|e| panic!("{e}"));

        assert!(matches!(result.fields.get("stdout"), Some(Value::String(s)) if s == "hello"));
        assert!(matches!(result.fields.get("exit"), Some(Value::Int(0))));
    }

    #[tokio::test]
    async fn test_exec_exit_code() {
        let probe = ExecProbe {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "exit 42".to_string()],
        };
        let ctx = Context::new(Config::default(), ExecutionMode::Validate);

        let result = execute(&probe, &ctx).await;
        assert!(result.is_ok());
        let result = result.unwrap_or_else(|e| panic!("{e}"));

        assert!(matches!(result.fields.get("exit"), Some(Value::Int(42))));
    }

    #[tokio::test]
    async fn test_exec_with_variable_interpolation() {
        let probe = ExecProbe {
            command: "echo".to_string(),
            args: vec!["$name".to_string()],
        };
        let mut ctx = Context::new(Config::default(), ExecutionMode::Validate);
        ctx.set_variable("$name", Value::String("world".into()));

        let result = execute(&probe, &ctx).await;
        assert!(result.is_ok());
        let result = result.unwrap_or_else(|e| panic!("{e}"));

        assert!(matches!(result.fields.get("stdout"), Some(Value::String(s)) if s == "world"));
    }

    #[tokio::test]
    async fn test_exec_variable_expands_to_full_command() {
        // simulates `probe exec $BUILD` where $BUILD = "echo hello world"
        // the probe has no explicit args, so the interpolated command must be re-split
        let probe = ExecProbe {
            command: "$BUILD".to_string(),
            args: vec![],
        };
        let mut ctx = Context::new(Config::default(), ExecutionMode::Validate);
        ctx.set_variable("$BUILD", Value::String("echo hello world".into()));

        let result = execute(&probe, &ctx).await;
        assert!(result.is_ok());
        let result = result.unwrap_or_else(|e| panic!("{e}"));

        assert!(
            matches!(result.fields.get("stdout"), Some(Value::String(s)) if s == "hello world")
        );
        assert!(matches!(result.fields.get("exit"), Some(Value::Int(0))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_timeout_kills_what_the_command_started() {
        // stands in for a server the probe launched: a background process
        // that would keep running, and keep its port, after the timeout
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let pid_file = dir.path().join("pid");
        let probe = ExecProbe {
            command: "sh".to_string(),
            args: vec![
                "-c".to_string(),
                format!("sleep 30 & echo $! > {}; wait", pid_file.display()),
            ],
        };
        let ctx = Context::new(Config::default(), ExecutionMode::Validate);

        let result = execute_with_timeout(&probe, &ctx, Some(Duration::from_millis(500))).await;
        assert!(result.is_err(), "the probe should have timed out");

        let pid = std::fs::read_to_string(&pid_file).unwrap_or_else(|e| panic!("{e}"));
        let pid = pid.trim();
        let mut alive = true;
        for _ in 0..20 {
            let status = std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .unwrap_or_else(|e| panic!("{e}"));
            if !status.success() {
                alive = false;
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(!alive, "background process {pid} outlived the timeout");
    }

    #[tokio::test]
    async fn test_exec_stderr() {
        let probe = ExecProbe {
            command: "sh".to_string(),
            args: vec!["-c".to_string(), "echo error >&2".to_string()],
        };
        let ctx = Context::new(Config::default(), ExecutionMode::Validate);

        let result = execute(&probe, &ctx).await;
        assert!(result.is_ok());
        let result = result.unwrap_or_else(|e| panic!("{e}"));

        assert!(matches!(result.fields.get("stderr"), Some(Value::String(s)) if s == "error"));
    }
}
