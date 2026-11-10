//! Storage contract for fork files (sm-69q.12, from the spike in epic design
//! §11.1): a flat `.skills-manager/fork-<concern>.<id>.json` must survive a
//! startup reindex, `write_all_from_db`, a backup commit and clone, and a
//! three-way object-merge pull between two libraries with divergent commits,
//! where a file changed on both sides goes to the newest commit.
//!
//! The fork's per-skill state rests on all of this being upstream behaviour
//! we don't control. If an upstream merge changes any of it, this test must
//! fail rather than let fork state get dropped silently on sync.
//!
//! Harness modelled on `core/merge/integration_tests.rs`: every step owns the
//! global central-repo override and switches it to the device it acts as.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use super::{ForkConcern, path_for, read, write};
use crate::core::skill_store::SkillStore;
use crate::core::sync_metadata::{self, SkillMetaFile, SourceMeta};
use crate::core::{central_repo, git_backup, merge};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct State {
    by: String,
}

fn state(by: &str) -> State {
    State { by: by.to_string() }
}

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

fn git_env(dir: &Path, args: &[&str], env: &[(&str, &str)]) -> String {
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
        .envs(env.iter().copied())
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn git(dir: &Path, args: &[&str]) -> String {
    git_env(dir, args, &[])
}

fn setup() -> Env {
    let guard = central_repo::test_base_dir_lock();
    let tmp = tempfile::tempdir().unwrap();
    let remote = tmp.path().join("remote.git");
    assert!(
        Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .arg(&remote)
            .output()
            .unwrap()
            .status
            .success()
    );
    Env {
        _guard: guard,
        tmp,
        remote,
    }
}

impl Env {
    fn device_a(&self) -> Device {
        let base = self.tmp.path().join("a");
        let skills = base.join("skills");
        std::fs::create_dir_all(&skills).unwrap();
        let store = SkillStore::new(&base.join("db.sqlite")).unwrap();
        let dev = Device {
            base,
            skills,
            store,
        };
        dev.activate();
        git(&dev.skills, &["init", "-b", "main"]);
        git_backup::configure_device_identity(&dev.skills, "Device A").unwrap();
        git(
            &dev.skills,
            &["remote", "add", "origin", self.remote.to_str().unwrap()],
        );
        dev
    }

    /// Second device, restored from the backup through the app's own clone.
    fn device_b_by_clone(&self) -> Device {
        let base = self.tmp.path().join("b");
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
        git_backup::configure_device_identity(&dev.skills, "Device B").unwrap();
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
                source_type: "import".to_string(),
                ref_: None,
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

    fn put(&self, concern: ForkConcern, id: &str, by: &str) {
        self.activate();
        write(concern, id, &state(by)).unwrap();
    }

    fn get(&self, concern: ForkConcern, id: &str) -> Option<State> {
        self.activate();
        read(concern, id).unwrap()
    }

    fn reindex(&self) {
        self.activate();
        sync_metadata::reindex_from_metadata(&self.store).unwrap();
    }

    fn write_all(&self) {
        self.activate();
        sync_metadata::write_all_from_db(&self.store).unwrap();
    }

    /// Back up through the app's commit path, then pin the commit's time so
    /// newest-wins is decided by the test, not by the wall clock.
    fn commit_at(&self, message: &str, unix_seconds: i64) {
        self.activate();
        git_backup::commit_all(&self.skills, message).unwrap();
        let date = format!("@{unix_seconds} +0000");
        git_env(
            &self.skills,
            &[
                "commit",
                "--amend",
                "--no-edit",
                "--allow-empty",
                "--date",
                &date,
            ],
            &[("GIT_COMMITTER_DATE", &date)],
        );
    }

    fn pull(&self) -> merge::apply::MergeSummary {
        self.activate();
        let summary = merge::gated_pull_unlocked(&self.store, &self.skills).unwrap();
        // Mirror the command layer: reconcile the DB from merged metadata.
        sync_metadata::reindex_from_metadata(&self.store).unwrap();
        summary
    }

    fn tracked(&self) -> String {
        git(&self.skills, &["ls-tree", "-r", "HEAD", "--name-only"])
    }

    fn tree_oid(&self) -> String {
        git(&self.skills, &["rev-parse", "HEAD^{tree}"])
    }
}

#[test]
fn flat_fork_files_survive_reindex_rewrite_backup_clone_and_three_way_merge() {
    use ForkConcern::{Baseline, Notes, Override, Pin, Watch};

    let env = setup();
    let a = env.device_a();
    a.write_skill("skill-1", "alpha", "base content");
    a.reindex();
    a.put(Baseline, "skill-1", "seed");

    // ── local lifecycle: startup reindex and a full metadata rewrite ──
    a.reindex();
    assert_eq!(a.get(Baseline, "skill-1"), Some(state("seed")));
    a.write_all();
    assert_eq!(a.get(Baseline, "skill-1"), Some(state("seed")));
    a.activate();
    let flat = path_for(Baseline, "skill-1").unwrap();
    assert_eq!(flat.parent().unwrap(), a.skills.join(".skills-manager"));

    // ── backup commit carries it; clone restores it ──
    let now = chrono::Utc::now().timestamp();
    a.commit_at("seed", now);
    assert!(
        a.tracked()
            .contains(".skills-manager/fork-baseline.skill-1.json"),
        "{}",
        a.tracked()
    );
    git(&a.skills, &["push", "-u", "origin", "main"]);

    let b = env.device_b_by_clone();
    assert_eq!(b.get(Baseline, "skill-1"), Some(state("seed")));
    b.write_all();
    assert_eq!(b.get(Baseline, "skill-1"), Some(state("seed")));

    // ── divergent commits ──
    // A (older): changes the baseline, adds a pin and an override.
    a.put(Baseline, "skill-1", "A");
    a.put(Pin, "skill-1", "A");
    a.put(Override, "skill-1", "A");
    a.commit_at("A edits fork state", now + 100);
    git(&a.skills, &["push", "origin", "main"]);

    // B (newer): changes the same baseline, the same override, adds a watch.
    b.put(Baseline, "skill-1", "B");
    b.put(Override, "skill-1", "B");
    b.put(Watch, "skill-1", "B");
    b.commit_at("B edits fork state", now + 200);
    let b_tip = git(&b.skills, &["rev-parse", "HEAD"]);

    // B merges A: ours (B) is newer, so B's edits to shared files stay;
    // A's one-sided additions arrive.
    let summary_b = b.pull();
    assert!(!summary_b.up_to_date, "{summary_b:?}");
    assert!(summary_b.new_conflicts.is_empty(), "{summary_b:?}");
    let expect = |dev: &Device| {
        assert_eq!(dev.get(Baseline, "skill-1"), Some(state("B")));
        assert_eq!(dev.get(Override, "skill-1"), Some(state("B")));
        assert_eq!(dev.get(Pin, "skill-1"), Some(state("A")));
        assert_eq!(dev.get(Watch, "skill-1"), Some(state("B")));
        assert_eq!(dev.get(Notes, "skill-1"), None);
    };
    expect(&b);

    // Mirror: A merges B's pre-merge tip. Theirs (B) is newer, so this time
    // the incoming side wins the shared files.
    git(
        &b.skills,
        &["push", "-f", "origin", &format!("{b_tip}:refs/heads/main")],
    );
    let summary_a = a.pull();
    assert!(summary_a.new_conflicts.is_empty(), "{summary_a:?}");
    expect(&a);

    // Both viewpoints land on the same tree.
    assert_eq!(a.tree_oid(), b.tree_oid());

    // And the merged files still survive the post-pull lifecycle.
    for dev in [&a, &b] {
        dev.reindex();
        dev.write_all();
        expect(dev);
    }
}
