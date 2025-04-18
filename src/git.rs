use std::{
    cell::OnceCell,
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::Result;
use clx::progress::{ProgressJob, ProgressJobBuilder, ProgressStatus};
use eyre::{WrapErr, eyre};
use git2::{Oid, Repository, StatusOptions, StatusShow, Tree};
use itertools::{Either, Itertools};
use xx::file::display_path;

use crate::env;

pub struct Git {
    repo: Repository,
    stash: Option<Either<String, Oid>>,
    root: PathBuf,
    patch_file: OnceCell<PathBuf>,
}

impl Git {
    pub fn new() -> Result<Self> {
        let cwd = std::env::current_dir()?;
        let root = xx::file::find_up(&cwd, &[".git"])
            .and_then(|p| p.parent().map(|p| p.to_path_buf()))
            .ok_or(eyre!("failed to find git repository"))?;
        std::env::set_current_dir(root)?;
        let repo = Repository::open(".").wrap_err("failed to open repository")?;
        let root = repo.workdir().unwrap().to_path_buf();
        Ok(Self {
            root: root.clone(),
            repo,
            stash: None,
            patch_file: OnceCell::new(),
        })
    }

    pub fn patch_file(&self) -> &Path {
        self.patch_file.get_or_init(|| {
            let name = self
                .root
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_str()
                .unwrap();
            let rand = getrandom::u32()
                .unwrap_or_default()
                .to_string()
                .chars()
                .take(8)
                .collect::<String>();
            env::HK_STATE_DIR
                .join("patches")
                .join(format!("{name}-{rand}.patch"))
        })
    }

    pub fn matching_remote_branch(&self, remote: &str) -> Result<Option<String>> {
        if let Some(branch) = self.current_branch()? {
            if let Ok(_ref) = self
                .repo
                .find_reference(&format!("refs/remotes/{remote}/{branch}"))
            {
                return Ok(_ref.name().map(|s| s.to_string()));
            }
        }
        Ok(None)
    }

    pub fn current_branch(&self) -> Result<Option<String>> {
        let head = self.repo.head().wrap_err("failed to get head")?;
        let branch_name = head.shorthand().map(|s| s.to_string());
        Ok(branch_name)
    }

    fn head_tree(&self) -> Result<Tree<'_>> {
        let head = self.repo.head().wrap_err("failed to get head")?;
        let head = head
            .peel_to_tree()
            .wrap_err("failed to peel head to tree")?;
        Ok(head)
    }

    // fn head_commit(&self) -> Result<Commit<'_>> {
    //     let head = self.repo.head().wrap_err("failed to get head")?;
    //     let commit = head
    //         .peel_to_commit()
    //         .wrap_err("failed to peel head to commit")?;
    //     Ok(commit)
    // }

    // fn head_commit_message(&self) -> Result<String> {
    //     let commit = self.head_commit()?;
    //     let message = commit
    //         .message()
    //         .ok_or(eyre!("failed to get commit message"))?;
    //     Ok(message.to_string())
    // }

    pub fn all_files(&self) -> Result<Vec<PathBuf>> {
        let head = self.head_tree()?;
        let mut files = Vec::new();
        head.walk(git2::TreeWalkMode::PreOrder, |root, entry| {
            if let Some(name) = entry.name() {
                let path = if root.is_empty() {
                    PathBuf::from(name)
                } else {
                    PathBuf::from(root).join(name)
                };
                if path.exists() {
                    files.push(path);
                }
            }
            git2::TreeWalkResult::Ok
        })
        .wrap_err("failed to walk tree")?;
        Ok(files)
    }

    // pub fn intent_to_add_files(&self) -> Result<Vec<PathBuf>> {
    //     // let added_files = self.added_files()?;
    //     // TODO: get this to work, should be the equivalent of `git diff --name-only --diff-filter=A`
    //     Ok(vec![])
    // }

    pub fn modified_files(&self) -> Result<Vec<PathBuf>> {
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true);
        let diff = self
            .repo
            .diff_tree_to_workdir_with_index(None, Some(&mut opts))
            .wrap_err("failed to get diff")?;
        let mut files = BTreeSet::new();
        diff.foreach(
            &mut |delta, _| {
                if delta.status() == git2::Delta::Deleted {
                    return true;
                }
                if let Some(path) = delta.new_file().path() {
                    files.insert(PathBuf::from(path));
                }
                true
            },
            None,
            None,
            None,
        )
        .wrap_err("failed to process diff")?;
        Ok(files.into_iter().collect())
    }

    pub fn staged_files(&self) -> Result<Vec<PathBuf>> {
        let mut status_options = StatusOptions::new();
        status_options.show(StatusShow::Index);
        let statuses = self
            .repo
            .statuses(Some(&mut status_options))
            .wrap_err("failed to get statuses")?;
        let paths = statuses
            .iter()
            .filter_map(|s| s.path().map(PathBuf::from))
            .filter(|p| p.exists())
            .collect_vec();
        Ok(paths)
    }

    pub fn unstaged_files(&self) -> Result<Vec<PathBuf>> {
        let mut status_options = StatusOptions::new();
        status_options
            .include_untracked(true)
            .show(StatusShow::Workdir);
        let statuses = self
            .repo
            .statuses(Some(&mut status_options))
            .wrap_err("failed to get statuses")?;
        let paths = statuses
            .iter()
            .filter_map(|s| s.path().map(PathBuf::from))
            .collect_vec();
        Ok(paths)
    }

    pub fn stash_unstaged(&mut self, force: bool) -> Result<()> {
        // Skip stashing if there's no initial commit yet or auto-stash is disabled
        if (!force && !*env::HK_STASH) || self.repo.head().is_err() {
            return Ok(());
        }
        let job = ProgressJobBuilder::new()
            .body(vec!["{{spinner()}} stash – {{message}}{% if files is defined %} ({{files}} file{{files|pluralize}}){% endif %}".to_string()])
            .prop("message", "Fetching unstaged files")
            .start();

        let unstaged_files = self.unstaged_files()?;
        job.prop("files", &unstaged_files.len());
        // TODO: if any intent_to_add files exist, run `git rm --cached -- <file>...` then `git add --intent-to-add -- <file>...` when unstashing
        // let intent_to_add = self.intent_to_add_files()?;
        // see https://github.com/pre-commit/pre-commit/blob/main/pre_commit/staged_files_only.py
        if unstaged_files.is_empty() {
            job.prop("message", "No unstaged changes to stash");
            job.set_status(ProgressStatus::Done);
            return Ok(());
        }

        // if let Ok(msg) = self.head_commit_message() {
        //     if msg.contains("Merge") {
        //         return Ok(());
        //     }
        // }
        self.stash = if *env::HK_STASH_USE_GIT {
            job.prop("message", "Running git stash");
            job.update();
            self.push_stash()?.map(Either::Right)
        } else {
            job.prop(
                "message",
                &format!(
                    "Creating git diff patch – {}",
                    display_path(self.patch_file())
                ),
            );
            job.update();
            self.build_diff()?.map(Either::Left)
        };
        if self.stash.is_none() {
            job.prop("message", "No unstaged files to stash");
            job.set_status(ProgressStatus::Done);
            return Ok(());
        }

        job.prop("message", "Removing unstaged changes");
        job.update();

        let mut checkout_opts = git2::build::CheckoutBuilder::new();
        checkout_opts.allow_conflicts(true);
        checkout_opts.remove_untracked(true);
        checkout_opts.force();
        checkout_opts.update_index(false);
        self.repo
            .checkout_index(None, Some(&mut checkout_opts))
            .wrap_err("failed to reset to head")?;

        job.prop("message", "Stashed unstaged changes");
        job.set_status(ProgressStatus::Done);
        // return Err(eyre!("failed to reset to head"));
        Ok(())
    }

    fn build_diff(&self) -> Result<Option<String>> {
        debug!("building diff for stash");
        // essentially: git diff-index --ignore-submodules --binary --exit-code --no-color --no-ext-diff (git write-tree)
        let mut opts = git2::DiffOptions::new();
        opts.include_untracked(true);
        opts.show_binary(true);
        opts.show_untracked_content(true);
        let diff = self
            .repo
            .diff_index_to_workdir(None, Some(&mut opts))
            .wrap_err("failed to get diff")?;
        let mut diff_bytes = vec![];
        diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
            match line.origin() {
                '+' | '-' | ' ' => diff_bytes.push(line.origin() as u8),
                _ => {}
            };
            diff_bytes.extend(line.content());
            true
        })
        .wrap_err("failed to print diff")?;
        let mut idx = self.repo.index()?;
        // if we can't write the index or there's no diff, don't stash
        if idx.write().is_err() || diff_bytes.is_empty() {
            Ok(None)
        } else {
            let patch = String::from_utf8_lossy(&diff_bytes).to_string();
            let patch_file = self.patch_file();
            if let Err(err) = xx::file::write(patch_file, &patch) {
                warn!("failed to write patch file: {:?}", err);
            }
            Ok(Some(patch))
        }
    }

    pub fn push_stash(&mut self) -> Result<Option<Oid>> {
        if self.unstaged_files()?.is_empty() {
            return Ok(None);
        }
        let sig = self.repo.signature()?;
        let mut flags = git2::StashFlags::default();
        flags.set(git2::StashFlags::INCLUDE_UNTRACKED, true);
        flags.set(git2::StashFlags::KEEP_INDEX, false);
        let oid = self
            .repo
            .stash_save2(&sig, None, Some(flags))
            .wrap_err("failed to stash")?;
        Ok(Some(oid))
    }

    pub fn pop_stash(&mut self) -> Result<()> {
        let Some(diff) = self.stash.take() else {
            return Ok(());
        };
        let job: Arc<ProgressJob>;

        match diff {
            Either::Left(diff) => {
                job = ProgressJobBuilder::new()
                    .prop(
                        "message",
                        &format!(
                            "stash – Applying git diff patch – {}",
                            display_path(self.patch_file())
                        ),
                    )
                    .start();
                let diff = git2::Diff::from_buffer(diff.as_bytes())?;
                let mut apply_opts = git2::ApplyOptions::new();
                self.repo
                    .apply(&diff, git2::ApplyLocation::WorkDir, Some(&mut apply_opts))
                    .wrap_err_with(|| {
                        format!("failed to apply diff {}", display_path(self.patch_file()))
                    })?;
            }
            Either::Right(_oid) => {
                job = ProgressJobBuilder::new()
                    .prop("message", "stash – Applying git stash")
                    .start();
                let mut opts = git2::StashApplyOptions::new();
                let mut checkout_opts = git2::build::CheckoutBuilder::new();
                checkout_opts.allow_conflicts(true);
                checkout_opts.force();
                opts.checkout_options(checkout_opts);
                opts.reinstantiate_index();
                self.repo
                    .stash_pop(0, Some(&mut opts))
                    .wrap_err("failed to reset to stash")?;
            }
        }
        job.set_status(ProgressStatus::Done);
        Ok(())
    }

    pub fn add(&self, pathspecs: &[&str]) -> Result<()> {
        let pathspecs = pathspecs
            .iter()
            .map(|p| p.replace(self.root.to_str().unwrap(), ""))
            .collect_vec();
        trace!("adding files: {:?}", &pathspecs);
        let mut index = self.repo.index().wrap_err("failed to get index")?;
        index
            .add_all(&pathspecs, git2::IndexAddOption::DEFAULT, None)
            .wrap_err("failed to add files to index")?;
        index.write().wrap_err("failed to write index")?;
        Ok(())
    }

    pub fn files_between_refs(&self, from_ref: &str, to_ref: &str) -> Result<Vec<PathBuf>> {
        let from_obj = self
            .repo
            .revparse_single(from_ref)
            .wrap_err(format!("Failed to parse reference: {}", from_ref))?;
        let to_obj = self
            .repo
            .revparse_single(to_ref)
            .wrap_err(format!("Failed to parse reference: {}", to_ref))?;

        let from_tree = from_obj
            .peel_to_tree()
            .wrap_err(format!("Failed to get tree for reference: {}", from_ref))?;
        let to_tree = to_obj
            .peel_to_tree()
            .wrap_err(format!("Failed to get tree for reference: {}", to_ref))?;

        let diff = self
            .repo
            .diff_tree_to_tree(Some(&from_tree), Some(&to_tree), None)
            .wrap_err("Failed to get diff between references")?;

        let mut files = BTreeSet::new();
        diff.foreach(
            &mut |_, _| true,
            None,
            None,
            Some(&mut |diff_delta, _, _| {
                if let Some(path) = diff_delta.new_file().path() {
                    let path_buf = PathBuf::from(path);
                    if path_buf.exists() {
                        files.insert(path_buf);
                    }
                }
                true
            }),
        )
        .wrap_err("Failed to process diff")?;

        Ok(files.into_iter().collect())
    }
}
