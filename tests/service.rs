//! Integration tests for `tephra service install|uninstall|status` (Task 7).
//!
//! Two tiers:
//!
//! - Golden-file tests: compare the pure unit-generation functions'
//!   output, for a FIXED fake exe path (`/fake/bin/tephra`) and vault
//!   (`goldenvault`), byte-for-byte against `tests/golden/*`. These run on
//!   every platform/CI job -- generation takes no platform action and
//!   touches no real paths.
//! - A real-`launchctl` runtime cycle (macOS only, and only when
//!   `TEPHRA_TEST_SERVICES=1`): install -> status(0) -> uninstall ->
//!   status(1) against the `gui` domain, using a fixture-registered vault
//!   named `tephratest-<pid>` so it can never collide with (or be mistaken
//!   for) a real vault's service. CI never sets this env var -- there is no
//!   way to exercise a real launchd/systemd instance from a CI sandbox --
//!   so this test is a no-op (prints a skip note and returns) there.
//!   Equivalent Linux coverage isn't practical to add here (this
//!   development machine is a Mac): the systemd generation/path-resolution
//!   halves already have full unit + golden coverage, per this plan's
//!   self-review note that "platform-runtime service tests can't run in CI
//!   both ways."

mod common;

use std::path::PathBuf;

use common::Fixture;
use tephra::service;

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

#[test]
fn launchd_plist_matches_golden_file() {
    let exe = std::path::Path::new("/fake/bin/tephra");
    let log = std::path::Path::new("/fake/home/Library/Logs/tephra-goldenvault.log");
    let got = service::generate_launchd_plist(exe, "goldenvault", log);
    assert_eq!(got, read_golden("launchd.plist"));
}

#[test]
fn systemd_service_matches_golden_file() {
    let exe = std::path::Path::new("/fake/bin/tephra");
    let got = service::generate_systemd_service(exe, "goldenvault");
    assert_eq!(got, read_golden("systemd.service"));
}

#[test]
fn systemd_timer_matches_golden_file() {
    let got = service::generate_systemd_timer("goldenvault");
    assert_eq!(got, read_golden("systemd.timer"));
}

/// The generated plist must be valid property-list XML, not merely a
/// string that happens to look right -- `plutil -lint` is the same
/// validator macOS itself uses when loading a LaunchAgent.
#[cfg(target_os = "macos")]
#[test]
fn launchd_plist_passes_plutil_lint() {
    let exe = std::path::Path::new("/fake/bin/tephra");
    let log = std::path::Path::new("/fake/home/Library/Logs/tephra-goldenvault.log");
    let plist = service::generate_launchd_plist(exe, "goldenvault", log);

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("com.tephra.goldenvault.plist");
    std::fs::write(&path, &plist).unwrap();

    let output = std::process::Command::new("plutil")
        .arg("-lint")
        .arg(&path)
        .output()
        .expect("spawn `plutil -lint`");
    assert!(
        output.status.success(),
        "plutil -lint rejected the generated plist: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Full install -> status(0) -> uninstall -> status(1) cycle against the
/// REAL `launchctl` `gui` domain on this machine. Gated behind
/// `TEPHRA_TEST_SERVICES=1` (never set in CI) so a normal `cargo test` run
/// never touches this developer's actual launchd state.
///
/// Uses a distinctive, PID-suffixed vault name so no run can collide with a
/// real vault's service, and guarantees `service uninstall` runs -- via a
/// `Drop` guard -- even if an assertion panics partway through, so no
/// `tephratest-*` LaunchAgent is ever left registered on this Mac.
///
/// Verified manually once with `TEPHRA_TEST_SERVICES=1 cargo test --test
/// service install_status_uninstall_cycle_against_real_launchctl`
/// (see the task report for the transcript); `launchctl list | grep
/// tephratest` was empty afterward.
#[cfg(target_os = "macos")]
#[test]
fn install_status_uninstall_cycle_against_real_launchctl() {
    if std::env::var("TEPHRA_TEST_SERVICES").as_deref() != Ok("1") {
        eprintln!(
            "skipping install_status_uninstall_cycle_against_real_launchctl: \
             set TEPHRA_TEST_SERVICES=1 to run this against the real launchctl \
             gui domain on this Mac (CI never sets this)."
        );
        return;
    }

    let vault_name = format!("tephratest-{}", std::process::id());
    let fx = Fixture::new(&vault_name);

    // RAII guard: runs `tephra service uninstall` in `Drop` so the real
    // service is torn down even if a `.assert()` below panics. The cycle's
    // own explicit uninstall call (below) makes this second call a no-op
    // idempotent uninstall in the success path.
    struct UninstallGuard<'a> {
        fx: &'a Fixture,
    }
    impl Drop for UninstallGuard<'_> {
        fn drop(&mut self) {
            let _ = self
                .fx
                .tephra_cmd()
                .arg("service")
                .arg("uninstall")
                .arg(&self.fx.name)
                .output();
        }
    }
    let _guard = UninstallGuard { fx: &fx };

    fx.tephra_cmd()
        .arg("service")
        .arg("install")
        .arg(&fx.name)
        .assert()
        .success();

    fx.tephra_cmd()
        .arg("service")
        .arg("status")
        .arg(&fx.name)
        .assert()
        .success();

    fx.tephra_cmd()
        .arg("service")
        .arg("uninstall")
        .arg(&fx.name)
        .assert()
        .success();

    fx.tephra_cmd()
        .arg("service")
        .arg("status")
        .arg(&fx.name)
        .assert()
        .failure()
        .code(1);

    // A second uninstall must be a no-op success (idempotent), not an
    // error, per DESIGN.md's service-management contract.
    fx.tephra_cmd()
        .arg("service")
        .arg("uninstall")
        .arg(&fx.name)
        .assert()
        .success();
}
