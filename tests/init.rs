//! Integration tests for `tephra init` (Task 10): non-interactive (`--yes`)
//! and interactive (piped-stdin) config writing/merging.
//!
//! Unlike most other integration test files, these don't use
//! `tests/common::Fixture` -- `init` never shells out to git and doesn't
//! need a bare remote/bridge/agent trio, just a `TEPHRA_CONFIG` path.

use predicates::prelude::PredicateBooleanExt;
use tempfile::tempdir;

fn tephra_cmd(config_path: &std::path::Path) -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("tephra").expect("find tephra binary");
    cmd.env("TEPHRA_CONFIG", config_path);
    cmd
}

// --- --yes: fresh file --------------------------------------------------

#[test]
fn yes_with_full_flags_writes_a_valid_loadable_config() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal")
        .arg("--work")
        .arg("/tmp/work-personal")
        .arg("--url")
        .arg("tailgit:obsidian-personal")
        .assert()
        .success()
        .stdout(predicates::str::contains("personal"));

    let cfg = tephra::config::load_from(&config_path).expect("written config should load");
    assert_eq!(cfg.vaults.len(), 1);
    let vault = &cfg.vaults["personal"];
    assert_eq!(vault.url, "tailgit:obsidian-personal");
    assert_eq!(vault.branch, "main");
}

#[test]
fn yes_honors_an_explicit_branch() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal")
        .arg("--work")
        .arg("/tmp/work-personal")
        .arg("--url")
        .arg("tailgit:obsidian-personal")
        .arg("--branch")
        .arg("trunk")
        .assert()
        .success();

    let cfg = tephra::config::load_from(&config_path).unwrap();
    assert_eq!(cfg.vaults["personal"].branch, "trunk");
}

#[test]
fn yes_without_a_required_flag_is_a_usage_error() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("personal")
        // --bridge/--work/--url all missing.
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("--bridge"));
}

// --- merge into an existing file ----------------------------------------

#[test]
fn merge_preserves_other_vault_and_a_hand_written_comment() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    std::fs::write(
        &config_path,
        "# hand-written comment, must survive the merge\n\
         [vaults.other]\n\
         bridge = \"/tmp/bridge-other\"\n\
         work = \"/tmp/work-other\"\n\
         url = \"tailgit:obsidian-other\"\n",
    )
    .unwrap();

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal")
        .arg("--work")
        .arg("/tmp/work-personal")
        .arg("--url")
        .arg("tailgit:obsidian-personal")
        .assert()
        .success();

    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        text.contains("# hand-written comment, must survive the merge"),
        "hand-written comment should survive the toml_edit merge, got:\n{text}"
    );

    let cfg = tephra::config::load_from(&config_path).unwrap();
    assert_eq!(cfg.vaults.len(), 2);
    assert_eq!(cfg.vaults["other"].url, "tailgit:obsidian-other");
    assert_eq!(cfg.vaults["personal"].url, "tailgit:obsidian-personal");
}

#[test]
fn duplicate_name_without_force_is_a_usage_error() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal")
        .arg("--work")
        .arg("/tmp/work-personal")
        .arg("--url")
        .arg("tailgit:obsidian-personal")
        .assert()
        .success();

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal-2")
        .arg("--work")
        .arg("/tmp/work-personal-2")
        .arg("--url")
        .arg("tailgit:obsidian-personal-2")
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("already exists"))
        .stderr(predicates::str::contains("--force"));

    // The original vault must be untouched by the refused attempt.
    let cfg = tephra::config::load_from(&config_path).unwrap();
    assert_eq!(cfg.vaults["personal"].url, "tailgit:obsidian-personal");
}

#[test]
fn force_replaces_an_existing_vault() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal")
        .arg("--work")
        .arg("/tmp/work-personal")
        .arg("--url")
        .arg("tailgit:obsidian-personal")
        .assert()
        .success();

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--force")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal-2")
        .arg("--work")
        .arg("/tmp/work-personal-2")
        .arg("--url")
        .arg("tailgit:obsidian-personal-2")
        .assert()
        .success();

    let cfg = tephra::config::load_from(&config_path).unwrap();
    assert_eq!(cfg.vaults.len(), 1);
    assert_eq!(cfg.vaults["personal"].url, "tailgit:obsidian-personal-2");
}

#[test]
fn force_replace_preserves_the_vaults_leading_comment_decor() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    // The replaced vault is NOT the first table in the file, so its leading
    // comment is genuinely the table's own decor (not the file header).
    std::fs::write(
        &config_path,
        "[vaults.other]\n\
         bridge = \"/tmp/bridge-other\"\n\
         work = \"/tmp/work-other\"\n\
         url = \"tailgit:obsidian-other\"\n\
         \n\
         # personal vault: leading comment, must survive --force replace\n\
         [vaults.personal]\n\
         bridge = \"/tmp/bridge-personal\"\n\
         work = \"/tmp/work-personal\"\n\
         url = \"tailgit:obsidian-personal\"\n",
    )
    .unwrap();

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--force")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal-2")
        .arg("--work")
        .arg("/tmp/work-personal-2")
        .arg("--url")
        .arg("tailgit:obsidian-personal-2")
        .assert()
        .success();

    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        text.contains("# personal vault: leading comment, must survive --force replace"),
        "the replaced vault's leading comment should survive, got:\n{text}"
    );

    let cfg = tephra::config::load_from(&config_path).unwrap();
    assert_eq!(cfg.vaults["personal"].url, "tailgit:obsidian-personal-2");
    assert_eq!(cfg.vaults["other"].url, "tailgit:obsidian-other");
}

#[test]
fn force_replace_of_the_first_table_preserves_the_file_header_comment() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    // A file-header comment sits as the FIRST table's leading decor in
    // toml_edit's model, so replacing that table is the case most likely to
    // eat it.
    std::fs::write(
        &config_path,
        "# tephra config, hand-annotated file header\n\
         [vaults.personal]\n\
         bridge = \"/tmp/bridge-personal\"\n\
         work = \"/tmp/work-personal\"\n\
         url = \"tailgit:obsidian-personal\"\n",
    )
    .unwrap();

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--force")
        .arg("--name")
        .arg("personal")
        .arg("--bridge")
        .arg("/tmp/bridge-personal-2")
        .arg("--work")
        .arg("/tmp/work-personal-2")
        .arg("--url")
        .arg("tailgit:obsidian-personal-2")
        .assert()
        .success();

    let text = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        text.contains("# tephra config, hand-annotated file header"),
        "the file-header comment should survive a --force replace of the first table, got:\n{text}"
    );

    let cfg = tephra::config::load_from(&config_path).unwrap();
    assert_eq!(cfg.vaults["personal"].url, "tailgit:obsidian-personal-2");
    assert_eq!(
        cfg.vaults["personal"].bridge.to_string_lossy(),
        "/tmp/bridge-personal-2"
    );
}

#[test]
fn invalid_name_is_a_usage_error_naming_the_charset() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    tephra_cmd(&config_path)
        .arg("init")
        .arg("--yes")
        .arg("--name")
        .arg("bad name")
        .arg("--bridge")
        .arg("/tmp/bridge-personal")
        .arg("--work")
        .arg("/tmp/work-personal")
        .arg("--url")
        .arg("tailgit:obsidian-personal")
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("bad name"))
        .stderr(predicates::str::contains(
            "ASCII letters, digits, '-', '_', and '.'",
        ));
}

// --- interactive path, driven via piped stdin ----------------------------

#[test]
fn interactive_prompts_driven_via_stdin_with_defaults_accepted() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // Prompt order is name, bridge, work, url, branch. Empty lines accept
    // the shown default (bridge/work/branch); name and url have no default
    // so they must be typed.
    tephra_cmd(&config_path)
        .write_stdin("personal\n\n\ntailgit:obsidian-personal\n\n")
        .arg("init")
        .assert()
        .success();

    let cfg = tephra::config::load_from(&config_path).unwrap();
    let vault = &cfg.vaults["personal"];
    assert_eq!(vault.url, "tailgit:obsidian-personal");
    assert_eq!(vault.branch, "main");
    assert!(
        vault.bridge.to_string_lossy().contains("bridge-personal"),
        "bridge should fall back to its templated default, got: {:?}",
        vault.bridge
    );
    assert!(
        vault.work.to_string_lossy().contains("work-personal"),
        "work should fall back to its templated default, got: {:?}",
        vault.work
    );
}

#[test]
fn interactive_bad_name_fails_before_any_further_prompt() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // stdin carries ONLY the (invalid) name. If init validated late -- after
    // prompting for bridge/work/url -- those prompts would hit EOF first and
    // the error would be "stdin closed", not the charset message. Asserting
    // the charset error (and its absence) proves validation happens
    // immediately after the name prompt.
    tephra_cmd(&config_path)
        .write_stdin("bad name\n")
        .arg("init")
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("bad name"))
        .stderr(predicates::str::contains(
            "ASCII letters, digits, '-', '_', and '.'",
        ))
        .stderr(predicates::str::contains("stdin closed").not())
        .stdout(predicates::str::contains("bridge path").not());
}

#[test]
fn interactive_prompts_with_no_input_at_all_is_a_clear_usage_error() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    // Closed/empty stdin: the very first prompt (vault name) hits EOF
    // immediately.
    tephra_cmd(&config_path)
        .write_stdin("")
        .arg("init")
        .assert()
        .failure()
        .code(2)
        .stderr(predicates::str::contains("stdin closed"))
        .stderr(predicates::str::contains("--yes"));
}
