# `hk`

**Usage**: `hk [FLAGS] <SUBCOMMAND>`

**Version**: 0.7.0

- **Usage**: `hk [FLAGS] <SUBCOMMAND>`

## Global Flags

### `-j --jobs <JOBS>`

Number of jobs to run in parallel

### `-p --profile... <PROFILE>`

Profiles to enable/disable prefix with ! to disable e.g. --profile slow --profile !fast

### `-s --slow`

Shorthand for --profile=slow

### `-v --verbose...`

Enables verbose output

### `-q --quiet`

Suppresses output

### `--silent`

Suppresses all output

## Subcommands

- [`hk cache clear`](/cli/cache/clear.md)
- [`hk check [FLAGS]`](/cli/check.md)
- [`hk completion <SHELL>`](/cli/completion.md)
- [`hk config`](/cli/config.md)
- [`hk fix [FLAGS]`](/cli/fix.md)
- [`hk init [-f --force] [--mise]`](/cli/init.md)
- [`hk install [--mise]`](/cli/install.md)
- [`hk run <SUBCOMMAND>`](/cli/run.md)
- [`hk run commit-msg <COMMIT_MSG_FILE>`](/cli/run/commit-msg.md)
- [`hk run pre-commit [FLAGS]`](/cli/run/pre-commit.md)
- [`hk run pre-push [FLAGS] [REMOTE] [URL]`](/cli/run/pre-push.md)
- [`hk run prepare-commit-msg <ARGS>…`](/cli/run/prepare-commit-msg.md)
- [`hk version`](/cli/version.md)
