use agent_server::skills::{CatalogLimits, DiagnosticKind, SkillCatalog};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

fn add_skill(root: &Path, directory: &str, name: &str, description: &str, body: &str) {
    let skill_dir = root.join(directory);
    fs::create_dir_all(&skill_dir).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: {description}\n---\n{body}"),
    )
    .unwrap();
}

#[test]
fn existing_skills_are_available_immediately_after_startup() {
    let root = TempDir::new().unwrap();
    add_skill(
        root.path(),
        "startup",
        "startup",
        "Available at boot",
        "Read this.",
    );
    let catalog = SkillCatalog::new(vec![root.path().to_path_buf()]);
    assert_eq!(catalog.snapshot().list()[0].name, "startup");
}

#[test]
fn reload_publishes_new_skills_and_activation_is_a_tool_result() {
    let root = TempDir::new().unwrap();
    let catalog = SkillCatalog::new(vec![root.path().to_path_buf()]);
    let before_reload = catalog.snapshot();
    add_skill(
        root.path(),
        "drafting",
        "drafting",
        "Help draft reports",
        "Use concise headings.",
    );

    let report = catalog.reload();
    let after_reload = catalog.snapshot();

    assert_eq!(report.skill_count, 1);
    assert!(before_reload.list().is_empty());
    assert_eq!(after_reload.list()[0].name, "drafting");
    assert_eq!(after_reload.search("REPORT").len(), 1);

    let tool_result = after_reload.activate("drafting").unwrap();
    assert_eq!(tool_result.name, "drafting");
    assert_eq!(tool_result.content, "Use concise headings.");
}

#[test]
fn reload_swaps_for_new_requests_without_mutating_an_existing_snapshot() {
    let root = TempDir::new().unwrap();
    add_skill(
        root.path(),
        "editor",
        "editor",
        "Edit text",
        "Original instructions.",
    );
    let catalog = SkillCatalog::new(vec![root.path().to_path_buf()]);
    let first_generation = catalog.reload().generation;
    let in_flight_request = catalog.snapshot();

    add_skill(
        root.path(),
        "editor",
        "editor",
        "Edit text",
        "Updated instructions.",
    );
    let second_generation = catalog.reload().generation;
    let new_request = catalog.snapshot();

    assert!(second_generation > first_generation);
    assert_eq!(
        in_flight_request.activate("editor").unwrap().content,
        "Original instructions."
    );
    assert_eq!(
        new_request.activate("editor").unwrap().content,
        "Updated instructions."
    );
}

#[test]
fn reload_removes_a_skill_when_its_skill_file_is_deleted() {
    let root = TempDir::new().unwrap();
    add_skill(
        root.path(),
        "summarize",
        "summarize",
        "Summarize text",
        "Be brief.",
    );
    let catalog = SkillCatalog::new(vec![root.path().to_path_buf()]);
    assert_eq!(catalog.reload().skill_count, 1);

    fs::remove_file(root.path().join("summarize/SKILL.md")).unwrap();
    let report = catalog.reload();

    assert_eq!(report.skill_count, 0);
    assert!(catalog.snapshot().list().is_empty());
}

#[test]
fn corrupt_skill_is_excluded_without_hiding_other_valid_skills() {
    let root = TempDir::new().unwrap();
    add_skill(root.path(), "good", "good", "A valid skill", "Good body.");
    let broken = root.path().join("broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("SKILL.md"), "---\nname: [invalid\n---\nbody").unwrap();
    let catalog = SkillCatalog::new(vec![root.path().to_path_buf()]);

    let report = catalog.reload();

    assert_eq!(report.skill_count, 1);
    assert_eq!(catalog.snapshot().list()[0].name, "good");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidSkill)
    );
}

#[test]
fn oversized_skill_file_is_rejected_at_the_read_bound() {
    let root = TempDir::new().unwrap();
    add_skill(
        root.path(),
        "oversized",
        "oversized",
        "Too large",
        "This body exceeds the configured maximum.",
    );
    let catalog = SkillCatalog::with_limits(
        vec![root.path().to_path_buf()],
        CatalogLimits {
            max_roots: 1,
            max_entries_per_root: 1,
            max_skills_per_root: 1,
            max_skill_file_bytes: 32,
        },
    );

    let report = catalog.reload();

    assert_eq!(report.skill_count, 0);
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::InvalidSkill)
    );
}

#[test]
fn root_entry_limit_discards_the_root_instead_of_publishing_a_partial_scan() {
    let root = TempDir::new().unwrap();
    add_skill(
        root.path(),
        "already-read",
        "already-read",
        "Valid",
        "Body.",
    );
    fs::create_dir(root.path().join("another-entry")).unwrap();
    let catalog = SkillCatalog::with_limits(
        vec![root.path().to_path_buf()],
        CatalogLimits {
            max_roots: 1,
            max_entries_per_root: 1,
            max_skills_per_root: 10,
            max_skill_file_bytes: 1024,
        },
    );

    let report = catalog.reload();

    assert_eq!(report.skill_count, 0);
    assert!(catalog.snapshot().list().is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::EntryLimitExceeded)
    );
}

#[test]
fn unavailable_root_does_not_fall_back_to_the_previous_snapshot() {
    let root = TempDir::new().unwrap();
    add_skill(
        root.path(),
        "usable",
        "usable",
        "Usable",
        "Previously available.",
    );
    let root_path = root.path().to_path_buf();
    let catalog = SkillCatalog::new(vec![root_path.clone()]);
    assert_eq!(catalog.reload().skill_count, 1);

    fs::remove_dir_all(&root_path).unwrap();
    let report = catalog.reload();

    assert_eq!(report.skill_count, 0);
    assert!(catalog.snapshot().list().is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::RootUnavailable)
    );
}

#[test]
fn unavailable_root_does_not_hide_skills_from_other_roots() {
    let valid_root = TempDir::new().unwrap();
    let missing_root = TempDir::new().unwrap();
    add_skill(
        valid_root.path(),
        "healthy",
        "healthy",
        "Healthy root skill",
        "Available.",
    );
    let missing_root_path = missing_root.path().to_path_buf();
    drop(missing_root);
    let catalog = SkillCatalog::new(vec![valid_root.path().to_path_buf(), missing_root_path]);

    let report = catalog.reload();

    assert_eq!(report.skill_count, 1);
    assert_eq!(catalog.snapshot().list()[0].name, "healthy");
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::RootUnavailable)
    );
}

#[test]
fn colliding_names_are_all_excluded() {
    let first_root = TempDir::new().unwrap();
    let second_root = TempDir::new().unwrap();
    add_skill(
        first_root.path(),
        "shared",
        "shared",
        "First definition",
        "First.",
    );
    add_skill(
        second_root.path(),
        "shared",
        "shared",
        "Second definition",
        "Second.",
    );
    let catalog = SkillCatalog::new(vec![
        first_root.path().to_path_buf(),
        second_root.path().to_path_buf(),
    ]);

    let report = catalog.reload();

    assert_eq!(report.skill_count, 0);
    assert!(catalog.snapshot().list().is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.kind == DiagnosticKind::NameCollision)
    );
}

#[cfg(unix)]
#[test]
fn symlinked_skill_files_are_not_activated() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    fs::write(outside.path().join("SKILL.md"), "secret").unwrap();
    let skill_dir = root.path().join("linked");
    fs::create_dir_all(&skill_dir).unwrap();
    symlink(outside.path().join("SKILL.md"), skill_dir.join("SKILL.md")).unwrap();
    fs::write(
        skill_dir.join("metadata.yml"),
        "---\nname: linked\ndescription: Linked file\n---\n",
    )
    .unwrap();
    let catalog = SkillCatalog::new(vec![root.path().to_path_buf()]);

    let report = catalog.reload();

    assert_eq!(report.skill_count, 0);
    assert!(catalog.snapshot().list().is_empty());
}
