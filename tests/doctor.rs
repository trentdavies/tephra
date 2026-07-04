//! Integration tests for `tephra doctor` (Task 10).
//!
//! Uses `tests/common::Fixture`'s bare-remote + bridge + agent(work) trio
//! (see that module's doc comment) since, unlike `init`, `doctor` genuinely
//! shells out to git against a real bridge checkout.

mod common;

use common::Fixture;
use predicates::prelude::PredicateBooleanExt;

fn doctor_cmd(fx: &Fixture) -> assert_cmd::Command {
    let mut cmd = fx.tephra_cmd();
    cmd.arg("doctor").arg(&fx.name);
    cmd
}

fn git_ok(fx: &Fixture, dir: &std::path::Path, args: &[&str]) {
    let out = fx.git(dir, args);
    assert!(
        out.status.success(),
        "git -C {} {} failed: {}",
        dir.display(),
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn healthy_fixture_all_ok_and_exits_zero() {
    let fx = Fixture::new("testvault");

    doctor_cmd(&fx)
        .assert()
        .success()
        .stdout(predicates::str::contains("FAIL").not())
        .stdout(predicates::str::contains("ok: git"))
        .stdout(predicates::str::contains(
            "ok: bridge exists and is a git repository",
        ))
        .stdout(predicates::str::contains("ok: git identity resolves"))
        .stdout(predicates::str::contains("ok: upstream configured"))
        .stdout(predicates::str::contains("ok: remote 'origin' reachable"))
        .stdout(predicates::str::contains("ok: work clone exists"));
}

#[test]
fn missing_bridge_fails_with_clone_remediation_and_skips_the_rest() {
    let fx = Fixture::new("testvault");
    std::fs::remove_dir_all(&fx.bridge).unwrap();

    doctor_cmd(&fx)
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains(
            "FAIL: bridge directory does not exist",
        ))
        .stdout(predicates::str::contains(format!(
            "clone it: git clone {} {}",
            fx.remote.display(),
            fx.bridge.display()
        )))
        .stdout(predicates::str::contains(
            "warn: skipping remaining bridge checks",
        ))
        .stdout(predicates::str::contains("git identity").not());
}

#[test]
fn missing_upstream_and_no_remote_fails_naming_set_upstream_to() {
    let fx = Fixture::new("testvault");
    git_ok(&fx, &fx.bridge, &["branch", "--unset-upstream", "main"]);
    git_ok(&fx, &fx.bridge, &["remote", "remove", "origin"]);

    doctor_cmd(&fx)
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains(
            "FAIL: branch 'main' has no upstream and",
        ))
        .stdout(predicates::str::contains("has no remotes configured"))
        .stdout(predicates::str::contains("branch --set-upstream-to"));
}

#[test]
fn unreachable_remote_fails_the_reachability_check() {
    let fx = Fixture::new("testvault");
    // Move the bare remote away: `origin`'s configured URL (a local path in
    // this fixture) no longer resolves to anything, simulating an
    // unreachable remote without needing real network access.
    let moved = fx.root.path().join("remote-moved-away.git");
    std::fs::rename(&fx.remote, &moved).unwrap();

    doctor_cmd(&fx)
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("ok: upstream configured"))
        .stdout(predicates::str::contains(
            "FAIL: remote 'origin' unreachable",
        ));
}

#[test]
fn missing_work_clone_warns_but_still_exits_zero() {
    let fx = Fixture::new("testvault");
    std::fs::remove_dir_all(&fx.agent).unwrap();

    doctor_cmd(&fx)
        .assert()
        .success()
        .stdout(predicates::str::contains("warn: work clone missing"))
        .stdout(predicates::str::contains(format!(
            "tephra clone {}",
            fx.name
        )))
        .stdout(predicates::str::contains("FAIL").not());
}

/// Reproduces the "useConfigOnly gap" that bit the prototype's machine: a
/// bridge directory not covered by any `includeIf`, with
/// `user.useConfigOnly = true` blocking git's gecos/hostname-guessing
/// fallback, and no identity available from the environment either. The
/// fixture normally disables `user.useConfigOnly` and supplies identity via
/// `GIT_AUTHOR_*`/`GIT_COMMITTER_*` env vars (see `tests/common`) -- this is
/// the one test that deliberately overrides both, and also points
/// `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` at `/dev/null` so the assertion
/// can't pass by accident just because the host happens to have a global
/// `~/.gitconfig` identity configured.
#[test]
fn identity_fails_when_useconfigonly_blocks_resolution_and_env_is_cleared() {
    let fx = Fixture::new("testvault");
    let mut cmd = fx.tephra_cmd();
    cmd.env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "user.useConfigOnly")
        .env("GIT_CONFIG_VALUE_0", "true")
        .env_remove("GIT_CONFIG_KEY_1")
        .env_remove("GIT_CONFIG_VALUE_1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .arg("doctor")
        .arg(&fx.name);

    cmd.assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains(
            "FAIL: git identity does not resolve",
        ))
        .stdout(predicates::str::contains("includeIf"));
}
