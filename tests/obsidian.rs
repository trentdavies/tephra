//! Integration tests for `tephra obsidian doctor` and
//! `tephra obsidian service install|uninstall` (Task 9).
//!
//! `doctor`'s checks all shell out to `ob`, so every test here first builds
//! a temp directory containing a FAKE, executable `ob` shim script and puts
//! ONLY that directory on the child process's `PATH` (never appending the
//! real `PATH`) before invoking the compiled `tephra` binary. No test in
//! this file ever invokes the real, in-use `ob` on this development
//! machine -- its real, live Obsidian Sync login/vault binding must never
//! be touched by a test run.
//!
//! Golden-file tests for `obsidian service install`'s generated
//! launchd/systemd units mirror `tests/service.rs`'s pattern exactly: pure
//! generator functions, fixed fake paths, byte-for-byte comparison against
//! `tests/golden/obsidian-*`. Runtime `service install`/`uninstall` against
//! the REAL launchctl/systemd is intentionally NOT exercised here (unlike
//! `tests/service.rs`'s `TEPHRA_TEST_SERVICES`-gated real-launchctl cycle
//! test): a live `KeepAlive`/`Restart=always` `ob sync --continuous` process
//! would fight this machine's own already-running Obsidian Sync setup for
//! the same bound vault (`~/dev/memory/bridge-personal`). Golden-file
//! coverage here plus the pure-function unit tests in `src/obsidian.rs`'s
//! own `#[cfg(test)]` module (path/label formatting, `which_ob_from`,
//! `resolve_node_pin`) are the full extent of this task's install/uninstall
//! verification, per the plan's note that this is golden + unit only.

mod common;

use std::path::{Path, PathBuf};

use common::Fixture;
use predicates::prelude::PredicateBooleanExt;

// --------------------------------------------------------------------
// golden-file tests: `obsidian service install`'s unit generation
// --------------------------------------------------------------------

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
        .join(name)
}

fn read_golden(name: &str) -> String {
    std::fs::read_to_string(golden_path(name))
        .unwrap_or_else(|e| panic!("reading golden file {name}: {e}"))
}

fn fake_bridge() -> PathBuf {
    PathBuf::from("/fake/home/bridge-goldenvault")
}

fn fake_log() -> PathBuf {
    PathBuf::from("/fake/home/Library/Logs/tephra-obsidian-goldenvault.log")
}

fn direct_invocation() -> tephra::obsidian::ObInvocation {
    tephra::obsidian::ObInvocation::Direct(PathBuf::from("/fake/bin/ob"))
}

fn pinned_invocation() -> tephra::obsidian::ObInvocation {
    tephra::obsidian::ObInvocation::Pinned {
        node: PathBuf::from("/fake/node/bin/node"),
        script: PathBuf::from("/fake/lib/node_modules/obsidian-headless/cli.js"),
    }
}

#[test]
fn obsidian_launchd_plist_direct_matches_golden_file() {
    let got = tephra::obsidian::generate_obsidian_launchd_plist(
        &direct_invocation(),
        "goldenvault",
        &fake_bridge(),
        &fake_log(),
    );
    assert_eq!(got, read_golden("obsidian-launchd.plist"));
}

#[test]
fn obsidian_launchd_plist_node_pinned_matches_golden_file() {
    let got = tephra::obsidian::generate_obsidian_launchd_plist(
        &pinned_invocation(),
        "goldenvault",
        &fake_bridge(),
        &fake_log(),
    );
    assert_eq!(got, read_golden("obsidian-launchd-node-pinned.plist"));
}

#[test]
fn obsidian_systemd_service_direct_matches_golden_file() {
    let got = tephra::obsidian::generate_obsidian_systemd_service(
        &direct_invocation(),
        "goldenvault",
        &fake_bridge(),
    );
    assert_eq!(got, read_golden("obsidian-systemd.service"));
}

#[test]
fn obsidian_systemd_service_node_pinned_matches_golden_file() {
    let got = tephra::obsidian::generate_obsidian_systemd_service(
        &pinned_invocation(),
        "goldenvault",
        &fake_bridge(),
    );
    assert_eq!(got, read_golden("obsidian-systemd-node-pinned.service"));
}

/// The generated plist must be valid property-list XML -- `plutil -lint` is
/// the same validator macOS itself uses when loading a LaunchAgent. Checks
/// both the direct and `--node`-pinned variants (see `tests/service.rs`'s
/// identical pattern for the bridge service's own plist).
#[cfg(target_os = "macos")]
#[test]
fn obsidian_launchd_plist_passes_plutil_lint_both_variants() {
    for (label, invocation) in [
        ("direct", direct_invocation()),
        ("pinned", pinned_invocation()),
    ] {
        let plist = tephra::obsidian::generate_obsidian_launchd_plist(
            &invocation,
            "goldenvault",
            &fake_bridge(),
            &fake_log(),
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("com.tephra.obsidian-sync.goldenvault.plist");
        std::fs::write(&path, &plist).unwrap();

        let output = std::process::Command::new("plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .expect("spawn `plutil -lint`");
        assert!(
            output.status.success(),
            "plutil -lint rejected the {label} variant: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

// --------------------------------------------------------------------
// doctor: driven via fake `ob` shims on PATH
// --------------------------------------------------------------------

/// Write an executable fake `ob` shim (a `/bin/sh` script dispatching on
/// `$1`) into `dir/ob`, and return `dir` cast as the `PATH` value to hand a
/// child process -- deliberately the ONLY entry, so the real `ob` (present
/// on this development machine, logged in, with a real bound vault) is
/// never reachable.
fn write_ob_shim(dir: &Path, script: &str) {
    let path = dir.join("ob");
    std::fs::write(&path, script).expect("write fake ob shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("chmod +x fake ob shim");
    }
}

const HAPPY_PATH_SHIM: &str = r#"#!/bin/sh
case "$1" in
  --version)
    echo "0.0.12"
    ;;
  sync-list-remote)
    echo "Fetching vaults..."
    echo ""
    echo "Vaults:"
    echo "  61901c20e85487f30ff10ae406143505  \"personal\"  (North America)"
    ;;
  sync-list-local)
    echo "Configured vaults:"
    echo "  61901c20e85487f30ff10ae406143505"
    echo "    Path: /fake/bridge"
    echo "    Host: sync-61.obsidian.md"
    ;;
  sync-status)
    echo "Sync Configuration:"
    echo "  Vault: personal (61901c20e85487f30ff10ae406143505)"
    echo "  Location: /fake/bridge"
    ;;
  *)
    echo "fake ob: unhandled subcommand $1" 1>&2
    exit 1
    ;;
esac
exit 0
"#;

/// Splice a replacement `case` arm for one subcommand into the happy-path
/// script, keeping the other three arms at their happy-path behavior --
/// isolates each test to the ONE check it's exercising.
fn shim_overriding(needle: &str, replacement: &str) -> String {
    assert!(
        HAPPY_PATH_SHIM.contains(needle),
        "fixture bug: {needle:?} not found in HAPPY_PATH_SHIM"
    );
    HAPPY_PATH_SHIM.replacen(needle, replacement, 1)
}

fn doctor_cmd(fx: &Fixture, path_dir: &Path) -> assert_cmd::Command {
    let mut cmd = fx.tephra_cmd();
    cmd.env("PATH", path_dir);
    cmd.arg("obsidian").arg("doctor").arg(&fx.name);
    cmd
}

#[test]
fn doctor_happy_path_all_checks_ok_and_exits_zero() {
    let fx = Fixture::new("goldenvault");
    let path_dir = tempfile::tempdir().unwrap();
    write_ob_shim(path_dir.path(), HAPPY_PATH_SHIM);

    doctor_cmd(&fx, path_dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains("ok: ob found on PATH ("))
        .stdout(predicates::str::contains("version 0.0.12"))
        .stdout(predicates::str::contains(
            "ok: logged in to Obsidian Sync (1 remote vault(s) visible)",
        ))
        .stdout(predicates::str::contains(
            "ok: native module binding loads (`ob sync-list-local` ran cleanly)",
        ))
        .stdout(predicates::str::contains(
            "ok: bridge bound (Vault: personal (61901c20e85487f30ff10ae406143505))",
        ))
        .stdout(predicates::str::contains("FAIL:").not());
}

#[test]
fn doctor_ob_missing_from_path_fails_check_one_and_skips_the_rest() {
    let fx = Fixture::new("goldenvault");
    // An empty PATH dir: no `ob` shim written at all.
    let path_dir = tempfile::tempdir().unwrap();

    doctor_cmd(&fx, path_dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("FAIL: ob not found on PATH"))
        .stdout(predicates::str::contains(
            "install: npm install -g obsidian-headless (Node 22+)",
        ))
        .stdout(predicates::str::contains(
            "warn: skipping remaining checks (ob unavailable)",
        ))
        .stdout(predicates::str::contains("logged in").not());
}

#[test]
fn doctor_not_logged_in_fails_check_two_with_login_remediation() {
    let fx = Fixture::new("goldenvault");
    let path_dir = tempfile::tempdir().unwrap();
    let script = shim_overriding(
        "  sync-list-remote)\n    echo \"Fetching vaults...\"\n    echo \"\"\n    echo \"Vaults:\"\n    echo \"  61901c20e85487f30ff10ae406143505  \\\"personal\\\"  (North America)\"\n    ;;\n",
        "  sync-list-remote)\n    echo \"No account logged in. Run \\\"ob login\\\" first.\" 1>&2\n    exit 1\n    ;;\n",
    );
    write_ob_shim(path_dir.path(), &script);

    doctor_cmd(&fx, path_dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains("ok: ob found on PATH ("))
        .stdout(predicates::str::contains(
            "FAIL: not logged in to Obsidian Sync",
        ))
        .stdout(predicates::str::contains("run: ob login"))
        // Checks 3 and 4 still run (the shim's happy-path responses for
        // them are untouched) -- only check 2 is isolated to failing.
        .stdout(predicates::str::contains(
            "ok: native module binding loads (`ob sync-list-local` ran cleanly)",
        ))
        .stdout(predicates::str::contains("ok: bridge bound ("));
}

#[test]
fn doctor_dlopen_failed_stderr_fails_check_three_with_native_binding_remediation() {
    let fx = Fixture::new("goldenvault");
    let path_dir = tempfile::tempdir().unwrap();
    let script = shim_overriding(
        "  sync-list-local)\n    echo \"Configured vaults:\"\n    echo \"  61901c20e85487f30ff10ae406143505\"\n    echo \"    Path: /fake/bridge\"\n    echo \"    Host: sync-61.obsidian.md\"\n    ;;\n",
        "  sync-list-local)\n    echo \"internal/modules/cjs/loader.js:1105\" 1>&2\n    echo \"Error: dlopen(.../better_sqlite3.node): ERR_DLOPEN_FAILED\" 1>&2\n    exit 1\n    ;;\n",
    );
    write_ob_shim(path_dir.path(), &script);

    doctor_cmd(&fx, path_dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains(
            "FAIL: native module binding failed to load",
        ))
        .stdout(predicates::str::contains(
            "reinstall obsidian-headless under the node your service uses; \
             if npm blocked build scripts, run: npm approve-scripts better-sqlite3",
        ))
        // Checks 2 and 4 still run and pass.
        .stdout(predicates::str::contains("ok: logged in to Obsidian Sync"))
        .stdout(predicates::str::contains("ok: bridge bound ("));
}

#[test]
fn doctor_no_sync_config_fails_check_four_with_sync_setup_remediation() {
    let fx = Fixture::new("goldenvault");
    let path_dir = tempfile::tempdir().unwrap();
    let script = shim_overriding(
        "  sync-status)\n    echo \"Sync Configuration:\"\n    echo \"  Vault: personal (61901c20e85487f30ff10ae406143505)\"\n    echo \"  Location: /fake/bridge\"\n    ;;\n",
        "  sync-status)\n    echo \"No sync configuration found for $3\" 1>&2\n    exit 3\n    ;;\n",
    );
    write_ob_shim(path_dir.path(), &script);

    doctor_cmd(&fx, path_dir.path())
        .assert()
        .failure()
        .code(1)
        .stdout(predicates::str::contains(
            "FAIL: bridge not bound to a synced vault",
        ))
        .stdout(predicates::str::contains(format!(
            "run: cd {} && ob sync-setup --vault {}",
            fx.bridge.display(),
            fx.name
        )))
        .stdout(predicates::str::contains(
            "(prompts for the E2E password -- interactive, yours to type)",
        ))
        // Checks 2 and 3 still run and pass.
        .stdout(predicates::str::contains("ok: logged in to Obsidian Sync"))
        .stdout(predicates::str::contains(
            "ok: native module binding loads (`ob sync-list-local` ran cleanly)",
        ));
}

/// A failure that ISN'T the specific "not logged in" text is a `warn:`, not
/// a `FAIL:` -- and a `warn` alone must not fail the overall exit code.
#[test]
fn doctor_other_sync_list_remote_failure_warns_without_failing_the_run() {
    let fx = Fixture::new("goldenvault");
    let path_dir = tempfile::tempdir().unwrap();
    let script = shim_overriding(
        "  sync-list-remote)\n    echo \"Fetching vaults...\"\n    echo \"\"\n    echo \"Vaults:\"\n    echo \"  61901c20e85487f30ff10ae406143505  \\\"personal\\\"  (North America)\"\n    ;;\n",
        "  sync-list-remote)\n    echo \"Error: network request timed out\" 1>&2\n    exit 1\n    ;;\n",
    );
    write_ob_shim(path_dir.path(), &script);

    doctor_cmd(&fx, path_dir.path())
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "warn: `ob sync-list-remote` failed: Error: network request timed out",
        ))
        .stdout(predicates::str::contains("FAIL:").not());
}
