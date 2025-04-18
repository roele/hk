use crate::{Result, config::Steps, error::Error, step_job::StepJob, step_response::StepResponse};
use crate::{glob, settings::Settings};
use crate::{step_context::StepContext, tera};
use clx::progress::ProgressStatus;
use ensembler::CmdLineRunner;
use eyre::{WrapErr, eyre};
use indexmap::{IndexMap, IndexSet};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::{fmt, process::Stdio};

use serde_with::serde_as;

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde_as]
pub struct RunStep {
    pub r#type: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde_as(as = "Option<OneOrMany<_>>")]
    pub profiles: Option<Vec<String>>,
    #[serde_as(as = "Option<OneOrMany<_>>")]
    pub glob: Option<Vec<String>>,
    pub dir: Option<String>,
    pub run: String,
    #[serde(default)]
    pub exclusive: bool,
    #[serde(default)]
    pub interactive: bool,
    pub depends: Vec<String>,
    pub env: IndexMap<String, String>,
    #[serde_as(as = "Option<OneOrMany<_>>")]
    pub stage: Option<Vec<String>>,
}

impl RunStep {
    pub(crate) fn init(&mut self, name: &str) {
        self.name = name.to_string();
        if self.interactive {
            self.exclusive = true;
        }
    }

    pub fn is_profile_enabled(&self) -> bool {
        is_profile_enabled(
            &self.name,
            self.enabled_profiles(),
            self.disabled_profiles(),
        )
    }

    pub fn enabled_profiles(&self) -> Option<IndexSet<String>> {
        self.profiles.as_ref().map(|profiles| {
            profiles
                .iter()
                .filter(|s| !s.starts_with('!'))
                .map(|s| s.to_string())
                .collect()
        })
    }

    pub fn disabled_profiles(&self) -> Option<IndexSet<String>> {
        self.profiles.as_ref().map(|profiles| {
            profiles
                .iter()
                .filter(|s| s.starts_with('!'))
                .map(|s| s.strip_prefix('!').unwrap().to_string())
                .collect()
        })
    }
}

impl fmt::Display for RunStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[serde_as]
pub struct LinterStep {
    pub r#type: Option<String>,
    #[serde(default)]
    pub name: String,
    #[serde_as(as = "Option<OneOrMany<_>>")]
    pub profiles: Option<Vec<String>>,
    #[serde(default)]
    pub exclusive: bool,
    #[serde(default)]
    pub interactive: bool,
    pub depends: Vec<String>,
    #[serde(default)]
    pub check_first: bool,
    #[serde(default)]
    pub batch: bool,
    #[serde(default)]
    pub stomp: bool,
    #[serde_as(as = "Option<OneOrMany<_>>")]
    pub glob: Option<Vec<String>>,
    pub check: Option<String>,
    pub check_diff: Option<String>,
    pub check_list_files: Option<String>,
    pub fix: Option<String>,
    pub workspace_indicator: Option<String>,
    pub prefix: Option<String>,
    pub dir: Option<String>,
    pub env: IndexMap<String, String>,
    pub root: Option<PathBuf>,
    #[serde_as(as = "Option<OneOrMany<_>>")]
    pub stage: Option<Vec<String>>,
    #[serde(default)]
    pub linter_dependencies: IndexMap<String, Vec<String>>,
}

impl fmt::Display for LinterStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    Text,
    Binary,
    Executable,
    NotExecutable,
    Symlink,
    NotSymlink,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunType {
    Check(CheckType),
    Fix,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckType {
    Check,
    ListFiles,
    Diff,
}

impl LinterStep {
    pub(crate) fn init(&mut self, name: &str) {
        self.name = name.to_string();
        if self.interactive {
            self.exclusive = true;
        }
    }

    pub fn run_cmd(&self, run_type: RunType) -> Option<&str> {
        match run_type {
            RunType::Check(CheckType::Check) => self.check.as_deref(),
            RunType::Check(CheckType::Diff) => self.check_diff.as_deref().or(self.check.as_deref()),
            RunType::Check(CheckType::ListFiles) => {
                self.check_list_files.as_deref().or(self.check.as_deref())
            }
            RunType::Fix => self
                .fix
                .as_deref()
                .or_else(|| self.run_cmd(RunType::Check(CheckType::Check))),
        }
    }

    pub fn available_run_type(&self, run_type: RunType) -> Option<RunType> {
        match (run_type, self.check.is_some(), self.fix.is_some()) {
            (RunType::Check(_), true, _) => Some(RunType::Check(CheckType::Check)),
            (RunType::Fix, _, _) => Some(RunType::Fix),
            (_, false, true) => Some(RunType::Fix),
            _ => None,
        }
    }

    pub fn enabled_profiles(&self) -> Option<IndexSet<String>> {
        self.profiles.as_ref().map(|profiles| {
            profiles
                .iter()
                .filter(|s| !s.starts_with('!'))
                .map(|s| s.to_string())
                .collect()
        })
    }

    pub fn disabled_profiles(&self) -> Option<IndexSet<String>> {
        self.profiles.as_ref().map(|profiles| {
            profiles
                .iter()
                .filter(|s| s.starts_with('!'))
                .map(|s| s.strip_prefix('!').unwrap().to_string())
                .collect()
        })
    }

    pub fn is_profile_enabled(&self) -> bool {
        is_profile_enabled(
            &self.name,
            self.enabled_profiles(),
            self.disabled_profiles(),
        )
    }

    /// For a list of files like this:
    /// src/crate-1/src/lib.rs
    /// src/crate-1/src/subdir/mod.rs
    /// src/crate-2/src/lib.rs
    /// src/crate-2/src/subdir/mod.rs
    /// If the workspace indicator is "Cargo.toml", and there are Cargo.toml files in the root of crate-1 and crate-2,
    /// this will return: ["src/crate-1/Cargo.toml", "src/crate-2/Cargo.toml"]
    pub fn workspaces_for_files(&self, files: &[PathBuf]) -> Result<Option<IndexSet<PathBuf>>> {
        let Some(workspace_indicator) = &self.workspace_indicator else {
            return Ok(None);
        };
        let mut dirs = files.iter().filter_map(|f| f.parent()).collect_vec();
        let mut workspaces: IndexSet<PathBuf> = Default::default();
        while let Some(dir) = dirs.pop() {
            if let Some(workspace) = xx::file::find_up(dir, &[workspace_indicator]) {
                if let Some(dir) = dir.parent() {
                    dirs.retain(|d| !d.starts_with(dir));
                }
                workspaces.insert(workspace);
            }
        }
        Ok(Some(workspaces))
    }
}

fn is_profile_enabled(
    name: &str,
    enabled: Option<IndexSet<String>>,
    disabled: Option<IndexSet<String>>,
) -> bool {
    let settings = Settings::get();
    let enabled_profiles = settings.enabled_profiles();
    if let Some(enabled) = enabled {
        let missing_profiles = enabled.difference(&enabled_profiles).collect::<Vec<_>>();
        if !missing_profiles.is_empty() {
            let missing_profiles = missing_profiles.iter().join(", ");
            debug!("{name}: skipping step due to missing profile: {missing_profiles}");
            return false;
        }
    }
    if let Some(disabled) = disabled {
        let enabled_profiles = settings.enabled_profiles();
        let disabled_profiles = disabled.intersection(&enabled_profiles).collect::<Vec<_>>();
        if !disabled_profiles.is_empty() {
            let disabled_profiles = disabled_profiles.iter().join(", ");
            debug!("{name}: skipping step due to disabled profile: {disabled_profiles}");
            return false;
        }
    }
    true
}

pub(crate) async fn exec_step(
    step: &Steps,
    ctx: &StepContext,
    job: &StepJob,
) -> Result<StepResponse> {
    let mut rsp = StepResponse::default();
    let mut tctx = job.tctx(&ctx.tctx);
    tctx.with_globs(step.glob().unwrap_or(&vec![]));
    tctx.with_files(&job.files);
    let file_msg = |files: &[PathBuf]| {
        format!(
            "{} file{}",
            files.len(),
            if files.len() == 1 { "" } else { "s" }
        )
    };
    let pr = job.build_progress(ctx);
    let Some(mut run) = step.run_cmd(job.run_type).map(|s| s.to_string()) else {
        warn!("{}: no run command", step);
        return Ok(rsp);
    };
    if let Some(prefix) = step.prefix() {
        run = format!("{} {}", prefix, run);
    }
    let files_to_add = if matches!(job.run_type, RunType::Fix) {
        if let Some(stage) = step.stage() {
            let stage = stage
                .iter()
                .map(|s| tera::render(s, &tctx).unwrap())
                .collect_vec();
            glob::get_matches(&stage, &job.files)?
        } else if step.glob().is_some() {
            job.files.clone()
        } else {
            vec![]
        }
        .into_iter()
        .map(|p| {
            (
                p.metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                p,
            )
        })
        .collect_vec()
    } else {
        vec![]
    };
    let run = tera::render(&run, &tctx).unwrap();
    pr.prop(
        "message",
        &format!(
            "{} – {} – {}",
            file_msg(&job.files),
            step.glob().unwrap_or(&vec![]).join(" "),
            run
        ),
    );
    pr.update();
    if log::log_enabled!(log::Level::Trace) {
        for file in &job.files {
            trace!("{step}: {}", file.display());
        }
    }
    let mut cmd = CmdLineRunner::new("sh")
        .arg("-o")
        .arg("errexit")
        .arg("-c")
        .arg(&run)
        .with_pr(pr.clone())
        .show_stderr_on_error(false);
    if step.interactive() {
        clx::progress::pause();
        cmd = cmd
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
    }
    if let Some(dir) = step.dir() {
        cmd = cmd.current_dir(dir);
    }
    for (key, value) in step.env() {
        cmd = cmd.env(key, value);
    }
    match cmd.execute().await {
        Ok(_) => {}
        Err(err) => {
            if step.interactive() {
                clx::progress::resume();
            }
            if let ensembler::Error::ScriptFailed(_bin, _args, _output, result) = &err {
                if let RunType::Check(CheckType::ListFiles) = job.run_type {
                    let stdout = result.stdout.clone();
                    return Err(Error::CheckListFailed {
                        source: eyre!("{}", err),
                        stdout,
                    })?;
                }
            }
            ctx.progress.set_status(ProgressStatus::Failed);
            return Err(err).wrap_err(run);
        }
    }
    if step.interactive() {
        clx::progress::resume();
    }
    rsp.files_to_add = files_to_add
        .into_iter()
        .filter(|(prev_mod, p)| {
            if !p.exists() {
                return false;
            }
            let Ok(metadata) = p.metadata().and_then(|m| m.modified()) else {
                return false;
            };
            metadata > *prev_mod
        })
        .map(|(_, p)| p)
        .collect_vec();
    ctx.inc_files_added(rsp.files_to_add.len());
    ctx.decrement_job_count();
    ctx.update_progress();
    pr.set_status(ProgressStatus::Done);
    Ok(rsp)
}
