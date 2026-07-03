//! Integration tests for `gitx`, driving real git repositories built by the
//! shared fixture (see `tests/common/mod.rs`).

mod common;

use std::path::PathBuf;

use common::Fixture;
use tephra::gitx;

fn git_ok(fx: &Fixture, dir: &std::path::Path, args: &[&str]) {
    let output = fx.git(dir, args);
    assert!(
        output.status.success(),
        "git -C {} {} failed: {}",
        dir.display(),
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn fixture_seeds_home_md_in_all_three_checkouts() {
    let fx = Fixture::new("testvault");

    // remote: bare repo, so check content via ls-tree rather than a
    // working-tree read.
    let output = fx.git(&fx.remote, &["ls-tree", "-r", "--name-only", "main"]);
    assert!(
        output.status.success(),
        "ls-tree failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let listing = String::from_utf8_lossy(&output.stdout);
    assert!(
        listing.lines().any(|l| l == "Home.md"),
        "remote should contain Home.md, got: {listing:?}"
    );

    for checkout in [&fx.bridge, &fx.agent] {
        let contents = std::fs::read_to_string(checkout.join("Home.md"))
            .unwrap_or_else(|e| panic!("reading Home.md in {checkout:?}: {e}"));
        assert_eq!(contents, "# Home\n");
    }
}

#[test]
fn bridge_upstream_resolves_to_origin_main() {
    let fx = Fixture::new("testvault");

    let upstream = gitx::upstream(&fx.bridge, "main").unwrap();

    assert_eq!(upstream, Some(("origin".to_string(), "main".to_string())));
}

#[test]
fn conflicted_paths_reports_unicode_add_add_conflict() {
    let fx = Fixture::new("testvault");
    let filename = "Café ☕.md";

    // Agent adds the file with one content and pushes.
    std::fs::write(fx.agent.join(filename), "AGENT CAFE\n").unwrap();
    git_ok(&fx, &fx.agent, &["add", "-A"]);
    git_ok(&fx, &fx.agent, &["commit", "--quiet", "-m", "memory: cafe"]);
    git_ok(&fx, &fx.agent, &["push", "--quiet", "origin", "main"]);

    // Bridge independently adds the same filename with different content
    // (an add/add conflict once the two histories are merged).
    std::fs::write(fx.bridge.join(filename), "HUMAN CAFE\n").unwrap();
    git_ok(&fx, &fx.bridge, &["add", "-A"]);
    git_ok(
        &fx,
        &fx.bridge,
        &["commit", "--quiet", "-m", "vault: human edits"],
    );

    git_ok(&fx, &fx.bridge, &["fetch", "--quiet", "origin"]);
    let merge = fx.git(&fx.bridge, &["merge", "--no-edit", "origin/main"]);
    assert!(
        !merge.status.success(),
        "expected an add/add merge conflict, but merge succeeded"
    );

    let conflicted = gitx::conflicted_paths(&fx.bridge).unwrap();
    assert_eq!(conflicted, vec![PathBuf::from(filename)]);

    git_ok(&fx, &fx.bridge, &["merge", "--abort"]);
}
