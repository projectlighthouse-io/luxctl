use crate::transpiler::ir::{Config, Value};
use std::collections::HashMap;
use std::path::PathBuf;

/// runtime execution context — holds captured variables, config, and user inputs
#[derive(Debug, Clone)]
pub struct Context {
    pub config: Config,
    pub variables: HashMap<String, Value>,
    pub user_inputs: HashMap<String, String>,
    pub mode: ExecutionMode,
    /// working directory for exec probes (lab workspace path)
    pub workspace: Option<PathBuf>,
    /// where probes execute — on the host, or inside the pinned Linux environment
    pub runner: Runner,
    /// what the host/backend actually supports, used to evaluate `requires { }`
    pub facts: Option<HostFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// `luxctl validate` — probe-only, skip steps with input
    Validate,
    /// `luxctl result` — run steps with input, compare user values
    Result,
}

/// where probes execute.
///
/// `Linux` carries an opaque command prefix supplied by the binary (something
/// like `docker exec -w /workspace lux-<slug>`). The crate never learns what
/// docker or lima are — it only knows how to put a command behind a prefix.
#[derive(Debug, Clone, Default)]
pub enum Runner {
    #[default]
    Local,
    Linux {
        prefix: Vec<String>,
        env: Vec<(String, String)>,
    },
}

impl Runner {
    /// rewrite a command so it lands in the right place.
    ///
    /// returns `(program, args, apply_workspace_cwd)`. the cwd flag is false in
    /// `Linux` mode because the prefix already carries the guest working
    /// directory — setting a host cwd there would be meaningless.
    pub fn wrap(&self, command: &str, args: &[String]) -> (String, Vec<String>, bool) {
        match self {
            Runner::Local => (command.to_string(), args.to_vec(), true),
            Runner::Linux { prefix, .. } => match prefix.split_first() {
                Some((program, rest)) => {
                    let mut wrapped: Vec<String> = rest.to_vec();
                    wrapped.push(command.to_string());
                    wrapped.extend(args.iter().cloned());
                    (program.clone(), wrapped, false)
                }
                // an empty prefix is a caller bug, not a reason to panic
                None => (command.to_string(), args.to_vec(), true),
            },
        }
    }

    /// wrap a `sh -c` script. arguments are passed through argv rather than
    /// interpolated into the script, so a path never becomes shell syntax.
    pub fn wrap_shell(&self, script: &str, args: &[String]) -> (String, Vec<String>, bool) {
        let mut sh_args = vec!["-c".to_string(), script.to_string(), "sh".to_string()];
        sh_args.extend(args.iter().cloned());
        self.wrap("sh", &sh_args)
    }

    pub fn env(&self) -> &[(String, String)] {
        match self {
            Runner::Local => &[],
            Runner::Linux { env, .. } => env,
        }
    }

    pub fn is_local(&self) -> bool {
        matches!(self, Runner::Local)
    }
}

/// what the selected backend actually provides. populated by the binary's
/// preflight; `requires { }` is evaluated against it.
#[derive(Debug, Clone, Default)]
pub struct HostFacts {
    /// host OS as reported by `std::env::consts::OS`
    pub host_os: String,
    /// human name of the selected backend, e.g. "colima"
    pub backend: String,
    /// guest kernel version triple
    pub kernel: Option<(u64, u64, u64)>,
    pub cgroup_v2: bool,
    pub userns: bool,
    pub btf: bool,
}

impl Context {
    pub fn new(config: Config, mode: ExecutionMode) -> Self {
        Self {
            config,
            variables: HashMap::new(),
            user_inputs: HashMap::new(),
            mode,
            workspace: None,
            runner: Runner::Local,
            facts: None,
        }
    }

    pub fn with_workspace(mut self, path: PathBuf) -> Self {
        self.workspace = Some(path);
        self
    }

    pub fn with_runner(mut self, runner: Runner) -> Self {
        self.runner = runner;
        self
    }

    pub fn with_facts(mut self, facts: HostFacts) -> Self {
        self.facts = Some(facts);
        self
    }

    pub fn set_variable(&mut self, name: &str, value: Value) {
        self.variables.insert(name.to_string(), value);
    }

    pub fn get_variable(&self, name: &str) -> Option<&Value> {
        self.variables.get(name)
    }

    pub fn has_variable(&self, name: &str) -> bool {
        self.variables.contains_key(name)
    }

    pub fn set_user_input(&mut self, name: &str, value: &str) {
        self.user_inputs.insert(name.to_string(), value.to_string());
    }

    pub fn get_user_input(&self, name: &str) -> Option<&str> {
        self.user_inputs.get(name).map(|s| s.as_str())
    }

    /// interpolate $variable references in a string
    pub fn interpolate(&self, s: &str) -> String {
        let variables = self.variables.iter().map(|(key, value)| {
            let var_ref = if key.starts_with('$') {
                key.clone()
            } else {
                format!("${key}")
            };
            (var_ref, value.to_string())
        });
        // also interpolate user inputs, after variables of the same name
        let inputs = self
            .user_inputs
            .iter()
            .map(|(key, value)| (format!("${key}"), value.clone()));

        // longest name first: `$BIN` is a prefix of `$BIN_PATH`, and replacing
        // it first turns `$BIN_PATH` into `<bin>_PATH`. HashMap order is random,
        // so without this the corruption came and went between runs.
        let mut refs: Vec<(String, String)> = variables.chain(inputs).collect();
        refs.sort_by_key(|(var_ref, _)| std::cmp::Reverse(var_ref.len()));

        let mut result = s.to_string();
        for (var_ref, value) in refs {
            result = result.replace(&var_ref, &value);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_variable_storage() {
        let mut ctx = Context::new(Config::default(), ExecutionMode::Validate);
        ctx.set_variable("$container_id", Value::String("abc123".into()));
        assert!(ctx.has_variable("$container_id"));
        assert!(
            matches!(ctx.get_variable("$container_id"), Some(Value::String(s)) if s == "abc123")
        );
    }

    #[test]
    fn test_interpolation() {
        let mut ctx = Context::new(Config::default(), ExecutionMode::Validate);
        ctx.set_variable("$job_id", Value::String("abc-123".into()));

        let result = ctx.interpolate("/jobs/$job_id");
        assert_eq!(result, "/jobs/abc-123");
    }

    #[test]
    fn test_interpolation_multiple() {
        let mut ctx = Context::new(Config::default(), ExecutionMode::Validate);
        ctx.set_variable("$host", Value::String("localhost".into()));
        ctx.set_variable("$port", Value::Int(8080));

        let result = ctx.interpolate("http://$host:$port/api");
        assert_eq!(result, "http://localhost:8080/api");
    }

    #[test]
    fn test_interpolation_prefers_the_longer_name() {
        // `$BIN` is a prefix of `$BIN_PATH`; whichever the map yields first,
        // the longer one must win. Repeated because HashMap order varies.
        for _ in 0..50 {
            let mut ctx = Context::new(Config::default(), ExecutionMode::Validate);
            ctx.set_variable("BIN", Value::String("seachart".into()));
            ctx.set_variable("BIN_PATH", Value::String("./seachart".into()));

            let result = ctx.interpolate("$BIN_PATH --encode google.com && $BIN -v");
            assert_eq!(result, "./seachart --encode google.com && seachart -v");
        }
    }

    #[test]
    fn test_user_input() {
        let mut ctx = Context::new(Config::default(), ExecutionMode::Result);
        ctx.set_user_input("container-id", "abc123");
        assert_eq!(ctx.get_user_input("container-id"), Some("abc123"));
    }
}
