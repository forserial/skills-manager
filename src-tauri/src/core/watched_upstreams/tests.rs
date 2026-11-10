use super::*;
use crate::core::skill_store::{SkillRecord, SkillTargetRecord};
use std::fs;
use std::sync::MutexGuard;
use tempfile::{TempDir, tempdir};

struct TestRepo {
    _lock: MutexGuard<'static, ()>,
    tmp: TempDir,
    store: SkillStore,
}

impl Drop for TestRepo {
    fn drop(&mut self) {
        central_repo::set_test_base_dir_override(None);
    }
}

fn test_repo() -> TestRepo {
    let lock = central_repo::test_base_dir_lock();
    let tmp = tempdir().unwrap();
    let base = tmp.path().join("repo");
    central_repo::set_test_base_dir_override(Some(base.clone()));
    fs::create_dir_all(central_repo::skills_dir()).unwrap();
    let store = SkillStore::new(&base.join("test.db")).unwrap();
    TestRepo {
        _lock: lock,
        tmp,
        store,
    }
}

fn record(id: &str, name: &str, source_type: &str) -> SkillRecord {
    let dir = central_repo::skills_dir().join(name);
    let now = chrono::Utc::now().timestamp_millis();
    SkillRecord {
        id: id.to_string(),
        name: name.to_string(),
        description: None,
        source_type: source_type.to_string(),
        source_ref: None,
        source_ref_resolved: None,
        source_subpath: None,
        source_branch: None,
        source_revision: None,
        remote_revision: None,
        central_path: dir.to_string_lossy().to_string(),
        content_hash: None,
        enabled: true,
        created_at: now,
        updated_at: now,
        status: "ok".to_string(),
        update_status: "local_only".to_string(),
        last_checked_at: None,
        last_check_error: None,
    }
}

/// A skill in the library, as a local-folder install leaves it.
fn insert_local_skill(store: &SkillStore, id: &str, name: &str) -> PathBuf {
    let mut rec = record(id, name, "local");
    let dir = PathBuf::from(&rec.central_path);
    fs::create_dir_all(dir.join("scripts")).unwrap();
    fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {name}\ndescription: d\n---\nbody\r\n"),
    )
    .unwrap();
    fs::write(dir.join("scripts/run.sh"), "echo hi\n").unwrap();
    rec.source_ref = Some("C:/work/somewhere".to_string());
    rec.content_hash = Some(crate::core::content_hash::hash_directory(&dir).unwrap());
    store.insert_skill(&rec).unwrap();
    dir
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .expect("git must be runnable");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn file_url(path: &Path) -> String {
    let raw = path.display().to_string().replace('\\', "/");
    if raw.starts_with('/') {
        format!("file://{raw}")
    } else {
        format!("file:///{raw}")
    }
}

/// A local bare repository fed from a work tree. Holds `skills/a/SKILL.md`
/// and `README.md`.
struct Upstream {
    work: PathBuf,
    url: String,
}

impl Upstream {
    fn new(root: &Path) -> Self {
        let bare = root.join("upstream.git");
        let work = root.join("upstream-work");
        fs::create_dir_all(&bare).unwrap();
        git_in(&bare, &["init", "-q", "--bare", "--initial-branch=main"]);
        // The blobless fetch behind the subpath comparison is served by it.
        git_in(&bare, &["config", "uploadpack.allowFilter", "true"]);
        fs::create_dir_all(work.join("skills/a")).unwrap();
        git_in(&work, &["init", "-q", "--initial-branch=main"]);
        fs::write(work.join("skills/a/SKILL.md"), "---\nname: a\n---\n").unwrap();
        fs::write(work.join("README.md"), "readme\n").unwrap();
        git_in(&work, &["add", "-A"]);
        git_in(&work, &["commit", "-qm", "init"]);
        git_in(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
        git_in(&work, &["push", "-q", "origin", "main"]);
        Upstream {
            work,
            url: file_url(&bare),
        }
    }

    fn head(&self) -> String {
        git_in(&self.work, &["rev-parse", "HEAD"])
    }

    fn commit(&self, rel: &str, body: &str) -> String {
        fs::write(self.work.join(rel), body).unwrap();
        git_in(&self.work, &["add", rel]);
        git_in(&self.work, &["commit", "-qm", rel]);
        git_in(&self.work, &["push", "-q", "origin", "main"]);
        self.head()
    }
}

/// Every file under `dir` with its bytes, to prove nothing was written there.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<_> = walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            (
                e.path()
                    .strip_prefix(dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/"),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect();
    out.sort();
    out
}

fn new_watch(url: &str) -> NewWatch {
    NewWatch {
        git_url: url.to_string(),
        ..Default::default()
    }
}

#[test]
fn watch_on_a_local_skill_reports_moved_after_a_new_upstream_commit_and_seen_clears_it() {
    let repo = test_repo();
    let dir = insert_local_skill(&repo.store, "s1", "mine");
    let before = snapshot(&dir);
    let upstream = Upstream::new(repo.tmp.path());
    let first = upstream.head();

    let added = add(
        &repo.store,
        "s1",
        NewWatch {
            note: Some("lifted the retry loop".to_string()),
            ..new_watch(&upstream.url)
        },
        None,
    )
    .unwrap();
    // No --revision: the current head is what was seen.
    assert_eq!(added.last_seen_revision.as_deref(), Some(first.as_str()));
    assert_eq!(added.note.as_deref(), Some("lifted the retry loop"));
    assert_eq!(added.id.len(), 8);

    let checked = check("s1", None).unwrap();
    assert_eq!(checked.len(), 1);
    assert!(!checked[0].moved);
    assert_eq!(checked[0].remote_revision.as_deref(), Some(first.as_str()));
    assert!(checked[0].checked_at.is_some());

    let second = upstream.commit("README.md", "changed\n");
    let checked = check("s1", None).unwrap();
    assert!(checked[0].moved, "a new commit upstream is movement");
    assert_eq!(checked[0].remote_revision.as_deref(), Some(second.as_str()));
    // Stored, so a plain list (no network) sees it too.
    assert!(any_moved(&list("s1").unwrap()));

    let seen = mark_seen(&repo.store, "s1", &added.id, None).unwrap();
    assert!(!seen.moved);
    assert_eq!(seen.last_seen_revision.as_deref(), Some(second.as_str()));
    assert!(!check("s1", None).unwrap()[0].moved, "seen clears moved");

    // The skill directory was never written.
    assert_eq!(snapshot(&dir), before);
    // The watch lives in the fork-file store.
    assert!(
        fork_files::path_for(ForkConcern::Watch, "s1")
            .unwrap()
            .is_file()
    );
}

#[test]
fn a_subpath_watch_ignores_commits_elsewhere_in_the_repository() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "s1", "mine");
    let upstream = Upstream::new(repo.tmp.path());
    add(
        &repo.store,
        "s1",
        NewWatch {
            subpath: Some("skills/a".to_string()),
            ..new_watch(&upstream.url)
        },
        None,
    )
    .unwrap();

    upstream.commit("README.md", "elsewhere\n");
    let checked = check("s1", None).unwrap();
    assert!(!checked[0].moved, "{:?}", checked[0]);

    upstream.commit("skills/a/SKILL.md", "---\nname: a\n---\nnew\n");
    assert!(check("s1", None).unwrap()[0].moved);
}

#[test]
fn an_unreachable_upstream_is_recorded_on_the_entry_not_returned() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "s1", "mine");
    let gone = repo.tmp.path().join("nowhere.git");
    add(
        &repo.store,
        "s1",
        NewWatch {
            revision: Some("a".repeat(40)),
            ..new_watch(&file_url(&gone))
        },
        None,
    )
    .unwrap();
    let checked = check("s1", None).unwrap();
    assert!(checked[0].check_error.is_some());
    assert!(!checked[0].moved);
}

#[test]
fn add_validates_and_refuses_duplicates_and_remove_drops_the_file_when_empty() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "s1", "mine");
    let rev = Some("b".repeat(40));
    let w = add(
        &repo.store,
        "s1",
        NewWatch {
            revision: rev.clone(),
            ..new_watch("https://github.com/owner/repo/tree/dev/skills/x")
        },
        None,
    )
    .unwrap();
    // A tree URL gives branch and subpath.
    assert_eq!(w.git_url, "https://github.com/owner/repo.git");
    assert_eq!(w.branch.as_deref(), Some("dev"));
    assert_eq!(w.subpath.as_deref(), Some("skills/x"));

    let dup = add(
        &repo.store,
        "s1",
        NewWatch {
            revision: rev.clone(),
            branch: Some("dev".to_string()),
            subpath: Some("skills/x".to_string()),
            ..new_watch("https://github.com/owner/repo.git")
        },
        None,
    );
    assert!(dup.is_err());

    for bad in ["", "-upload-pack=x", "ext::sh -c x", "not a url"] {
        assert!(
            add(
                &repo.store,
                "s1",
                NewWatch {
                    revision: rev.clone(),
                    ..new_watch(bad)
                },
                None
            )
            .is_err(),
            "{bad:?}"
        );
    }
    assert!(
        add(
            &repo.store,
            "s1",
            NewWatch {
                revision: Some("zz".to_string()),
                ..new_watch("owner/other")
            },
            None
        )
        .is_err()
    );
    assert!(add(&repo.store, "missing", new_watch("owner/other"), None).is_err());

    assert!(remove(&repo.store, "s1", "nope").is_err());
    remove(&repo.store, "s1", &w.id).unwrap();
    assert!(list("s1").unwrap().is_empty());
    assert!(
        !fork_files::path_for(ForkConcern::Watch, "s1")
            .unwrap()
            .exists()
    );
}

#[test]
fn unknown_fields_from_a_newer_build_survive_a_rewrite() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "s1", "mine");
    let path = fork_files::path_for(ForkConcern::Watch, "s1").unwrap();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"version":2,"future":true,"upstreams":[{"id":"abcd1234","git_url":"https://github.com/o/r.git","added_at":1,"last_seen_revision":null,"hint":"x"}]}"#,
    )
    .unwrap();
    add(
        &repo.store,
        "s1",
        NewWatch {
            revision: Some("c".repeat(40)),
            ..new_watch("o/other")
        },
        None,
    )
    .unwrap();
    let raw: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(raw["future"], Value::Bool(true));
    assert_eq!(raw["upstreams"][0]["hint"], Value::String("x".to_string()));
    assert_eq!(raw["upstreams"].as_array().unwrap().len(), 2);
}

#[test]
fn fork_copies_content_to_a_new_local_skill_watching_the_original() {
    let repo = test_repo();
    let upstream = Upstream::new(repo.tmp.path());
    let revision = upstream.head();
    // The original: a git skill, deployed to an agent.
    let mut original = record("orig", "borrowed", "git");
    let original_dir = PathBuf::from(&original.central_path);
    fs::create_dir_all(original_dir.join("refs")).unwrap();
    fs::write(
        original_dir.join("SKILL.md"),
        "---\nname: borrowed\n---\nhi\r\n",
    )
    .unwrap();
    fs::write(original_dir.join("refs/notes.md"), "notes\n").unwrap();
    original.source_ref = Some(upstream.url.clone());
    // A git install records the URL it cloned from.
    original.source_ref_resolved = Some(upstream.url.clone());
    original.source_subpath = Some("skills/a".to_string());
    original.source_branch = Some("main".to_string());
    original.source_revision = Some(revision.clone());
    original.update_status = "up_to_date".to_string();
    original.content_hash = Some(crate::core::content_hash::hash_directory(&original_dir).unwrap());
    repo.store.insert_skill(&original).unwrap();
    repo.store
        .insert_target(&SkillTargetRecord {
            id: "t1".to_string(),
            skill_id: "orig".to_string(),
            tool: "claude_code".to_string(),
            target_path: repo
                .tmp
                .path()
                .join("agent/borrowed")
                .to_string_lossy()
                .to_string(),
            mode: "copy".to_string(),
            status: "ok".to_string(),
            synced_at: None,
            last_error: None,
            source_hash: None,
        })
        .unwrap();
    let row_json =
        |id: &str| serde_json::to_value(repo.store.get_skill_by_id(id).unwrap().unwrap()).unwrap();
    let original_before = row_json("orig");
    let original_files = snapshot(&original_dir);

    let forked = fork_skill(&repo.store, "orig", "my-version").unwrap();

    assert_ne!(forked.skill_id, "orig");
    assert_eq!(forked.name, "my-version");
    assert_eq!(forked.forked_from, "orig");
    let fork_dir = PathBuf::from(&forked.central_path);
    assert_eq!(fork_dir, central_repo::skills_dir().join("my-version"));
    // Identical content, except the frontmatter name the reindex reads.
    let mut expected = original_files.clone();
    for (path, bytes) in &mut expected {
        if path == "SKILL.md" {
            *bytes = b"---\nname: my-version\n---\nhi\r\n".to_vec();
        }
    }
    assert_eq!(snapshot(&fork_dir), expected);
    assert_eq!(
        skill_metadata::parse_skill_md(&fork_dir).name.as_deref(),
        Some("my-version")
    );

    let row = repo
        .store
        .get_skill_by_id(&forked.skill_id)
        .unwrap()
        .unwrap();
    assert_eq!(row.source_type, "local");
    assert_eq!(row.source_ref, None);
    assert_eq!(row.update_status, "local_only");
    assert!(
        repo.store
            .get_targets_for_skill(&forked.skill_id)
            .unwrap()
            .is_empty()
    );

    let watches = list(&forked.skill_id).unwrap();
    assert_eq!(watches.len(), 1);
    assert_eq!(watches[0].git_url, upstream.url);
    assert_eq!(watches[0].subpath.as_deref(), Some("skills/a"));
    assert_eq!(watches[0].branch.as_deref(), Some("main"));
    assert_eq!(
        watches[0].last_seen_revision.as_deref(),
        Some(revision.as_str())
    );
    assert_eq!(forked.watched.as_ref(), Some(&watches[0]));
    assert!(!check(&forked.skill_id, None).unwrap()[0].moved);

    // The original is as it was.
    assert_eq!(row_json("orig"), original_before);
    assert_eq!(snapshot(&original_dir), original_files);
    assert_eq!(repo.store.get_targets_for_skill("orig").unwrap().len(), 1);
    assert!(list("orig").unwrap().is_empty());

    // The name is taken now, by skill name or by directory.
    assert!(fork_skill(&repo.store, "orig", "my-version").is_err());
    assert!(fork_skill(&repo.store, "orig", "borrowed").is_err());
    fs::create_dir_all(central_repo::skills_dir().join("stray")).unwrap();
    assert!(fork_skill(&repo.store, "orig", "stray").is_err());
    assert!(fork_skill(&repo.store, "orig", "../escape").is_err());
    assert!(fork_skill(&repo.store, "missing", "x").is_err());
}

#[test]
fn fork_prefers_the_baseline_revision_and_a_local_original_gets_no_watch() {
    let repo = test_repo();
    let mut original = record("orig", "borrowed", "git");
    let dir = PathBuf::from(&original.central_path);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("SKILL.md"), "---\nname: borrowed\n---\n").unwrap();
    original.source_ref = Some("https://github.com/o/r.git".to_string());
    original.source_revision = None; // a device that got the library by clone
    repo.store.insert_skill(&original).unwrap();
    fork_files::write(
        ForkConcern::Baseline,
        "orig",
        &serde_json::json!({ "skill_id": "orig", "revision": "d".repeat(40) }),
    )
    .unwrap();

    let forked = fork_skill(&repo.store, "orig", "copy-one").unwrap();
    assert_eq!(
        forked.watched.unwrap().last_seen_revision,
        Some("d".repeat(40))
    );

    insert_local_skill(&repo.store, "loc", "plain");
    let forked = fork_skill(&repo.store, "loc", "plain-copy").unwrap();
    assert!(forked.watched.is_none());
    assert!(list(&forked.skill_id).unwrap().is_empty());
}

#[test]
fn removing_a_skill_removes_its_watched_upstreams() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "gone", "gone");
    insert_local_skill(&repo.store, "kept", "kept");
    for id in ["gone", "kept"] {
        add(
            &repo.store,
            id,
            NewWatch {
                revision: Some("e".repeat(40)),
                ..new_watch("owner/repo")
            },
            None,
        )
        .unwrap();
    }
    crate::commands::skills::delete_managed_skills_by_ids(&repo.store, &["gone".to_string()])
        .unwrap();
    assert!(
        !fork_files::path_for(ForkConcern::Watch, "gone")
            .unwrap()
            .exists()
    );
    assert!(list("gone").unwrap().is_empty());
    assert_eq!(list("kept").unwrap().len(), 1);
}

#[test]
fn frontmatter_rename_changes_only_the_name_line() {
    let crlf = "---\r\nname: orig\r\ndescription: d\r\n---\r\nname: body text\r\n";
    assert_eq!(
        frontmatter_with_name(crlf, "mine").unwrap(),
        "---\r\nname: mine\r\ndescription: d\r\n---\r\nname: body text\r\n"
    );
    // Leading blank lines, as the parser allows; a name needing quotes.
    assert_eq!(
        frontmatter_with_name("\n---\nname: a\n---\n", "my skill").unwrap(),
        "\n---\nname: \"my skill\"\n---\n"
    );
    // Nested `name:` keys are not the skill's name.
    assert_eq!(
        frontmatter_with_name("---\nmeta:\n  name: x\nname: a\n---\n", "b").unwrap(),
        "---\nmeta:\n  name: x\nname: b\n---\n"
    );
    assert_eq!(
        frontmatter_with_name("no frontmatter\nname: a\n", "b"),
        None
    );
    assert_eq!(
        frontmatter_with_name("---\ndescription: d\n---\n", "b"),
        None
    );
    assert_eq!(
        frontmatter_with_name("---\nname: a\n", "b"),
        None,
        "unclosed"
    );
}
