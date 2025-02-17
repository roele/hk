#!/usr/bin/env bats

setup() {
    load 'test_helper/bats-support/load'
    load 'test_helper/bats-assert/load'
    load 'test_helper/bats-file/load'

    export PROJECT_ROOT="$BATS_TEST_DIRNAME/.."
    export PKL_PATH="$PROJECT_ROOT/pkl"

    # Create a temporary directory for each test
    TEST_TEMP_DIR="$(temp_make)"
    mkdir -p "$TEST_TEMP_DIR/src/proj"
    cd "$TEST_TEMP_DIR/src/proj"

    # Initialize a git repository
    export GIT_CONFIG_NOSYSTEM=1
    export HOME="$TEST_TEMP_DIR"
    git config --global init.defaultBranch main
    git config --global user.email "test@example.com"
    git config --global user.name "Test User"
    git init .

    # Add hk to PATH (assuming it's installed)
    PATH="$(dirname $BATS_TEST_DIRNAME)/target/debug:$PATH"
}

teardown() {
    chmod -R u+w "$TEST_TEMP_DIR"
    temp_del "$TEST_TEMP_DIR"
}

@test "hk --version prints version" {
    run hk --version
    assert_output --regexp "^hk\ [0-9]+\.[0-9]+\.[0-9]+$"
}

@test "hk generate creates hk.pkl" {
    hk g
    assert_file_contains hk.pkl "min_hk_version"
}

@test "hk install creates git hooks" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["prettier"] = new builtins.Prettier {}
}
EOF
    hk install
    assert_file_exists ".git/hooks/pre-commit"
}

@test "git runs pre-commit on staged files" {
    cat <<EOF > test.js
console.log("test")
EOF
    run git add test.js
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["prettier"] = new builtins.Prettier {}
}
EOF
    hk install
    run cat test.js
    assert_output 'console.log("test")'
    run git commit -m "test"
    run cat test.js
    assert_output 'console.log("test");'
}

@test "hk run pre-commit --all runs on all files" {
    cat <<EOF > test.js
console.log("test")
EOF
    git add test.js
    git commit -m init
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["prettier"] = new builtins.Prettier {}
}
EOF
    hk run pre-commit -a
    run cat test.js
    assert_output 'console.log("test");'
}

@test "builtin: json" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["json"] = new builtins.Jq {}
}
EOF
    cat <<EOF > test.json
{ "invalid": 
EOF
    git add test.json
    run hk run pre-commit
    assert_failure
    assert_output --partial "jq: parse error"
}

@test "builtin: json format" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["jq"] = new builtins.Jq {}
}
EOF
    cat <<EOF > test.json
{"test": 123}
EOF
    git add test.json
    hk run pre-commit
    assert_file_contains test.json '{
  "test": 123
}'
}

@test "builtin: yaml" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["yq"] = new builtins.Yq {}
}
EOF
    cat <<EOF > test.yaml
test: :
EOF
    git add test.yaml
    run hk run pre-commit
    assert_failure
    assert_output --partial "yaml: mapping values are not allowed"
}

@test "builtin: yaml format" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["yq"] = new builtins.Yq {}
}
EOF
    cat <<EOF > test.yaml
    test: 123
EOF
    git add test.yaml
    cat test.yaml
    hk run pre-commit
    assert_file_contains test.yaml 'test: 123'
}

@test "builtin: shellcheck" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["shellcheck"] = new builtins.Shellcheck {}
}
EOF
    cat <<EOF > test.sh
#!/bin/bash
cat \$1
EOF
    git add test.sh
    run hk run pre-commit
    assert_failure
    assert_output --partial "SC2086"
}

@test "HK_SKIP_STEPS skips specified steps" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["prettier"] = new builtins.Prettier {}
    ["shellcheck"] = new builtins.Shellcheck {}
}
EOF
    touch test.sh
    touch test.js
    git add test.sh test.js
    export HK_SKIP_STEPS="shellcheck"
    run hk run pre-commit -v
    assert_success
    assert_output --partial "prettier"
    assert_output --partial "shellcheck: skipping step due to HK_SKIP_STEPS"
}

@test "HK_SKIP_HOOK skips entire hooks" {
    cat <<EOF > hk.pkl
amends "$PKL_PATH/hk.pkl"
import "$PKL_PATH/builtins.pkl"

\`pre-commit\` {
    ["prettier"] = new builtins.Prettier {}
    ["shellcheck"] = new builtins.Shellcheck {}
}
EOF
    touch test.sh
    touch test.js
    git add test.sh test.js
    export HK_SKIP_HOOK="pre-commit"
    run hk run pre-commit -v
    assert_success
    assert_output --partial "pre-commit: skipping hook due to HK_SKIP_HOOK"
}
