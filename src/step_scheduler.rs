use crate::{
    env,
    error::Error,
    step_depends::StepDepends,
    step_job::{StepJob, StepJobStatus},
    step_locks::StepLocks,
    step_queue::StepQueueBuilder,
    step_response::StepResponse,
    tera::Context,
    ui::style,
};
use clx::progress::{self, ProgressJob, ProgressJobBuilder, ProgressOutput};
use indexmap::{IndexMap, IndexSet};
use itertools::Itertools;
use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::task::JoinSet;

use crate::{Result, step_context::StepContext};
use crate::{
    config::Config,
    git::Git,
    settings::Settings,
    step::{CheckType, RunType, Step},
};

pub struct StepScheduler<'a> {
    run_type: RunType,
    repo: &'a Git,
    steps: Vec<Arc<Step>>,
    files: Vec<PathBuf>,
    tctx: Context,
    failed: Arc<Mutex<bool>>,
    semaphore: Arc<Semaphore>,
}

impl<'a> StepScheduler<'a> {
    pub fn new(hook: &IndexMap<String, Step>, run_type: RunType, repo: &'a Git) -> Self {
        let settings = Settings::get();
        let config = Config::get().unwrap();
        Self {
            run_type,
            repo,
            steps: hook
                .values()
                .flat_map(|s| match &s.r#type {
                    Some(r#type) if r#type == "check" || r#type == "fix" => config
                        .linters
                        .values()
                        .cloned()
                        .map(|linter| linter.into())
                        .collect(),
                    _ => vec![s.clone()],
                })
                .map(Arc::new)
                .collect(),
            files: vec![],
            tctx: Default::default(),
            failed: Arc::new(Mutex::new(false)),
            semaphore: Arc::new(Semaphore::new(settings.jobs().get())),
        }
    }

    pub fn with_files(mut self, files: Vec<PathBuf>) -> Self {
        self.files = files;
        self
    }

    pub fn with_linters(mut self, linters: &[String]) -> Self {
        if linters.is_empty() {
            return self;
        }
        self.steps = self
            .steps
            .iter()
            .filter(|s| linters.contains(&s.name))
            .cloned()
            .collect();
        self
    }

    pub fn with_tctx(mut self, tctx: Context) -> Self {
        self.tctx = tctx;
        self
    }

    pub async fn run(self) -> Result<()> {
        let settings = Settings::get();
        let jobs = settings.jobs().get();
        let file_locks = self
            .files
            .iter()
            .map(|file| (file.clone(), Arc::new(RwLock::new(()))))
            .collect::<IndexMap<PathBuf, _>>();
        let queue = StepQueueBuilder::new(self.steps, self.files, self.run_type).build()?;
        let total_jobs = queue.groups.iter().flatten().count();
        let mut remaining_jobs = total_jobs;

        for (i, group) in queue.groups.iter().enumerate() {
            let mut set = JoinSet::new();
            let step_contexts: HashMap<String, Arc<StepContext>> = group
                .iter()
                .map(|job| job.step.clone())
                .unique_by(|step| step.name.clone())
                .map(|step| {
                    let jobs_total = group
                        .iter()
                        .filter(|job| job.step.name == step.name)
                        .count();
                    (
                        step.name.clone(),
                        Arc::new(StepContext {
                            semaphore: self.semaphore.clone(),
                            failed: self.failed.clone(),
                            file_locks: file_locks.clone(),
                            tctx: self.tctx.clone(),
                            depends: Arc::new(StepDepends::new(group)),
                            progress: step.build_step_progress(),
                            files_added: Arc::new(std::sync::Mutex::new(0)),
                            jobs_total,
                            jobs_remaining: Arc::new(std::sync::Mutex::new(jobs_total)),
                        }),
                    )
                })
                .collect();

            let mut future_group_progress_jobs = if progress::output() == ProgressOutput::Text {
                vec![]
            } else {
                queue
                    .groups
                    .iter()
                    .skip(i + 1)
                    .map(|group| {
                        ProgressJobBuilder::new()
                            .status(progress::ProgressStatus::RunningCustom(
                                style::eyellow("❯").dim().to_string(),
                            ))
                            .prop(
                                "message",
                                &group
                                    .iter()
                                    .map(|j| j.step.name.clone())
                                    .unique()
                                    .join(", "),
                            )
                            .start()
                    })
                    .collect_vec()
            };

            let total_progress = if progress::output() == ProgressOutput::Text || total_jobs <= jobs
            {
                None
            } else {
                Some(
                    ProgressJobBuilder::new()
                        .progress_total(total_jobs)
                        .progress_current(total_jobs - remaining_jobs)
                        .body(vec!["{{progress_bar()}}".to_string()])
                        .start(),
                )
            };

            for job in group {
                StepScheduler::run_step(
                    step_contexts.get(&job.step.name).unwrap().clone(),
                    job.clone(),
                    &mut set,
                )
                .await?;
            }

            // Wait for tasks and abort on first error
            let mut files_to_stage = IndexSet::new();
            let abort = |set: &mut JoinSet<Result<StepResponse>>,
                         future_group_progress_jobs: &mut Vec<Arc<ProgressJob>>,
                         e: eyre::Error| {
                set.abort_all();
                for job in future_group_progress_jobs.iter_mut() {
                    job.remove();
                }
                for p in step_contexts.values().map(|ctx| &ctx.progress) {
                    if p.is_running() {
                        p.set_status(clx::progress::ProgressStatus::DoneCustom(
                            style::eyellow("▲").to_string(),
                        ));
                    }
                    for child in p.children() {
                        if child.is_running() {
                            child.set_status(clx::progress::ProgressStatus::DoneCustom(
                                style::eyellow("▲").to_string(),
                            ));
                        }
                    }
                }
                progress::flush();
                if !log::log_enabled!(log::Level::Debug) {
                    if let Some(ensembler::Error::ScriptFailed(bin, args, output, result)) =
                        e.chain().find_map(|e| e.downcast_ref::<ensembler::Error>())
                    {
                        let mut cmd = format!("{} {}", bin, args.join(" "));
                        if cmd.starts_with("sh -o errexit -c ") {
                            cmd = cmd[24..].to_string();
                        }
                        eprintln!("{}\n{output}", style::ered(format!("Error running {cmd}")));
                        if let Err(e) = write_output_file(result) {
                            eprintln!("Error writing output file: {e:?}");
                        }
                        std::process::exit(1);
                    }
                }
                Err(e)
            };
            while let Some(result) = set.join_next().await {
                remaining_jobs -= 1;
                if let Some(total_progress) = &total_progress {
                    total_progress.progress_current(total_jobs - remaining_jobs);
                }
                match result {
                    Ok(Ok(ctx)) => {
                        files_to_stage.extend(ctx.files_to_add);
                    }
                    Ok(Err(e)) => {
                        // Task failed to execute
                        return abort(&mut set, &mut future_group_progress_jobs, e);
                    }
                    Err(e) => {
                        // JoinError occurred
                        return abort(&mut set, &mut future_group_progress_jobs, e.into());
                    }
                }
            }
            for job in future_group_progress_jobs.iter_mut() {
                job.remove();
            }
            if let Some(total_progress) = &total_progress {
                total_progress.remove();
            }

            if !files_to_stage.is_empty() {
                trace!("staging files: {:?}", &files_to_stage);
                self.repo.add(
                    &files_to_stage
                        .iter()
                        .map(|f| f.to_str().unwrap())
                        .collect_vec(),
                )?;
            }
        }
        Ok(())
    }

    async fn run_step(
        ctx: Arc<StepContext>,
        mut job: StepJob,
        set: &mut JoinSet<Result<StepResponse>>,
    ) -> Result<()> {
        let step = job.step.clone();

        trace!("{step}: spawning step");
        set.spawn(async move {
            if job.check_first {
                let mut check_job = job.clone();
                check_job.run_type = match job.run_type {
                    RunType::Fix if step.check_diff.is_some() => RunType::Check(CheckType::Diff),
                    RunType::Fix if step.check_list_files.is_some() => {
                        RunType::Check(CheckType::ListFiles)
                    }
                    RunType::Fix => RunType::Check(CheckType::Check),
                    _ => unreachable!(),
                };
                debug!("{step}: running check step first due to fix step contention");
                match run(
                    &ctx,
                    check_job,
                )
                .await
                {
                    Ok(rsp) => {
                        debug!("{step}: successfully ran check step first");
                        ctx.depends.job_done(&step.name);
                        return Ok(rsp);
                    }
                    Err(e) => {
                        if let Some(Error::CheckListFailed { source, stdout }) =
                            e.downcast_ref::<Error>()
                        {
                            debug!("{step}: failed check step first: {source}");
                            let filtered_files: HashSet<PathBuf> =
                                stdout.lines().map(PathBuf::from).collect();
                            let files: IndexSet<PathBuf> = job.files.into_iter().filter(|f| filtered_files.contains(f)).collect();
                            for f in filtered_files.into_iter().filter(|f| !files.contains(f)) {
                                warn!("{step}: file in check_list_files not found in original files: {}", f.display());
                            }
                            job.files = files.into_iter().collect();
                        }
                        debug!("{step}: failed check step first: {e}");
                    }
                }
            }
            let failed = ctx.failed.clone();
            let result = match run(
                &ctx,
                job,
            )
            .await
            {
                Ok(rsp) => Ok(rsp),
                Err(err) => {
                    // Mark as failed to prevent new steps from starting
                    *failed.lock().await = true;
                    Err(err)
                }
            };
            ctx.depends.job_done(&step.name);
            result
        });
        Ok(())
    }
}

fn write_output_file(result: &ensembler::CmdResult) -> Result<()> {
    let path = env::HK_STATE_DIR.join("output.log");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let output = console::strip_ansi_codes(&result.combined_output);
    std::fs::write(&path, output.to_string())?;
    eprintln!("\nSee {} for full command output", path.display());
    Ok(())
}

async fn run(ctx: &StepContext, mut job: StepJob) -> Result<StepResponse> {
    let step = job.step.clone();
    match job.status {
        StepJobStatus::Pending => {
            let locks = StepLocks::lock(ctx, &job).await?;
            job.status = StepJobStatus::Started(locks);
        }
        // StepJobStatus::Ready(locks) => {
        //     job.status = StepJobStatus::Started(locks);
        // }
        status => unreachable!("invalid status: {}", status),
    }
    if *ctx.failed.lock().await {
        trace!("{step}: skipping step due to previous failure");
        return Ok(Default::default());
    }
    match step.run(ctx, &job).await {
        Ok(rsp) => {
            trace!("{step}: successfully ran step");
            Ok(rsp)
        }
        Err(err) => {
            trace!("{step}: failed to run step: {err}");
            Err(err.wrap_err(step.name.clone()))
        }
    }
}
