//! Integration tests for `tephra agent init`: scaffolding `AGENTS.md` +
//! byte-identical `CLAUDE.md` from the embedded template. See
//! `docs/DESIGN.md` §Agent awareness and `templates/AGENTS.md`.

mod common;

use std::fs;

use common::Fixture;

// --- 1. scaffold after clone ------------------------------------------------

#[test]
fn init_scaffolds_agents_and_claude_md_after_clone() {
    let fx = Fixture::new("testvault");

    fx.tephra_cmd()
        .arg("agent")
        .arg("init")
        .arg(&fx.name)
        .assert()
        .success();

    let agents_path = fx.agent.join("AGENTS.md");
    let claude_path = fx.agent.join("CLAUDE.md");
    assert!(agents_path.exists(), "AGENTS.md should be written");
    assert!(claude_path.exists(), "CLAUDE.md should be written");

    let agents_content = fs::read_to_string(&agents_path).unwrap();
    let claude_content = fs::read_to_string(&claude_path).unwrap();
    assert_eq!(
        agents_content, claude_content,
        "AGENTS.md and CLAUDE.md must be byte-identical"
    );

    assert!(
        agents_content.contains(&fx.name),
        "scaffolded content should contain the vault name, got: {agents_content}"
    );
    let url = fx.remote.display().to_string();
    assert!(
        agents_content.contains(&url),
        "scaffolded content should contain the vault url, got: {agents_content}"
    );
    assert!(
        !agents_content.contains('{'),
        "no {{placeholder}} should remain unsubstituted, got: {agents_content}"
    );
}

// --- 2. no-overwrite without --force; --force overwrites --------------------

#[test]
fn init_refuses_to_overwrite_without_force_then_force_overwrites() {
    let fx = Fixture::new("testvault");

    fx.tephra_cmd()
        .arg("agent")
        .arg("init")
        .arg(&fx.name)
        .assert()
        .success();

    // Second run without --force must fail, naming both existing files.
    fx.tephra_cmd()
        .arg("agent")
        .arg("init")
        .arg(&fx.name)
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("AGENTS.md"))
        .stderr(predicates::str::contains("CLAUDE.md"))
        .stderr(predicates::str::contains("--force"));

    // Plant sentinel content to prove --force actually replaces it.
    fs::write(fx.agent.join("AGENTS.md"), "SENTINEL AGENTS CONTENT\n").unwrap();
    fs::write(fx.agent.join("CLAUDE.md"), "SENTINEL CLAUDE CONTENT\n").unwrap();

    fx.tephra_cmd()
        .arg("agent")
        .arg("init")
        .arg(&fx.name)
        .arg("--force")
        .assert()
        .success();

    let agents_content = fs::read_to_string(fx.agent.join("AGENTS.md")).unwrap();
    let claude_content = fs::read_to_string(fx.agent.join("CLAUDE.md")).unwrap();
    assert_ne!(
        agents_content, "SENTINEL AGENTS CONTENT\n",
        "--force should replace the planted AGENTS.md sentinel"
    );
    assert_ne!(
        claude_content, "SENTINEL CLAUDE CONTENT\n",
        "--force should replace the planted CLAUDE.md sentinel"
    );
    assert_eq!(agents_content, claude_content);
}

// --- 3. missing clone --------------------------------------------------------

#[test]
fn init_on_missing_clone_errors_and_names_the_clone_command() {
    let fx = Fixture::new("testvault");
    fs::remove_dir_all(&fx.agent).unwrap();

    fx.tephra_cmd()
        .arg("agent")
        .arg("init")
        .arg(&fx.name)
        .assert()
        .failure()
        .code(1)
        .stderr(predicates::str::contains("not cloned; run: tephra clone"));
}
