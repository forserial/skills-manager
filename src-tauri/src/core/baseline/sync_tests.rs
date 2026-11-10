//! Baselines across devices (sm-69q.2, design §11.3): a backup clone reads
//! the baseline from its file; after a library-sync conflict is resolved
//! either way the baseline describes the content that was kept (Rule B); and
//! what an existing library keeps when it first connects to a remote.
//!
//! Two scratch libraries on one machine stand in for two devices, with a
//! local bare repository as the backup remote. "Updating" a skill on a
//! device is simulated by writing the new upstream content into its library
//! copy and recording a baseline for it, which is what an update leaves
//! behind; the real update command is covered in `tests.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{load, record, Baseline, Origin};
use crate::core::merge::resolve::{resolve_conflict_unlocked, ResolveAction};
use crate::core::skill_store::SkillStore;
use crate::core::sync_metadata::{self, SkillMetaFile, SourceMeta};
use crate::core::{central_repo, git_backup, merge};

const REV1: &str = "1111111111111111111111111111111111111111";
const REV2: &str = "2222222222222222222222222222222222222222";
const ID: &str = "skill-1";

struct Device {
    base: PathBuf,
    skills: PathBuf,
    store: SkillStore,
}

struct Env {
    _guard: std::sync::MutexGuard<'static, ()>,
    tmp: tempfile::TempDir,
    remote: PathBuf,
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
        .args([
            "-c",
            "user.email=test@example.com",
            "-c",
            "user.name=Raw Git",
        ])
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
    assert!(Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(&remote)
        .output()
        .unwrap()
        .status
        .success());
    Env {
        _guard: guard,
        tmp,
        remote,
    }
}

impl Env {
    fn device(&self, name: &str) -> Device {
        let base = self.tmp.path().join(name);
        let skills = base.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        let store = SkillStore::new(&base.join("db.sqlite")).unwrap();
        Device {
            base,
            skills,
            store,
        }
    }

    fn device_a(&self) -> Device {
        let dev = self.device("a");
        dev.activate();
        git(&dev.skills, &["init", "-b", "main"]);
        git_backup::configure_device_identity(&dev.skills, "Device A").unwrap();
        git(
            &dev.skills,
            &["remote", "add", "origin", self.remote.to_str().unwrap()],
        );
        dev
    }

    /// Restored from the backup through the app's own clone.
    fn device_by_clone(&self, name: &str, label: &str) -> Device {
        let base = self.tmp.path().join(name);
        std::fs::create_dir_all(&base).unwrap();
        let skills = base.join("skills");
        let store = SkillStore::new(&base.join("db.sqlite")).unwrap();
        let dev = Device {
            base,
            skills,
            store,
        };
        dev.activate();
        git_backup::clone_into_strict(&dev.skills, self.remote.to_str().unwrap()).unwrap();
        git_backup::configure_device_identity(&dev.skills, label).unwrap();
        dev.reindex();
        dev
    }
}

impl Device {
    fn activate(&self) {
        central_repo::set_test_base_dir_override(Some(self.base.clone()));
    }

    fn write_skill(&self, id: &str, path: &str, content: &str) {
        let dir = self.skills.join(path);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), content).unwrap();
        let meta = SkillMetaFile {
            schema_version: 1,
            skill_id: id.to_string(),
            path: path.to_string(),
            path_key: sync_metadata::path_key(path),
            enabled: true,
            tags: Vec::new(),
            source: SourceMeta {
                source_type: "git".to_string(),
                ref_: Some("https://example.invalid/up.git".to_string()),
                subpath: None,
                branch: None,
            },
        };
        let meta_dir = self.skills.join(".skills-manager/skills");
        std::fs::create_dir_all(&meta_dir).unwrap();
        std::fs::write(
            meta_dir.join(format!("{id}.json")),
            sync_metadata::canonical_json_bytes(&meta).unwrap(),
        )
        .unwrap();
        std::fs::write(
            self.skills.join(".skills-manager/schema.json"),
            b"{\n  \"schema_version\": 1,\n  \"app_min_version\": \"2.0.0\",\n  \"created_by\": \"skills-manager\"\n}\n",
        )
        .unwrap();
    }

    /// Upstream content lands in the library and its baseline is recorded,
    /// as an install or update leaves it.
    fn take_upstream(&self, path: &str, content: &str, revision: &str, origin: Origin) {
        self.activate();
        self.write_skill(ID, path, content);
        record(ID, revision, origin, &self.skills.join(path)).unwrap();
    }

    fn baseline(&self, id: &str) -> Option<Baseline> {
        self.activate();
        load(id).unwrap()
    }

    fn revision(&self, id: &str) -> Option<String> {
        self.baseline(id).map(|b| b.revision)
    }

    fn reindex(&self) {
        self.activate();
        sync_metadata::reindex_from_metadata(&self.store).unwrap();
    }

    fn commit(&self, message: &str) {
        self.reindex();
        if git_backup::has_uncommitted_changes(&self.skills).unwrap() {
            git_backup::commit_all(&self.skills, message).unwrap();
        }
    }

    fn push(&self) {
        git(&self.skills, &["push", "-u", "origin", "main"]);
    }

    fn pull(&self) -> merge::apply::MergeSummary {
        self.activate();
        let summary = merge::gated_pull_unlocked(&self.store, &self.skills).unwrap();
        sync_metadata::reindex_from_metadata(&self.store).unwrap();
        summary
    }

    fn resolve(&self, action: ResolveAction) {
        self.activate();
        resolve_conflict_unlocked(&self.store, &self.skills, ID, action).unwrap();
        sync_metadata::reindex_from_metadata(&self.store).unwrap();
        sync_metadata::write_all_from_db(&self.store).unwrap();
    }

    fn skill_md(&self, path: &str) -> String {
        std::fs::read_to_string(self.skills.join(path).join("SKILL.md")).unwrap()
    }

    /// The baseline must describe the library copy: for a skill nobody
    /// edited locally, exactly; otherwise it is the upstream manifest that
    /// local edits are measured against.
    fn manifest_matches(&self, id: &str, path: &str) -> bool {
        let baseline = self.baseline(id).unwrap();
        super::differing_paths(
            &baseline.files,
            &super::manifest_of(&self.skills.join(path)).unwrap(),
        )
        .is_empty()
    }

    fn status(&self) -> String {
        git(&self.skills, &["status", "--porcelain"])
    }
}

const BASE: &str = "---\nname: alpha\n---\nupstream v1\n";
const UPDATED: &str = "---\nname: alpha\n---\nupstream v2\n";
const EDITED: &str = "---\nname: alpha\n---\nupstream v1 plus a local edit\n";

/// A installs the skill at REV1 and backs it up; B clones the backup.
fn seeded(env: &Env) -> (Device, Device) {
    let a = env.device_a();
    a.take_upstream("alpha", BASE, REV1, Origin::Install);
    a.commit("install");
    a.push();
    let b = env.device_by_clone("b", "Device B");
    (a, b)
}

#[test]
fn a_backup_clone_reads_the_baseline_and_its_revision_from_the_file() {
    let env = setup();
    let (_a, b) = seeded(&env);
    // The clone's database has no source revision: SkillMetaFile carries none.
    let row = b.store.get_skill_by_id(ID).unwrap().unwrap();
    assert_eq!(row.source_revision, None);
    let baseline = b.baseline(ID).expect("baseline travels with the backup");
    assert_eq!(baseline.revision, REV1);
    assert!(b.manifest_matches(ID, "alpha"));
    // The base store does not travel: the tree id is a hint to fetch.
    assert!(!super::base_store_has_tree(
        baseline.tree_id.as_deref().unwrap()
    ));
}

/// Device A edits the skill while device B updates it; A pulls. The content
/// conflicts and A keeps its own copy, but only B changed the baseline file,
/// so before resolving A holds B's baseline against A's content (§11.3).
fn a_edits_b_updates_a_pulls(env: &Env) -> (Device, Device) {
    let (a, b) = seeded(env);
    b.take_upstream("alpha", UPDATED, REV2, Origin::Update);
    b.commit("update");
    b.push();

    a.activate();
    a.write_skill(ID, "alpha", EDITED);
    a.commit("local edit");
    let summary = a.pull();
    assert_eq!(summary.new_conflicts, vec![ID], "{summary:?}");
    assert_eq!(a.skill_md("alpha"), EDITED);
    assert_eq!(
        a.revision(ID).as_deref(),
        Some(REV2),
        "the mismatch Rule B fixes"
    );
    (a, b)
}

/// The mirror: A edits and pushes; B updates and pulls.
fn b_updates_a_edits_b_pulls(env: &Env) -> (Device, Device) {
    let (a, b) = seeded(env);
    a.activate();
    a.write_skill(ID, "alpha", EDITED);
    a.commit("local edit");
    a.push();

    b.take_upstream("alpha", UPDATED, REV2, Origin::Update);
    b.commit("update");
    let summary = b.pull();
    assert_eq!(summary.new_conflicts, vec![ID], "{summary:?}");
    assert_eq!(b.skill_md("alpha"), UPDATED);
    (a, b)
}

#[test]
fn keep_local_restores_the_local_sides_baseline() {
    {
        let env = setup();
        let (a, _b) = a_edits_b_updates_a_pulls(&env);
        a.resolve(ResolveAction::KeepLocal);
        assert_eq!(a.skill_md("alpha"), EDITED);
        assert_eq!(a.revision(ID).as_deref(), Some(REV1));
        assert_eq!(
            a.status(),
            "",
            "the restored baseline is part of the resolution commit"
        );
    }
    let env = setup();
    let (_a, b) = b_updates_a_edits_b_pulls(&env);
    b.resolve(ResolveAction::KeepLocal);
    assert_eq!(b.skill_md("alpha"), UPDATED);
    assert_eq!(b.revision(ID).as_deref(), Some(REV2));
    assert!(b.manifest_matches(ID, "alpha"));
    assert_eq!(b.status(), "");
}

#[test]
fn use_remote_takes_the_remote_sides_baseline() {
    {
        let env = setup();
        let (a, _b) = a_edits_b_updates_a_pulls(&env);
        a.resolve(ResolveAction::UseRemote);
        assert_eq!(a.skill_md("alpha"), UPDATED);
        assert_eq!(a.revision(ID).as_deref(), Some(REV2));
        assert!(a.manifest_matches(ID, "alpha"));
        assert_eq!(a.status(), "");
    }
    let env = setup();
    let (_a, b) = b_updates_a_edits_b_pulls(&env);
    b.resolve(ResolveAction::UseRemote);
    assert_eq!(b.skill_md("alpha"), EDITED);
    assert_eq!(b.revision(ID).as_deref(), Some(REV1));
    assert_eq!(b.status(), "");
}

#[test]
fn keep_both_gives_each_copy_its_own_sides_baseline() {
    let env = setup();
    let (a, _b) = a_edits_b_updates_a_pulls(&env);
    a.resolve(ResolveAction::KeepBoth);
    assert_eq!(a.revision(ID).as_deref(), Some(REV1));
    let copies: Vec<_> = a
        .store
        .get_all_skills()
        .unwrap()
        .into_iter()
        .filter(|s| s.id != ID)
        .collect();
    assert_eq!(copies.len(), 1);
    let copy = &copies[0];
    let baseline = a
        .baseline(&copy.id)
        .expect("the copy keeps the remote baseline");
    assert_eq!(baseline.revision, REV2);
    assert_eq!(baseline.skill_id, copy.id);
    let copy_dir = Path::new(&copy.central_path)
        .strip_prefix(&a.skills)
        .unwrap()
        .to_string_lossy()
        .to_string();
    assert!(a.manifest_matches(&copy.id, &copy_dir));
    assert_eq!(a.status(), "");
}

#[test]
fn keep_local_without_a_local_baseline_leaves_it_unknown() {
    // A's skill predates baselines; B updates and records one.
    let env = setup();
    let a = env.device_a();
    a.write_skill(ID, "alpha", BASE);
    a.commit("install");
    a.push();
    let b = env.device_by_clone("b", "Device B");
    b.take_upstream("alpha", UPDATED, REV2, Origin::Update);
    b.commit("update");
    b.push();
    a.activate();
    a.write_skill(ID, "alpha", EDITED);
    a.commit("local edit");
    a.pull();
    assert_eq!(a.revision(ID).as_deref(), Some(REV2));
    a.resolve(ResolveAction::KeepLocal);
    assert_eq!(a.baseline(ID), None);
    assert_eq!(a.status(), "");
}

/// When an existing library first connects to a remote by clone, the
/// clone's `.skills-manager/` stays and what only the local library has is
/// carried in file by file (sm-69q.15, `clone_metadata_carry`): a local-only
/// skill keeps its record and its baseline. Until sm-69q.15 the baseline was
/// dropped (and the record with it).
#[test]
fn first_connect_by_clone_keeps_local_only_baselines() {
    let env = setup();
    let (_a, _b) = seeded(&env);

    let c = env.device("c");
    c.activate();
    c.write_skill("skill-c", "gamma", BASE);
    record("skill-c", REV1, Origin::Install, &c.skills.join("gamma")).unwrap();
    c.reindex();
    assert_eq!(c.revision("skill-c").as_deref(), Some(REV1));

    {
        let _lock = crate::core::repo_lock::RepoLock::acquire_foreground("test").unwrap();
        git_backup::clone_into_unlocked(&c.skills, env.remote.to_str().unwrap(), &[]).unwrap();
    }
    // The local-only skill keeps its folder and its baseline.
    assert!(c.skills.join("gamma/SKILL.md").is_file());
    assert_eq!(c.revision("skill-c").as_deref(), Some(REV1));
    // The remote's baselines arrive with its metadata.
    assert_eq!(c.revision(ID).as_deref(), Some(REV1));
}
