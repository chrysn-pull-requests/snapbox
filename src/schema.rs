//! `cmd.toml` Schema
//!
//! [`OneShot`] is the top-level item in the `cmd.toml` files.

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::io::prelude::*;

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct TryCmd {
    pub(crate) run: Run,
    pub(crate) fs: Filesystem,
}

impl TryCmd {
    pub(crate) fn load(path: &std::path::Path) -> Result<Self, String> {
        let mut sequence = if let Some(ext) = path.extension() {
            if ext == std::ffi::OsStr::new("toml") {
                let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                let one_shot = OneShot::parse_toml(&raw)?;
                let mut sequence: Self = one_shot.into();

                let stdin_path = path.with_extension("stdin");
                let stdin = if stdin_path.exists() {
                    Some(crate::File::read_from(&stdin_path, sequence.run.binary)?)
                } else {
                    None
                };
                sequence.run.stdin = stdin;

                let stdout_path = path.with_extension("stdout");
                let stdout = if stdout_path.exists() {
                    Some(crate::File::read_from(&stdout_path, sequence.run.binary)?)
                } else {
                    None
                };
                sequence.run.expected_stdout = stdout;

                let stderr_path = path.with_extension("stderr");
                let stderr = if stderr_path.exists() {
                    Some(crate::File::read_from(&stderr_path, sequence.run.binary)?)
                } else {
                    None
                };
                sequence.run.expected_stderr = stderr;

                sequence
            } else if ext == std::ffi::OsStr::new("trycmd") {
                let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                Self::parse_trycmd(&raw)?
            } else {
                return Err(format!("Unsupported extension: {}", ext.to_string_lossy()));
            }
        } else {
            return Err("No extension".into());
        };

        if let Some(cwd) = sequence.fs.cwd.take() {
            sequence.fs.cwd = Some(
                path.parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .join(cwd),
            );
        }
        if sequence.fs.base.is_none() {
            let base_path = path.with_extension("in");
            if base_path.exists() {
                sequence.fs.base = Some(base_path);
            } else if sequence.fs.cwd.is_some() {
                sequence.fs.base = sequence.fs.cwd.clone();
            }
        }
        if sequence.fs.cwd.is_none() {
            sequence.fs.cwd = sequence.fs.base.clone();
        }
        if sequence.fs.sandbox.is_none() {
            sequence.fs.sandbox = Some(path.with_extension("out").exists());
        }

        Ok(sequence)
    }

    fn parse_trycmd(s: &str) -> Result<Self, String> {
        let mut cmdline = Vec::new();
        let mut status = Some(CommandStatus::Success);
        let mut stdout = String::new();

        let mut lines: VecDeque<_> = crate::lines::LinesWithTerminator::new(s).collect();
        if let Some(line) = lines.pop_front() {
            if let Some(raw) = line.strip_prefix("$ ") {
                cmdline.extend(shlex::Shlex::new(raw.trim()));
            } else {
                return Err(format!("Expected `$` line, got `{}`", line));
            }
        }
        while let Some(line) = lines.pop_front() {
            if let Some(raw) = line.strip_prefix("> ") {
                cmdline.extend(shlex::Shlex::new(raw.trim()));
            } else {
                lines.push_front(line);
                break;
            }
        }
        if let Some(line) = lines.pop_front() {
            if let Some(raw) = line.strip_prefix("? ") {
                status = Some(raw.trim().parse::<CommandStatus>()?);
            } else {
                lines.push_front(line);
            }
        }
        if !lines.is_empty() {
            stdout.extend(lines);
        }

        let mut env = Env::default();

        let bin = loop {
            if cmdline.is_empty() {
                return Err(String::from("No bin specified"));
            }
            let next = cmdline.remove(0);
            if let Some((key, value)) = next.split_once('=') {
                env.add.insert(key.to_owned(), value.to_owned());
            } else {
                break next;
            }
        };
        let run = Run {
            bin: Some(Bin::Name(bin)),
            args: cmdline,
            env,
            status,
            stderr_to_stdout: true,
            expected_stdout: Some(crate::File::Text(stdout)),
            ..Default::default()
        };
        Ok(Self {
            run,
            ..Default::default()
        })
    }
}

impl std::str::FromStr for TryCmd {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_trycmd(s)
    }
}

impl From<OneShot> for TryCmd {
    fn from(other: OneShot) -> Self {
        let OneShot {
            bin,
            args,
            env,
            stderr_to_stdout,
            status,
            binary,
            timeout,
            fs,
        } = other;
        Self {
            run: Run {
                bin,
                args: args.into_vec(),
                env,
                stdin: None,
                stderr_to_stdout,
                status,
                expected_stdout: None,
                expected_stderr: None,
                binary,
                timeout,
            },
            fs,
        }
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub(crate) bin: Option<Bin>,
    pub(crate) args: Vec<String>,
    pub(crate) env: Env,
    pub(crate) stdin: Option<crate::File>,
    pub(crate) stderr_to_stdout: bool,
    pub(crate) status: Option<CommandStatus>,
    pub(crate) expected_stdout: Option<crate::File>,
    pub(crate) expected_stderr: Option<crate::File>,
    pub(crate) binary: bool,
    pub(crate) timeout: Option<std::time::Duration>,
}

impl Run {
    pub(crate) fn to_command(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<std::process::Command, String> {
        let bin = match &self.bin {
            Some(Bin::Path(path)) => Ok(path.clone()),
            Some(Bin::Name(name)) => Err(format!("Unknown bin.name = {}", name)),
            Some(Bin::Error(err)) => Err(err.clone().into_string()),
            None => Err(String::from("No bin specified")),
        }?;
        if !bin.exists() {
            return Err(format!("Bin doesn't exist: {}", bin.display()));
        }

        let mut cmd = std::process::Command::new(bin);
        cmd.args(&self.args);
        if let Some(cwd) = cwd {
            cmd.current_dir(cwd);
        }
        self.env.apply(&mut cmd);

        Ok(cmd)
    }

    pub(crate) fn to_output(
        &self,
        cwd: Option<&std::path::Path>,
    ) -> Result<std::process::Output, String> {
        let mut cmd = self.to_command(cwd)?;

        if self.stderr_to_stdout {
            cmd.stdin(std::process::Stdio::piped());
            let (mut reader, writer) = os_pipe::pipe().map_err(|e| e.to_string())?;
            let writer_clone = writer.try_clone().map_err(|e| e.to_string())?;
            cmd.stdout(writer);
            cmd.stderr(writer_clone);
            let child = cmd.spawn().map_err(|e| e.to_string())?;

            // Avoid a deadlock! This parent process is still holding open pipe
            // writers (inside the Command object), and we have to close those
            // before we read. Here we do this by dropping the Command object.
            drop(cmd);

            let mut output = crate::wait_with_input_output(child, self.stdin(), self.timeout)
                .map_err(|e| e.to_string())?;
            assert!(output.stdout.is_empty());
            assert!(output.stderr.is_empty());
            reader
                .read_to_end(&mut output.stdout)
                .map_err(|e| e.to_string())?;
            Ok(output)
        } else {
            cmd.stdin(std::process::Stdio::piped());
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            let child = cmd.spawn().map_err(|e| e.to_string())?;
            crate::wait_with_input_output(child, self.stdin(), self.timeout)
                .map_err(|e| e.to_string())
        }
    }

    pub(crate) fn status(&self) -> CommandStatus {
        self.status.unwrap_or_default()
    }

    pub(crate) fn stdin(&self) -> Option<&[u8]> {
        self.stdin.as_ref().map(|f| f.as_bytes())
    }
}

/// Top-level data in `cmd.toml` files
#[derive(Clone, Default, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct OneShot {
    pub(crate) bin: Option<Bin>,
    #[serde(default)]
    pub(crate) args: Args,
    #[serde(default)]
    pub(crate) env: Env,
    #[serde(default)]
    pub(crate) stderr_to_stdout: bool,
    pub(crate) status: Option<CommandStatus>,
    #[serde(default)]
    pub(crate) binary: bool,
    #[serde(default)]
    #[serde(deserialize_with = "humantime_serde::deserialize")]
    pub(crate) timeout: Option<std::time::Duration>,
    #[serde(default)]
    pub(crate) fs: Filesystem,
}

impl OneShot {
    fn parse_toml(s: &str) -> Result<Self, String> {
        toml_edit::de::from_str(s).map_err(|e| e.to_string())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub(crate) enum Args {
    Joined(JoinedArgs),
    Split(Vec<String>),
}

impl Args {
    fn new() -> Self {
        Self::Split(Default::default())
    }

    fn as_slice(&self) -> &[String] {
        match self {
            Self::Joined(j) => j.inner.as_slice(),
            Self::Split(v) => v.as_slice(),
        }
    }

    fn into_vec(self) -> Vec<String> {
        match self {
            Self::Joined(j) => j.inner,
            Self::Split(v) => v,
        }
    }
}

impl Default for Args {
    fn default() -> Self {
        Self::new()
    }
}

impl std::ops::Deref for Args {
    type Target = [String];

    fn deref(&self) -> &Self::Target {
        self.as_slice()
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub(crate) struct JoinedArgs {
    inner: Vec<String>,
}

impl JoinedArgs {
    #[cfg(test)]
    pub(crate) fn from_vec(inner: Vec<String>) -> Self {
        JoinedArgs { inner }
    }

    #[allow(clippy::inherent_to_string_shadow_display)]
    fn to_string(&self) -> String {
        shlex::join(self.inner.iter().map(|s| s.as_str()))
    }
}

impl std::str::FromStr for JoinedArgs {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let inner = shlex::Shlex::new(s).collect();
        Ok(Self { inner })
    }
}

impl std::fmt::Display for JoinedArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.to_string().fmt(f)
    }
}

impl<'de> serde::de::Deserialize<'de> for JoinedArgs {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        std::str::FromStr::from_str(&s).map_err(serde::de::Error::custom)
    }
}

impl serde::ser::Serialize for JoinedArgs {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::ser::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// Describe command's the filesystem context
#[derive(Clone, Default, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Filesystem {
    pub(crate) cwd: Option<std::path::PathBuf>,
    /// Sandbox base
    pub(crate) base: Option<std::path::PathBuf>,
    pub(crate) sandbox: Option<bool>,
}

impl Filesystem {
    pub(crate) fn sandbox(&self) -> bool {
        self.sandbox.unwrap_or_default()
    }

    pub(crate) fn rel_cwd(&self) -> Result<&std::path::Path, String> {
        if let (Some(orig_cwd), Some(orig_base)) = (self.cwd.as_deref(), self.base.as_deref()) {
            let rel_cwd = orig_cwd.strip_prefix(orig_base).map_err(|_| {
                format!(
                    "fs.cwd ({}) must be within fs.base ({})",
                    orig_cwd.display(),
                    orig_base.display()
                )
            })?;
            Ok(rel_cwd)
        } else {
            Ok(std::path::Path::new(""))
        }
    }
}

/// Describe command's environment
#[derive(Clone, Default, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct Env {
    #[serde(default)]
    pub(crate) inherit: Option<bool>,
    #[serde(default)]
    pub(crate) add: BTreeMap<String, String>,
    #[serde(default)]
    pub(crate) remove: Vec<String>,
}

impl Env {
    pub(crate) fn update(&mut self, other: &Self) {
        if self.inherit.is_none() {
            self.inherit = other.inherit;
        }
        self.add
            .extend(other.add.iter().map(|(k, v)| (k.clone(), v.clone())));
        self.remove.extend(other.remove.iter().cloned());
    }

    pub(crate) fn apply(&self, command: &mut std::process::Command) {
        if !self.inherit() {
            command.env_clear();
        }
        for remove in &self.remove {
            command.env_remove(&remove);
        }
        command.envs(&self.add);
    }

    pub(crate) fn inherit(&self) -> bool {
        self.inherit.unwrap_or(true)
    }
}

/// Target under test
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Bin {
    Path(std::path::PathBuf),
    Name(String),
    #[serde(skip)]
    Error(crate::Error),
}

impl From<std::path::PathBuf> for Bin {
    fn from(other: std::path::PathBuf) -> Self {
        Self::Path(other)
    }
}

impl<'a> From<&'a std::path::PathBuf> for Bin {
    fn from(other: &'a std::path::PathBuf) -> Self {
        Self::Path(other.clone())
    }
}

impl<'a> From<&'a std::path::Path> for Bin {
    fn from(other: &'a std::path::Path) -> Self {
        Self::Path(other.to_owned())
    }
}

impl<P, E> From<Result<P, E>> for Bin
where
    P: Into<Bin>,
    E: std::fmt::Display,
{
    fn from(other: Result<P, E>) -> Self {
        match other {
            Ok(path) => path.into(),
            Err(err) => {
                let err = crate::Error::new(err.to_string());
                Bin::Error(err)
            }
        }
    }
}

/// Expected status for command
#[derive(Copy, Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum CommandStatus {
    Success,
    Failed,
    Interrupted,
    Skipped,
    Code(i32),
}

impl Default for CommandStatus {
    fn default() -> Self {
        CommandStatus::Success
    }
}

impl std::str::FromStr for CommandStatus {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            "skipped" => Ok(Self::Skipped),
            _ => s
                .parse::<i32>()
                .map(Self::Code)
                .map_err(|_| format!("Expected an exit code, got {}", s)),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn parse_trycmd_command() {
        let expected = TryCmd {
            run: Run {
                bin: Some(Bin::Name("cmd".into())),
                status: Some(CommandStatus::Success),
                stderr_to_stdout: true,
                expected_stdout: Some(crate::File::Text("".into())),
                expected_stderr: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let actual = TryCmd::parse_trycmd("$ cmd").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_trycmd_command_line() {
        let expected = TryCmd {
            run: Run {
                bin: Some(Bin::Name("cmd".into())),
                args: vec!["arg1".into(), "arg with space".into()],
                status: Some(CommandStatus::Success),
                stderr_to_stdout: true,
                expected_stdout: Some(crate::File::Text("".into())),
                expected_stderr: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let actual = TryCmd::parse_trycmd("$ cmd arg1 'arg with space'").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_trycmd_multi_line() {
        let expected = TryCmd {
            run: Run {
                bin: Some(Bin::Name("cmd".into())),
                args: vec!["arg1".into(), "arg with space".into()],
                status: Some(CommandStatus::Success),
                stderr_to_stdout: true,
                expected_stdout: Some(crate::File::Text("".into())),
                expected_stderr: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let actual = TryCmd::parse_trycmd("$ cmd arg1\n> 'arg with space'").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_trycmd_env() {
        let expected = TryCmd {
            run: Run {
                bin: Some(Bin::Name("cmd".into())),
                env: Env {
                    add: IntoIterator::into_iter([
                        ("KEY1".into(), "VALUE1".into()),
                        ("KEY2".into(), "VALUE2 with space".into()),
                    ])
                    .collect(),
                    ..Default::default()
                },
                status: Some(CommandStatus::Success),
                stderr_to_stdout: true,
                expected_stdout: Some(crate::File::Text("".into())),
                expected_stderr: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let actual = TryCmd::parse_trycmd("$ KEY1=VALUE1 KEY2='VALUE2 with space' cmd").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_trycmd_status() {
        let expected = TryCmd {
            run: Run {
                bin: Some(Bin::Name("cmd".into())),
                status: Some(CommandStatus::Skipped),
                stderr_to_stdout: true,
                expected_stdout: Some(crate::File::Text("".into())),
                expected_stderr: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let actual = TryCmd::parse_trycmd("$ cmd\n? skipped").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_trycmd_status_code() {
        let expected = TryCmd {
            run: Run {
                bin: Some(Bin::Name("cmd".into())),
                status: Some(CommandStatus::Code(-1)),
                stderr_to_stdout: true,
                expected_stdout: Some(crate::File::Text("".into())),
                expected_stderr: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let actual = TryCmd::parse_trycmd("$ cmd\n? -1").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_trycmd_stdout() {
        let expected = TryCmd {
            run: Run {
                bin: Some(Bin::Name("cmd".into())),
                status: Some(CommandStatus::Success),
                stderr_to_stdout: true,
                expected_stdout: Some(crate::File::Text("Hello World".into())),
                expected_stderr: None,
                ..Default::default()
            },
            ..Default::default()
        };
        let actual = TryCmd::parse_trycmd("$ cmd\nHello World").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_minimal() {
        let expected = OneShot {
            ..Default::default()
        };
        let actual = OneShot::parse_toml("").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_minimal_env() {
        let expected = OneShot {
            ..Default::default()
        };
        let actual = OneShot::parse_toml("[env]").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_bin_name() {
        let expected = OneShot {
            bin: Some(Bin::Name("cmd".into())),
            ..Default::default()
        };
        let actual = OneShot::parse_toml("bin.name = 'cmd'").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_bin_path() {
        let expected = OneShot {
            bin: Some(Bin::Path("/usr/bin/cmd".into())),
            ..Default::default()
        };
        let actual = OneShot::parse_toml("bin.path = '/usr/bin/cmd'").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_args_split() {
        let expected = OneShot {
            args: Args::Split(vec!["arg1".into(), "arg with space".into()]),
            ..Default::default()
        };
        let actual = OneShot::parse_toml(r#"args = ["arg1", "arg with space"]"#).unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_args_joined() {
        let expected = OneShot {
            args: Args::Joined(JoinedArgs::from_vec(vec![
                "arg1".into(),
                "arg with space".into(),
            ])),
            ..Default::default()
        };
        let actual = OneShot::parse_toml(r#"args = "arg1 'arg with space'""#).unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_status_success() {
        let expected = OneShot {
            status: Some(CommandStatus::Success),
            ..Default::default()
        };
        let actual = OneShot::parse_toml("status = 'success'").unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn parse_toml_status_code() {
        let expected = OneShot {
            status: Some(CommandStatus::Code(42)),
            ..Default::default()
        };
        let actual = OneShot::parse_toml("status.code = 42").unwrap();
        assert_eq!(expected, actual);
    }
}
