//! First connect by clone, end to end on scratch libraries: a local library
//! with no `.git` connects to a backup remote through the app's own clone,
//! then the app's post-clone reconcile and a later metadata rewrite run, as
//! they would in the app (`git_backup_clone`) or the CLI's next command.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::json;

use crate::core::fork_files::{self, ForkConcern};
use crate::core::skill_store::{ScenarioRecord, SkillRecord, SkillStore};
use crate::core::sync_metadata::{
    self, ScenarioMetaFile, ScenarioSkillMetaFile, SkillMetaFile, SourceMeta,
};
use crate::core::{central_repo, git_backup};

const SCHEMA: &[u8] =
    b"{\n  \"schema_version\": 1,\n  \"app_min_version\": \"2.0.0\",\n  \"created_by\": \"skills-manager\"\n}\n";

struct Env {
    _guard: std::sync::MutexGuard<'static, ()>,
    tmp: tempfile::TempDir,
    remote: PathBuf,
    seed: PathBuf,
}

impl Drop for Env {
    fn drop(&mut self) {
        central_repo::set_test_base_dir_override(None);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=test@example.com", "-c", "user.name=Seed"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn setup() -> Env {
    let guard = central_repo::test_base_dir_lock();
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.git");
    let seed = tmp.path().join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    git(tmp.path(), &["init", "--bare", "--initial-branch=main", remote.to_str().unwrap()]);
    git(&seed, &["init", "-b", "main"]);
    git(&seed, &["remote", "add", "origin", remote.to_str().unwrap()]);
    Env {
        _guard: guard,
        tmp,
        remote,
        seed,
    }
}

impl Env {
    /// The backup another device pushed: these files, committed and pushed.
    fn remote_holds(&self, files: &[(&str, Vec<u8>)]) {
        for (rel, bytes) in files {
            let path = self.seed.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }
        git(&self.seed, &["add", "-A"]);
        git(&self.seed, &["commit", "-m", "backup from another device"]);
        git(&self.seed, &["push", "-u", "origin", "main"]);
    }

    /// The device that connects: a library with no `.git` yet.
    fn local(&self) -> Local {
        let base = self.tmp.path().join("local");
        let skills = base.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        central_repo::set_test_base_dir_override(Some(base.clone()));
        let store = SkillStore::new(&base.join("db.sqlite")).unwrap();
        Local { skills, store }
    }

    fn remote_url(&self) -> &str {
        self.remote.to_str().unwrap()
    }
}

struct Local {
    skills: PathBuf,
    store: SkillStore,
}

fn skill_md(name: &str) -> String {
    format!("---\nname: {name}\ndescription: test skill\n---\nsame on every device\n")
}

impl Local {
    fn add_skill(&self, id: &str, folder: &str, git_source: Option<&str>) {
        let dir = self.skills.join(folder);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), skill_md(folder)).unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        self.store
            .insert_skill(&SkillRecord {
                id: id.to_string(),
                name: folder.to_string(),
                description: None,
                source_type: if git_source.is_some() { "git" } else { "import" }.to_string(),
                source_ref: git_source.map(str::to_string),
                source_ref_resolved: None,
                source_subpath: None,
                source_branch: git_source.map(|_| "main".to_string()),
                source_revision: git_source.map(|_| "1111111111111111111111111111111111111111".to_string()),
                remote_revision: None,
                central_path: dir.to_string_lossy().to_string(),
                content_hash: None,
                enabled: true,
                created_at: now,
                updated_at: now,
                status: "ok".to_string(),
                update_status: "unknown".to_string(),
                last_checked_at: None,
                last_check_error: None,
            })
            .unwrap();
    }

    fn add_preset(&self, id: &str, name: &str, members: &[&str]) {
        self.store
            .insert_scenario(&ScenarioRecord {
                id: id.to_string(),
                name: name.to_string(),
                description: None,
                icon: None,
                sort_order: 0,
                created_at: 1,
                updated_at: 1,
            })
            .unwrap();
        for member in members {
            self.store.add_skill_to_scenario(id, member).unwrap();
        }
    }

    /// Write the metadata the way the app does after every change.
    fn write_metadata(&self) {
        sync_metadata::write_all_from_db_unlocked(&self.store).unwrap();
    }

    fn fork_file(&self, concern: ForkConcern, id: &str) -> Option<Vec<u8>> {
        std::fs::read(fork_files::path_for(concern, id).unwrap()).ok()
    }

    /// The app's connect-by-clone (`git_backup_clone`): clone, reconcile the
    /// index; then a later operation rewrites the metadata (and sweeps fork
    /// files whose skill has no record).
    fn connect_by_clone(&self, url: &str) -> anyhow::Result<()> {
        git_backup::clone_into_unlocked(&self.skills, url, &[])?;
        crate::commands::git_backup::reconcile_skills_index_unlocked(&self.store)?;
        sync_metadata::write_all_from_db_unlocked(&self.store)?;
        Ok(())
    }

    fn ids_at(&self, folder: &str) -> Vec<String> {
        let path = self.skills.join(folder).to_string_lossy().to_string();
        self.store
            .get_all_skills()
            .unwrap()
            .into_iter()
            .filter(|s| s.central_path == path)
            .map(|s| s.id)
            .collect()
    }

    fn preset_members(&self, preset_id: &str) -> BTreeSet<String> {
        self.store
            .get_skill_ids_for_scenario(preset_id)
            .unwrap()
            .into_iter()
            .collect()
    }
}

fn skill_meta(id: &str, path: &str) -> Vec<u8> {
    sync_metadata::canonical_json_bytes(&SkillMetaFile {
        schema_version: 1,
        skill_id: id.to_string(),
        path: path.to_string(),
        path_key: sync_metadata::path_key(path),
        enabled: true,
        tags: Vec::new(),
        source: SourceMeta {
            source_type: "import".to_string(),
            ref_: None,
            subpath: None,
            branch: None,
        },
    })
    .unwrap()
}

fn preset_meta(id: &str, name: &str) -> Vec<u8> {
    sync_metadata::canonical_json_bytes(&ScenarioMetaFile {
        schema_version: 1,
        scenario_id: id.to_string(),
        name: name.to_string(),
        description: None,
        icon: None,
        sort_order: 0,
    })
    .unwrap()
}

fn member_meta(preset_id: &str, skill_id: &str, sort_order: i32) -> Vec<u8> {
    sync_metadata::canonical_json_bytes(&ScenarioSkillMetaFile {
        schema_version: 1,
        scenario_id: preset_id.to_string(),
        skill_id: skill_id.to_string(),
        sort_order,
        tools: Default::default(),
    })
    .unwrap()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

const ADAPTED: [ForkConcern; 4] = [
    ForkConcern::Baseline,
    ForkConcern::Pin,
    ForkConcern::Notes,
    ForkConcern::Watch,
];

const SOLO_REVISION: &str = "1111111111111111111111111111111111111111";

/// A baseline, a pin, a change note and a watch, each written as its own
/// module writes it (the pin through `pinning::pin` itself).
fn write_adapted_state(local: &Local, id: &str) {
    let baseline = crate::core::baseline::Baseline {
        skill_id: id.to_string(),
        revision: SOLO_REVISION.to_string(),
        origin: crate::core::baseline::Origin::Install,
        recorded_at: 1,
        tree_id: None,
        content_hash: None,
        files: Vec::new(),
    };
    fork_files::write(ForkConcern::Baseline, id, &baseline).unwrap();
    crate::core::pinning::pin(&local.store, id, Some("reviewed")).unwrap();
    let notes = json!({
        "skill_id": id,
        "notes": [{ "path": "SKILL.md", "note": "edited on this device", "updated_at": 1 }],
    });
    fork_files::write(ForkConcern::Notes, id, &notes).unwrap();
    let watch = json!({
        "version": 1,
        "upstreams": [{
            "id": "w1",
            "git_url": "https://example.invalid/borrowed.git",
            "last_seen_revision": SOLO_REVISION,
            "added_at": 1,
        }],
    });
    fork_files::write(ForkConcern::Watch, id, &watch).unwrap();
    assert_eq!(crate::core::change_notes::load(id).unwrap().len(), 1);
    assert_eq!(crate::core::watched_upstreams::list(id).unwrap().len(), 1);
}

/// The scenario from sm-69q.11 (11), widened to everything a first connect
/// can drop: a local-only adapted skill (source, tags, baseline, pin, notes,
/// watch), a skill both sides installed under different ids, a local-only
/// preset, and a local preset whose name the remote already uses.
#[test]
fn first_connect_by_clone_keeps_local_only_skills_presets_and_fork_state() {
    let env = setup();
    env.remote_holds(&[
        ("shared/SKILL.md", skill_md("shared").into_bytes()),
        (".skills-manager/schema.json", SCHEMA.to_vec()),
        (".skills-manager/skills/shared-a.json", skill_meta("shared-a", "shared")),
        (".skills-manager/scenarios/work-a.json", preset_meta("work-a", "Work")),
        (".skills-manager/scenario-skills/work-a/shared-a.json", member_meta("work-a", "shared-a", 0)),
    ]);

    let local = env.local();
    local.add_skill("shared-d", "shared", None);
    local.add_skill("solo-d", "solo", Some("https://example.invalid/solo.git"));
    local
        .store
        .set_tags_for_skill("solo-d", &["mine".to_string()])
        .unwrap();
    local.add_preset("mine-d", "Mine", &["solo-d", "shared-d"]);
    local.add_preset("work-d", "Work", &["solo-d"]);
    local.write_metadata();
    write_adapted_state(&local, "solo-d");
    let fork_before: Vec<_> = ADAPTED.iter().map(|c| local.fork_file(*c, "solo-d")).collect();

    local.connect_by_clone(env.remote_url()).unwrap();

    let mut lost = Vec::new();
    // Read back the way `skills show` / `status` / `pins` / `watch list` do.
    if crate::core::baseline::summary_for("solo-d").map(|b| b.revision)
        != Some(SOLO_REVISION.to_string())
    {
        lost.push("solo-d: baseline (as skills show reads it)".to_string());
    }
    if crate::core::pinning::active("solo-d").and_then(|p| p.note).as_deref() != Some("reviewed") {
        lost.push("solo-d: pin (as skills pins reads it)".to_string());
    }
    if crate::core::change_notes::load("solo-d").map(|n| n.len()).unwrap_or(0) != 1 {
        lost.push("solo-d: change note (as skills changes reads it)".to_string());
    }
    if crate::core::watched_upstreams::list("solo-d").map(|w| w.len()).unwrap_or(0) != 1 {
        lost.push("solo-d: watch (as skills watch list reads it)".to_string());
    }
    match local.store.get_skill_by_id("solo-d").unwrap() {
        None => lost.push("solo-d: library record (skill not found)".to_string()),
        Some(solo) => {
            if solo.source_type != "git"
                || solo.source_ref.as_deref() != Some("https://example.invalid/solo.git")
                || solo.source_branch.as_deref() != Some("main")
            {
                lost.push(format!("solo-d: source ({} {:?})", solo.source_type, solo.source_ref));
            }
            let tags = local.store.get_tags_map().unwrap().remove("solo-d");
            if tags != Some(vec!["mine".to_string()]) {
                lost.push(format!("solo-d: tags ({tags:?})"));
            }
        }
    }
    if !local.skills.join("solo/SKILL.md").exists() {
        lost.push("solo/: the folder".to_string());
    }
    for (concern, before) in ADAPTED.iter().zip(&fork_before) {
        if local.fork_file(*concern, "solo-d") != *before {
            lost.push(format!("solo-d: {}", concern.stem()));
        }
    }
    let shared_ids = local.ids_at("shared");
    if shared_ids != vec!["shared-a".to_string()] {
        lost.push(format!("shared/: expected exactly the remote's record, got {shared_ids:?}"));
    }
    let presets: BTreeSet<(String, String)> = local
        .store
        .get_all_scenarios()
        .unwrap()
        .into_iter()
        .map(|p| (p.id, p.name))
        .collect();
    let expected_presets: BTreeSet<(String, String)> = [("mine-d", "Mine"), ("work-a", "Work")]
        .into_iter()
        .map(|(id, name)| (id.to_string(), name.to_string()))
        .collect();
    if presets != expected_presets {
        lost.push(format!("presets: {presets:?}"));
    }
    if local.preset_members("mine-d") != set(&["solo-d", "shared-a"]) {
        lost.push(format!("preset Mine members: {:?}", local.preset_members("mine-d")));
    }
    if local.preset_members("work-a") != set(&["shared-a", "solo-d"]) {
        lost.push(format!("preset Work members: {:?}", local.preset_members("work-a")));
    }

    assert!(lost.is_empty(), "first connect by clone lost:\n  {}", lost.join("\n  "));
}

/// Every file under `root` with its bytes, `.git` aside.
fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<_> = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.file_name() != ".git")
        .map(|e| e.unwrap())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let rel = e.path().strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
            (rel, std::fs::read(e.path()).unwrap())
        })
        .collect();
    files.sort();
    files
}

fn backups_beside(skills: &Path) -> Vec<String> {
    std::fs::read_dir(skills.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("skills-backup-before-clone"))
        .collect()
}

/// A failure inside the carry step keeps `clone_into_unlocked`'s rollback
/// contract: the library is as it was, with no `.git` and no backup left.
#[test]
fn a_failure_while_carrying_puts_the_local_library_back() {
    let env = setup();
    env.remote_holds(&[
        ("shared/SKILL.md", skill_md("shared").into_bytes()),
        (".skills-manager/schema.json", SCHEMA.to_vec()),
        (".skills-manager/skills/shared-a.json", skill_meta("shared-a", "shared")),
    ]);
    let local = env.local();
    local.add_skill("solo-d", "solo", Some("https://example.invalid/solo.git"));
    local.add_preset("mine-d", "Mine", &["solo-d"]);
    local.write_metadata();
    fork_files::write(ForkConcern::Pin, "solo-d", &json!({ "pinned": true })).unwrap();
    // A record the carry step cannot read: it fails after the clone and the
    // merge of local folders have already happened.
    std::fs::write(local.skills.join(".skills-manager/skills/broken.json"), "{ not json").unwrap();
    let before = snapshot(&local.skills);

    let err = git_backup::clone_into_unlocked(&local.skills, env.remote_url(), &[]).unwrap_err();

    let msg = format!("{err:#}");
    assert!(msg.contains("broken.json"), "the unreadable file is not named: {msg}");
    assert!(!local.skills.join(".git").exists(), "the clone was left live");
    assert!(!local.skills.join("shared").exists(), "the clone's content was left live");
    assert_eq!(snapshot(&local.skills), before, "the local library changed");
    assert!(backups_beside(&local.skills).is_empty());
}

/// The same folder on both sides under different ids, the remote's folder
/// absent (excluded from its backup): the remote's record adopts the local
/// folder, and the local record and its fork files do not come across.
#[test]
fn a_folder_the_clone_has_a_record_for_keeps_only_that_record() {
    let env = setup();
    env.remote_holds(&[
        ("other/SKILL.md", skill_md("other").into_bytes()),
        (".skills-manager/schema.json", SCHEMA.to_vec()),
        (".skills-manager/skills/other-a.json", skill_meta("other-a", "other")),
        // Recorded on the remote, its folder never backed up.
        (".skills-manager/skills/solo-a.json", skill_meta("solo-a", "solo")),
    ]);
    let local = env.local();
    local.add_skill("solo-d", "solo", Some("https://example.invalid/solo.git"));
    local.write_metadata();
    fork_files::write(ForkConcern::Baseline, "solo-d", &json!({ "rev": "x" })).unwrap();

    local.connect_by_clone(env.remote_url()).unwrap();

    assert_eq!(local.ids_at("solo"), vec!["solo-a".to_string()]);
    assert!(local.store.get_skill_by_id("solo-d").unwrap().is_none());
    assert_eq!(local.fork_file(ForkConcern::Baseline, "solo-d"), None);
    assert_eq!(local.ids_at("other"), vec!["other-a".to_string()]);
}

/// A device that hit the bug backed its orphaned folder up: the remote has
/// the folder and no record. This device's record is the only one, so it
/// adopts the folder.
#[test]
fn a_folder_the_clone_has_without_a_record_is_adopted_by_the_local_record() {
    let env = setup();
    env.remote_holds(&[
        ("solo/SKILL.md", skill_md("solo").into_bytes()),
        ("other/SKILL.md", skill_md("other").into_bytes()),
        (".skills-manager/schema.json", SCHEMA.to_vec()),
        (".skills-manager/skills/other-a.json", skill_meta("other-a", "other")),
    ]);
    let local = env.local();
    local.add_skill("solo-d", "solo", Some("https://example.invalid/solo.git"));
    local.write_metadata();
    fork_files::write(ForkConcern::Notes, "solo-d", &json!({ "notes": ["mine"] })).unwrap();
    let notes = local.fork_file(ForkConcern::Notes, "solo-d");

    local.connect_by_clone(env.remote_url()).unwrap();

    assert_eq!(local.ids_at("solo"), vec!["solo-d".to_string()]);
    assert_eq!(local.fork_file(ForkConcern::Notes, "solo-d"), notes);
}

/// A remote whose `.skills-manager/` holds no skill index (no
/// `schema.json`): the app rebuilds the index from the database, which
/// keeps the local records, so only their fork files need to come across.
/// No index is created, so the remote's unrecorded folders are still
/// adopted by that rebuild.
#[test]
fn a_clone_without_a_skill_index_gets_the_fork_files_of_the_local_records() {
    let env = setup();
    env.remote_holds(&[
        ("legacy/SKILL.md", skill_md("legacy").into_bytes()),
        ("shared/SKILL.md", skill_md("shared").into_bytes()),
        (".skills-manager/protocol.json", b"{}\n".to_vec()),
    ]);
    let local = env.local();
    local.add_skill("shared-d", "shared", None);
    local.add_skill("solo-d", "solo", Some("https://example.invalid/solo.git"));
    local.write_metadata();
    for concern in ADAPTED {
        fork_files::write(concern, "solo-d", &json!({ "concern": concern.stem() })).unwrap();
    }
    let fork_before: Vec<_> = ADAPTED.iter().map(|c| local.fork_file(*c, "solo-d")).collect();

    git_backup::clone_into_unlocked(&local.skills, env.remote_url(), &[]).unwrap();
    assert!(
        !local.skills.join(".skills-manager/schema.json").exists(),
        "no index may be created: it would hide the remote's unrecorded folders"
    );
    crate::commands::git_backup::reconcile_skills_index_unlocked(&local.store).unwrap();

    assert!(local.store.get_skill_by_id("solo-d").unwrap().is_some());
    let after: Vec<_> = ADAPTED.iter().map(|c| local.fork_file(*c, "solo-d")).collect();
    assert_eq!(after, fork_before);
    assert_eq!(local.ids_at("shared"), vec!["shared-d".to_string()]);
    assert_eq!(local.ids_at("legacy").len(), 1, "the remote's folder is adopted");
}

/// A re-clone (the local `.git` set aside) goes through the same step.
#[test]
fn a_reclone_carries_local_only_records_too() {
    let env = setup();
    env.remote_holds(&[
        ("shared/SKILL.md", skill_md("shared").into_bytes()),
        (".skills-manager/schema.json", SCHEMA.to_vec()),
        (".skills-manager/skills/shared-a.json", skill_meta("shared-a", "shared")),
    ]);
    let local = env.local();
    local.add_skill("solo-d", "solo", None);
    local.write_metadata();
    fork_files::write(ForkConcern::Watch, "solo-d", &json!({ "watch": true })).unwrap();
    git(&local.skills, &["init", "-b", "main"]);

    let outcome =
        git_backup::reclone_from_remote_unlocked(&local.skills, env.remote_url(), &[]).unwrap();
    if let Some(kept) = outcome.kept_git {
        let _ = std::fs::remove_dir_all(kept);
    }
    crate::commands::git_backup::reconcile_skills_index_unlocked(&local.store).unwrap();
    sync_metadata::write_all_from_db_unlocked(&local.store).unwrap();

    assert_eq!(local.ids_at("solo"), vec!["solo-d".to_string()]);
    assert!(local.fork_file(ForkConcern::Watch, "solo-d").is_some());
    assert_eq!(local.ids_at("shared"), vec!["shared-a".to_string()]);
}
