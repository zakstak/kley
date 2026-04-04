use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Result;

use crate::diagnostics::{Diagnostic, DiagnosticSeverity};

const REPO_SLUG: &str = "zakstak/kley";

pub fn run(writer: &mut impl Write) -> Result<bool> {
    let env = RuntimeEnv::detect();
    let runner = ProcessRunner;
    let report = build_report(&runner, &env);
    report.write_to(writer)?;
    Ok(report.fail == 0)
}

fn build_report(runner: &impl CommandRunner, env: &RuntimeEnv) -> Report {
    let mut report = Report::default();
    let selected_remote = select_remote(runner);
    let launcher = resolve_launcher(env);

    report.push_line(format!("── Running from: {} ──", env.current_dir.display()));
    report.push_line("   Environment: Host");
    report.push_line(format!(
        "   Git user:  {}",
        command_value_or(runner, git_config_command("user.name"), "(not set)")
    ));
    report.push_line(format!(
        "   Git email: {}",
        command_value_or(runner, git_config_command("user.email"), "(not set)")
    ));
    report.push_line(format!(
        "   GitHub:    {}",
        command_value_or(
            runner,
            command("gh").args(["api", "user", "--jq", ".login"]),
            "(not authenticated)",
        )
    ));
    report.push_line(format!(
        "   Remote:    {}",
        selected_remote.as_deref().unwrap_or("(none reachable)")
    ));
    report.blank_line();

    report.push_line("── Git access checks ──");
    report.required(
        "git is installed",
        runner.run(&command("git").arg("--version")).success,
    );
    report.required(
        "inside a git repo",
        runner
            .run(&command("git").args(["rev-parse", "--is-inside-work-tree"]))
            .success,
    );
    report.required("upstream/origin exists", remote_exists(runner));
    report.required("can fetch from a remote", selected_remote.is_some());
    report.blank_line();

    report.push_line("── GitHub CLI checks ──");
    report.required(
        "gh is installed",
        runner.run(&command("gh").arg("--version")).success,
    );
    report.required(
        "gh is authenticated",
        runner.run(&command("gh").args(["auth", "status"])).success,
    );
    report.required(
        "can list PRs on repo",
        runner
            .run(&command("gh").args(["pr", "list", "--repo", REPO_SLUG, "--limit", "1"]))
            .success,
    );
    report.blank_line();

    report.push_line("── Rust toolchain ──");
    report.required(
        "cargo is installed",
        runner.run(&command("cargo").arg("--version")).success,
    );
    report.required(
        "cargo fmt available",
        runner
            .run(&command("cargo").args(["fmt", "--version"]))
            .success,
    );
    report.required(
        "cargo clippy available",
        runner
            .run(&command("cargo").args(["clippy", "--version"]))
            .success,
    );
    report.required(
        "kley binary works",
        runner.run(&launcher.help_command()).success,
    );
    report.blank_line();

    report.push_line("── Dev toolchain checks ──");
    report.optional(
        "gcc is installed",
        runner.run(&command("gcc").arg("--version")).success,
    );
    report.optional(
        "make is installed",
        runner.run(&command("make").arg("--version")).success,
    );
    report.optional(
        "cmake is installed",
        runner.run(&command("cmake").arg("--version")).success,
    );
    report.optional(
        "node is installed",
        runner.run(&command("node").arg("--version")).success,
    );
    report.optional(
        "npm is installed",
        runner.run(&command("npm").arg("--version")).success,
    );
    report.optional(
        "go is installed",
        runner.run(&command("go").arg("version")).success,
    );
    report.optional(
        "python3 is installed",
        runner.run(&command("python3").arg("--version")).success,
    );
    report.optional(
        "sqlite3 is installed",
        runner.run(&command("sqlite3").arg("--version")).success,
    );
    report.optional(
        "shellcheck is installed",
        runner.run(&command("shellcheck").arg("--version")).success,
    );
    report.optional(
        "tree is installed",
        runner.run(&command("tree").arg("--version")).success,
    );
    report.optional(
        "jq is installed",
        runner.run(&command("jq").arg("--version")).success,
    );
    report.optional(
        "fd is installed",
        runner.run(&command("fd").arg("--version")).success,
    );
    report.optional(
        "bat is installed",
        runner.run(&command("bat").arg("--version")).success,
    );
    report.blank_line();

    report.push_line("── LSPs ──");
    run_required_lsp_checks(&mut report, runner);
    report.blank_line();

    report.push_line("── Linters & Formatters ──");
    report.optional(
        "golangci-lint",
        runner
            .run(&command("golangci-lint").arg("--version"))
            .success,
    );
    report.optional(
        "prettier",
        runner.run(&command("prettier").arg("--version")).success,
    );
    report.optional(
        "gitleaks",
        runner.run(&command("gitleaks").arg("version")).success,
    );
    report.optional(
        "cargo-nextest",
        runner
            .run(&command("cargo").args(["nextest", "--version"]))
            .success,
    );
    report.blank_line();

    report.push_line("━━━━━━━━━━━━━━━━━━━━━━");
    report.push_line(format!(
        "  Passed: {}  Failed: {}  Warnings: {}",
        report.pass, report.fail, report.warn
    ));
    report.push_line("━━━━━━━━━━━━━━━━━━━━━━");
    report.blank_line();

    if report.fail > 0 {
        report.push_line("⚠ Fix the failing checks above before continuing.");
    } else {
        report.push_line("✓ All checks passed.");
    }

    report
}

#[derive(Clone, Copy, Debug)]
pub struct LspRequirement {
    pub id: &'static str,
    pub program: &'static str,
    pub args: &'static [&'static str],
}

const LSP_REQUIREMENTS: [LspRequirement; 6] = [
    LspRequirement {
        id: "rust-analyzer",
        program: "rust-analyzer",
        args: &["--version"],
    },
    LspRequirement {
        id: "gopls",
        program: "gopls",
        args: &["version"],
    },
    LspRequirement {
        id: "bash-language-server",
        program: "bash-language-server",
        args: &["--version"],
    },
    LspRequirement {
        id: "nixd",
        program: "nixd",
        args: &["--version"],
    },
    LspRequirement {
        id: "yaml-language-server",
        program: "yaml-language-server",
        args: &["--version"],
    },
    LspRequirement {
        id: "pyright",
        program: "pyright",
        args: &["--version"],
    },
];

pub struct LspCheckResult {
    pub id: &'static str,
    pub success: bool,
}

pub fn lsp_requirements() -> &'static [LspRequirement] {
    &LSP_REQUIREMENTS
}

pub fn command_for_lsp_requirement(requirement: &LspRequirement) -> CommandSpec {
    command(requirement.program).args(requirement.args.iter().copied())
}

pub fn run_required_lsp_checks_with_runner(runner: &impl CommandRunner) -> Vec<LspCheckResult> {
    LSP_REQUIREMENTS
        .iter()
        .map(|requirement| {
            let spec = command_for_lsp_requirement(requirement);
            let success = runner.run(&spec).success;
            LspCheckResult {
                id: requirement.id,
                success,
            }
        })
        .collect()
}

fn run_required_lsp_checks(report: &mut Report, runner: &impl CommandRunner) {
    for result in run_required_lsp_checks_with_runner(runner) {
        report.required(result.id, result.success);
    }
}

fn remote_exists(runner: &impl CommandRunner) -> bool {
    runner
        .run(&command("git").args(["remote", "get-url", "upstream"]))
        .success
        || runner
            .run(&command("git").args(["remote", "get-url", "origin"]))
            .success
}

fn select_remote(runner: &impl CommandRunner) -> Option<String> {
    for remote in ["origin", "upstream"] {
        if runner.run(&remote_probe_command(remote)).success {
            return Some(remote.to_string());
        }
    }

    None
}

fn command_value_or(runner: &impl CommandRunner, spec: CommandSpec, default: &str) -> String {
    let output = runner.run(&spec);
    if output.success {
        let value = output.stdout.trim();
        if !value.is_empty() {
            return value.lines().next().unwrap_or(value).trim().to_string();
        }
    }

    default.to_string()
}

fn git_config_command(key: &str) -> CommandSpec {
    command("git").args(["config", key])
}

fn remote_probe_command(remote: &str) -> CommandSpec {
    command("git")
        .args(["ls-remote", remote, "HEAD"])
        .env("GIT_TERMINAL_PROMPT", "0")
}

fn command(program: impl Into<String>) -> CommandSpec {
    CommandSpec::new(program)
}

fn resolve_launcher(env: &RuntimeEnv) -> KleyLauncher {
    if let Some(manifest_path) = find_repo_manifest(&env.current_dir) {
        return KleyLauncher::Cargo { manifest_path };
    }

    if let Some(manifest_path) = find_repo_manifest(&env.current_exe) {
        return KleyLauncher::Cargo { manifest_path };
    }

    KleyLauncher::PathBinary
}

fn find_repo_manifest(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    };

    start.ancestors().find_map(|ancestor| {
        let manifest_path = ancestor.join("Cargo.toml");
        if is_kley_repo_manifest(&manifest_path) {
            Some(manifest_path)
        } else {
            None
        }
    })
}

fn is_kley_repo_manifest(manifest_path: &Path) -> bool {
    let Ok(contents) = std::fs::read_to_string(manifest_path) else {
        return false;
    };

    manifest_declares_kley_package(&contents)
}

fn manifest_declares_kley_package(contents: &str) -> bool {
    let mut in_package = false;

    for line in contents.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_package = trimmed == "[package]";
            continue;
        }

        if !in_package || trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };

        if key.trim() == "name" {
            let value = value.trim();
            return value == "\"kley\"" || value == "'kley'";
        }
    }

    false
}

fn cargo_launcher_help_command(manifest_path: &Path) -> CommandSpec {
    command("cargo")
        .args(["run", "--quiet", "--manifest-path"])
        .arg(manifest_path.to_string_lossy().into_owned())
        .args(["--bin", "kley", "--", "--help"])
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RuntimeEnv {
    current_dir: PathBuf,
    current_exe: PathBuf,
}

impl RuntimeEnv {
    fn detect() -> Self {
        Self {
            current_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            current_exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("kley")),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum KleyLauncher {
    Cargo { manifest_path: PathBuf },
    PathBinary,
}

impl KleyLauncher {
    fn help_command(&self) -> CommandSpec {
        match self {
            Self::Cargo { manifest_path } => cargo_launcher_help_command(manifest_path),
            Self::PathBinary => command("kley").arg("--help"),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Report {
    lines: Vec<String>,
    checks: Vec<PreflightCheck>,
    pass: usize,
    fail: usize,
    warn: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PreflightCheck {
    label: String,
    required: bool,
    success: bool,
    diagnostics: Vec<Diagnostic>,
}

impl PreflightCheck {
    fn new(label: impl Into<String>, required: bool, success: bool) -> Self {
        let label = label.into();
        let (code, severity) = match (required, success) {
            (_, true) => ("preflight.check.ok", DiagnosticSeverity::Info),
            (true, false) => ("preflight.check.failed", DiagnosticSeverity::Error),
            (false, false) => (
                "preflight.check.optional_missing",
                DiagnosticSeverity::Warning,
            ),
        };

        Self {
            label: label.clone(),
            required,
            success,
            diagnostics: vec![
                Diagnostic::new(code, severity, label, "preflight").with_details(
                    serde_json::json!({
                        "required": required,
                        "success": success,
                    }),
                ),
            ],
        }
    }
}

impl Report {
    fn push_line(&mut self, line: impl Into<String>) {
        self.lines.push(line.into());
    }

    fn blank_line(&mut self) {
        self.lines.push(String::new());
    }

    fn record_check(&mut self, check: PreflightCheck) {
        if check.success {
            self.pass += 1;
            self.push_line(format!("  ✓ {}", check.label));
        } else if check.required {
            self.fail += 1;
            self.push_line(format!("  ✗ {}", check.label));
        } else {
            self.warn += 1;
            self.push_line(format!("  ⚠ {} (optional)", check.label));
        }
        self.checks.push(check);
    }

    fn required(&mut self, label: &str, success: bool) {
        self.record_check(PreflightCheck::new(label, true, success));
    }

    fn optional(&mut self, label: &str, success: bool) {
        self.record_check(PreflightCheck::new(label, false, success));
    }

    fn write_to(&self, writer: &mut impl Write) -> io::Result<()> {
        for line in &self.lines {
            writeln!(writer, "{line}")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandSpec {
    program: String,
    args: Vec<String>,
    envs: Vec<(String, String)>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            envs: Vec::new(),
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.envs.push((key.into(), value.into()));
        self
    }

    pub fn key(&self) -> String {
        format!("{}|{:?}|{:?}", self.program, self.args, self.envs)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub exit_code: Option<i32>,
}

impl CommandOutput {
    pub fn success_with_stdout(stdout: impl Into<String>) -> Self {
        Self {
            success: true,
            stdout: stdout.into(),
            stderr: String::new(),
            exit_code: Some(0),
        }
    }

    pub fn success() -> Self {
        Self::success_with_stdout("")
    }

    pub fn failure() -> Self {
        Self::default()
    }
}

pub trait CommandRunner {
    fn run(&self, spec: &CommandSpec) -> CommandOutput;
}

struct ProcessRunner;

impl CommandRunner for ProcessRunner {
    fn run(&self, spec: &CommandSpec) -> CommandOutput {
        let output = Command::new(&spec.program)
            .args(&spec.args)
            .envs(spec.envs.iter().map(|(key, value)| (key, value)))
            .output();

        match output {
            Ok(output) => CommandOutput {
                success: output.status.success(),
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                exit_code: output.status.code(),
            },
            Err(_) => CommandOutput::default(),
        }
    }
}

pub mod preflight_test_support {

    use std::cell::RefCell;
    use std::collections::{HashMap, VecDeque};

    pub use super::{CommandOutput, CommandRunner, CommandSpec};
    pub use super::{
        LspCheckResult, LspRequirement, command_for_lsp_requirement, lsp_requirements,
        run_required_lsp_checks_with_runner,
    };

    pub struct FakeRunner {
        responses: RefCell<HashMap<String, VecDeque<CommandOutput>>>,
    }

    impl FakeRunner {
        pub fn new(entries: Vec<(CommandSpec, CommandOutput)>) -> Self {
            let mut responses: HashMap<String, VecDeque<CommandOutput>> = HashMap::new();
            for (spec, output) in entries {
                responses.entry(spec.key()).or_default().push_back(output);
            }

            Self {
                responses: RefCell::new(responses),
            }
        }
    }

    impl CommandRunner for FakeRunner {
        fn run(&self, spec: &CommandSpec) -> CommandOutput {
            self.responses
                .borrow_mut()
                .get_mut(&spec.key())
                .and_then(VecDeque::pop_front)
                .unwrap_or_else(|| panic!("unexpected command: {}", spec.key()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::preflight_test_support::FakeRunner;
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn select_remote_prefers_origin_without_probing_upstream() {
        let runner = FakeRunner::new(vec![(
            remote_probe_command("origin"),
            CommandOutput::success(),
        )]);

        assert_eq!(select_remote(&runner), Some("origin".to_string()));
    }

    #[test]
    fn optional_checks_always_warn_when_missing() {
        let mut report = Report::default();
        report.optional("cmake is installed", false);
        assert_eq!(report.warn, 1);
        assert_eq!(report.fail, 0);
        assert_eq!(report.lines, vec!["  ⚠ cmake is installed (optional)"]);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].diagnostics.len(), 1);
        assert_eq!(
            report.checks[0].diagnostics[0].severity,
            DiagnosticSeverity::Warning
        );
    }

    #[test]
    fn launcher_uses_repo_manifest_from_current_dir() {
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let work_dir = repo_root.join("nested/worktree");
        fs::create_dir_all(&work_dir).unwrap();
        fs::write(repo_root.join("Cargo.toml"), "[package]\nname = 'kley'\n").unwrap();

        let env = RuntimeEnv {
            current_dir: work_dir,
            current_exe: PathBuf::from("/usr/local/bin/kley"),
        };

        assert_eq!(
            resolve_launcher(&env),
            KleyLauncher::Cargo {
                manifest_path: repo_root.join("Cargo.toml")
            }
        );
    }

    #[test]
    fn launcher_uses_repo_manifest_from_current_exe() {
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let exe_dir = repo_root.join("target/debug");
        fs::create_dir_all(&exe_dir).unwrap();
        fs::write(repo_root.join("Cargo.toml"), "[package]\nname = 'kley'\n").unwrap();

        let env = RuntimeEnv {
            current_dir: PathBuf::from("/tmp"),
            current_exe: exe_dir.join("kley"),
        };

        assert_eq!(
            resolve_launcher(&env),
            KleyLauncher::Cargo {
                manifest_path: repo_root.join("Cargo.toml")
            }
        );
    }

    #[test]
    fn launcher_falls_back_to_path_binary_without_repo_manifest() {
        let env = RuntimeEnv {
            current_dir: PathBuf::from("/tmp"),
            current_exe: PathBuf::from("/usr/local/bin/kley"),
        };

        assert_eq!(resolve_launcher(&env), KleyLauncher::PathBinary);
        assert_eq!(
            resolve_launcher(&env).help_command(),
            command("kley").arg("--help")
        );
    }

    #[test]
    fn launcher_falls_back_to_path_binary_for_unrelated_rust_repo() {
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let work_dir = repo_root.join("nested/worktree");
        fs::create_dir_all(&work_dir).unwrap();
        fs::write(
            repo_root.join("Cargo.toml"),
            "[package]\nname = 'not-kley'\n",
        )
        .unwrap();

        let env = RuntimeEnv {
            current_dir: work_dir,
            current_exe: PathBuf::from("/usr/local/bin/kley"),
        };

        assert_eq!(resolve_launcher(&env), KleyLauncher::PathBinary);
    }

    #[test]
    fn report_rendering_includes_summary_and_failure_guidance() {
        let temp = tempdir().unwrap();
        let repo_root = temp.path();
        let work_dir = repo_root.join("nested/worktree");
        let manifest_path = repo_root.join("Cargo.toml");

        fs::create_dir_all(&work_dir).unwrap();
        fs::write(&manifest_path, "[package]\nname = 'kley'\n").unwrap();

        let env = RuntimeEnv {
            current_dir: work_dir.clone(),
            current_exe: PathBuf::from("/tmp/kley"),
        };
        let expected_kley_help_command = resolve_launcher(&env).help_command();

        let runner = FakeRunner::new(vec![
            (remote_probe_command("upstream"), CommandOutput::failure()),
            (remote_probe_command("origin"), CommandOutput::failure()),
            (
                git_config_command("user.name"),
                CommandOutput::success_with_stdout("saga\n"),
            ),
            (
                git_config_command("user.email"),
                CommandOutput::success_with_stdout("saga@example.com\n"),
            ),
            (
                command("gh").args(["api", "user", "--jq", ".login"]),
                CommandOutput::success_with_stdout("saga-agent\n"),
            ),
            (command("git").arg("--version"), CommandOutput::success()),
            (
                command("git").args(["rev-parse", "--is-inside-work-tree"]),
                CommandOutput::success(),
            ),
            (
                command("git").args(["remote", "get-url", "upstream"]),
                CommandOutput::failure(),
            ),
            (
                command("git").args(["remote", "get-url", "origin"]),
                CommandOutput::success(),
            ),
            (command("gh").arg("--version"), CommandOutput::success()),
            (
                command("gh").args(["auth", "status"]),
                CommandOutput::success(),
            ),
            (
                command("gh").args(["pr", "list", "--repo", REPO_SLUG, "--limit", "1"]),
                CommandOutput::success(),
            ),
            (command("cargo").arg("--version"), CommandOutput::success()),
            (
                command("cargo").args(["fmt", "--version"]),
                CommandOutput::success(),
            ),
            (
                command("cargo").args(["clippy", "--version"]),
                CommandOutput::success(),
            ),
            (expected_kley_help_command, CommandOutput::failure()),
            (command("gcc").arg("--version"), CommandOutput::success()),
            (command("make").arg("--version"), CommandOutput::success()),
            (command("cmake").arg("--version"), CommandOutput::failure()),
            (command("node").arg("--version"), CommandOutput::success()),
            (command("npm").arg("--version"), CommandOutput::success()),
            (command("go").arg("version"), CommandOutput::success()),
            (
                command("python3").arg("--version"),
                CommandOutput::success(),
            ),
            (
                command("sqlite3").arg("--version"),
                CommandOutput::success(),
            ),
            (
                command("shellcheck").arg("--version"),
                CommandOutput::success(),
            ),
            (command("tree").arg("--version"), CommandOutput::success()),
            (command("jq").arg("--version"), CommandOutput::success()),
            (command("fd").arg("--version"), CommandOutput::success()),
            (command("bat").arg("--version"), CommandOutput::success()),
            (
                command("rust-analyzer").arg("--version"),
                CommandOutput::success(),
            ),
            (command("gopls").arg("version"), CommandOutput::success()),
            (
                command("bash-language-server").arg("--version"),
                CommandOutput::success(),
            ),
            (command("nixd").arg("--version"), CommandOutput::success()),
            (
                command("yaml-language-server").arg("--version"),
                CommandOutput::success(),
            ),
            (
                command("pyright").arg("--version"),
                CommandOutput::success(),
            ),
            (
                command("golangci-lint").arg("--version"),
                CommandOutput::success(),
            ),
            (
                command("prettier").arg("--version"),
                CommandOutput::success(),
            ),
            (command("gitleaks").arg("version"), CommandOutput::success()),
            (
                command("cargo").args(["nextest", "--version"]),
                CommandOutput::success(),
            ),
        ]);

        let report = build_report(&runner, &env);
        let mut rendered = Vec::new();
        report.write_to(&mut rendered).unwrap();
        let text = String::from_utf8(rendered).unwrap();

        assert!(text.contains(&format!("── Running from: {} ──", work_dir.display())));
        assert!(text.contains("   GitHub:    saga-agent"));
        assert!(text.contains("   Remote:    (none reachable)"));
        assert!(text.contains("  ✗ can fetch from a remote"));
        assert!(text.contains("  ✗ kley binary works"));
        assert!(text.contains("  ⚠ cmake is installed (optional)"));
        assert!(text.contains("  Passed: 31  Failed: 2  Warnings: 1"));
        assert!(text.contains("⚠ Fix the failing checks above before continuing."));
    }

    #[test]
    fn preflight_requires_all_v1_lsp_servers() {
        let runner = FakeRunner::new(
            LSP_REQUIREMENTS
                .iter()
                .map(|check| (command_for_lsp_requirement(check), CommandOutput::failure()))
                .collect(),
        );

        let mut report = Report::default();
        run_required_lsp_checks(&mut report, &runner);

        assert_eq!(report.fail, LSP_REQUIREMENTS.len());
        assert_eq!(report.pass, 0);
    }

    #[test]
    fn preflight_reports_each_missing_lsp_binary_by_name() {
        let runner = FakeRunner::new(
            LSP_REQUIREMENTS
                .iter()
                .map(|check| (command_for_lsp_requirement(check), CommandOutput::failure()))
                .collect(),
        );

        let mut report = Report::default();
        run_required_lsp_checks(&mut report, &runner);

        assert_eq!(report.fail, LSP_REQUIREMENTS.len());
        for check in &LSP_REQUIREMENTS {
            assert!(
                report.lines.iter().any(|line| line.contains(check.id)),
                "missing report line for {}",
                check.id
            );
        }
    }
}
