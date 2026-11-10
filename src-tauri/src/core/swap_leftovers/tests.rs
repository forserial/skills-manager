//! sm-69q.23 on scratch libraries. The crashes are real: a fail point panics
//! inside `swap_skill_directory` between its two renames, so nothing after it
//! runs (no error path, no restore), and the test looks at what is left.

use std::fs;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::*;
use crate::core::fail_points;
use crate::core::orphaned_folders::{self, FolderKind};
use crate::core::write_order_tests::{Library, library, write_skill};

fn later() -> SystemTime {
    SystemTime::now() + STALE_AFTER + Duration::from_secs(60)
}

fn hidden(lib: &Library, skill: &str, kind: &str) -> PathBuf {
    lib.root.join(format!(".{skill}.{kind}-{}", uuid::Uuid::new_v4()))
}

/// `keep`'s source, with content that differs from the library's.
fn new_version_of_keep(lib: &Library) -> PathBuf {
    let src = lib.tmp.path().join("sources/keep");
    write_skill(&src, "keep");
    fs::write(src.join("SKILL.md"), "---\nname: keep\ndescription: v2\n---\n\nNew body\n").unwrap();
    lib.store.update_skill_source_ref("keep", &src.to_string_lossy()).unwrap();
    src
}

fn skill_md(dir: &Path) -> String {
    fs::read_to_string(dir.join("SKILL.md")).unwrap()
}

fn starting_with(lib: &Library, prefix: &str) -> Vec<String> {
    lib.entries().into_iter().filter(|n| n.starts_with(prefix)).collect()
}

fn listed(lib: &Library) -> Vec<(String, FolderKind)> {
    orphaned_folders::find(&lib.store)
        .unwrap()
        .into_iter()
        .map(|f| (f.dir_name, f.kind))
        .collect()
}

/// The next start, as `app_state` runs it: this step, then the reindex.
fn next_start(lib: &Library) -> Vec<String> {
    recover_at_startup(&lib.store);
    lib.after_reindex()
}

#[test]
fn only_names_this_app_makes_are_leftovers() {
    let id = uuid::Uuid::new_v4();
    assert_eq!(parse_name(&format!(".keep.staged-{id}")), Some((LeftoverKind::Staged, "keep")));
    assert_eq!(parse_name(&format!(".my.skill.backup-{id}")), Some((LeftoverKind::Backup, "my.skill")));
    // The installer's own staging inside an update's staged folder.
    assert_eq!(
        parse_name(&format!("..keep.staged-{id}.staged-{id}")),
        Some((LeftoverKind::Staged, &*format!(".keep.staged-{id}")))
    );
    for name in [
        "keep".to_string(),
        ".git".to_string(),
        ".keep.staged-1234".to_string(),
        ".keep.backup-old".to_string(),
        format!("keep.staged-{id}"),
        format!("..staged-{id}"),
        format!(".keep.copy-{id}"),
    ] {
        assert_eq!(parse_name(&name), None, "{name}");
    }
}

// ── a crash between the swap's two renames ──

#[test]
fn an_update_that_crashed_between_the_renames_gets_its_skill_and_record_back() {
    let lib = library();
    let before = skill_md(&lib.root.join("keep"));
    new_version_of_keep(&lib);
    {
        let _armed = fail_points::arm("swap.between_renames");
        let crashed = catch_unwind(AssertUnwindSafe(|| {
            crate::commands::skills::reimport_local_skill_internal(&lib.store, "keep", None)
        }));
        assert!(crashed.is_err(), "the fail point crashes the update");
    }
    // What the crash left: the old copy only as the backup, nothing at `keep`.
    assert!(!lib.root.join("keep").exists());
    assert_eq!(starting_with(&lib, ".keep.backup-").len(), 1, "{:?}", lib.entries());

    assert_eq!(next_start(&lib), vec!["keep"], "the record survives the reindex");
    assert_eq!(skill_md(&lib.root.join("keep")), before, "the old copy, which the record describes");
    let skill = lib.store.get_skill_by_id("keep").unwrap().unwrap();
    let on_disk = crate::core::content_hash::hash_directory(&lib.root.join("keep")).unwrap();
    assert_eq!(skill.content_hash.as_deref(), Some(on_disk.as_str()));
    assert_eq!(lib.entries(), vec!["keep"], "no backup left");
    lib.assert_consistent();
}

#[test]
fn without_the_startup_step_the_same_crash_loses_the_record() {
    // The control for the test above: the reindex alone drops the skill.
    let lib = library();
    new_version_of_keep(&lib);
    let _armed = fail_points::arm("swap.between_renames");
    let _ = catch_unwind(AssertUnwindSafe(|| {
        crate::commands::skills::reimport_local_skill_internal(&lib.store, "keep", None)
    }));
    assert_eq!(lib.after_reindex(), Vec::<String>::new());
}

#[test]
fn a_reinstall_that_crashed_between_the_renames_gets_its_skill_back_and_lists_the_copy() {
    // The installer's swap (place_copy) has no guard to remove its staged
    // folder, so a crash leaves both halves, as a killed process would.
    let lib = library();
    let before = skill_md(&lib.root.join("keep"));
    let src = new_version_of_keep(&lib);
    {
        let _armed = fail_points::arm("swap.between_renames");
        let crashed = catch_unwind(AssertUnwindSafe(|| {
            crate::core::installer::install_skill_dir_to_destination(&src, "keep", &lib.root.join("keep"))
        }));
        assert!(crashed.is_err());
    }
    let staged = starting_with(&lib, ".keep.staged-");
    assert_eq!(staged.len(), 1, "{:?}", lib.entries());
    assert_eq!(starting_with(&lib, ".keep.backup-").len(), 1);

    assert_eq!(next_start(&lib), vec!["keep"]);
    assert_eq!(skill_md(&lib.root.join("keep")), before);
    // The new copy was being written moments ago: left, and shown.
    assert_eq!(listed(&lib), vec![(staged[0].clone(), FolderKind::Staged)]);

    // A start after an hour without changes removes it.
    let report = recover_unlocked(&lib.store, &lib.root, later());
    assert_eq!(report.removed, vec![lib.root.join(&staged[0])]);
    assert_eq!(lib.entries(), vec!["keep"]);
    assert!(listed(&lib).is_empty());
    lib.assert_consistent();
}

// ── staged folders ──

#[test]
fn a_stale_staged_folder_is_removed_and_a_recent_one_is_shown() {
    let lib = library();
    let staged = hidden(&lib, "ghost", "staged");
    write_skill(&staged, "ghost");

    let report = recover_unlocked(&lib.store, &lib.root, SystemTime::now());
    assert!(report.removed.is_empty() && report.restored.is_empty());
    assert!(staged.is_dir(), "recent: may still be in use");
    let folders = orphaned_folders::find(&lib.store).unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].kind, FolderKind::Staged);
    assert!(folders[0].note.as_deref().unwrap().contains("may still be in use"));
    // Never taken for a skill.
    let refused = orphaned_folders::adopt(&lib.store, &folders[0].dir_name).unwrap_err();
    assert!(refused.message.contains("not a skill to adopt"), "{}", refused.message);

    let report = recover_unlocked(&lib.store, &lib.root, later());
    assert_eq!(report.removed, vec![staged.clone()]);
    assert!(!staged.exists());
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

#[test]
fn a_staged_folder_in_use_by_another_process_is_left_alone() {
    // An update stages and swaps under the repo lock; another thread stands in
    // for the other process (the app and CLI share the library). The lock is a
    // file lock on its own handle, so the two exclude each other as processes do.
    let lib = library();
    let staged = hidden(&lib, "keep", "staged");
    let (locked_tx, locked_rx) = std::sync::mpsc::channel();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let writer_dir = staged.clone();
    let writer = std::thread::spawn(move || {
        let _lock = RepoLock::acquire("test: update staging").unwrap();
        write_skill(&writer_dir, "keep");
        locked_tx.send(()).unwrap();
        // Keep copying until told to stop.
        let mut n = 0;
        while done_rx.recv_timeout(Duration::from_millis(10)).is_err() {
            fs::write(writer_dir.join(format!("part-{n}.txt")), b"x").unwrap();
            n += 1;
        }
    });
    locked_rx.recv().unwrap();

    // While the copy holds the lock, the startup step waits, then gives up and
    // touches nothing, even with a clock that calls everything stale.
    let report = recover_waiting(&lib.store, Duration::from_millis(200), later());
    assert_eq!(report, None);
    assert!(staged.join("SKILL.md").is_file());

    // A copy made without the lock: the step runs, and the age keeps it.
    done_tx.send(()).unwrap();
    writer.join().unwrap();
    let report = recover_waiting(&lib.store, Duration::from_secs(5), SystemTime::now()).unwrap();
    assert!(report.removed.is_empty());
    assert_eq!(report.kept.len(), 1);
    assert!(staged.join("SKILL.md").is_file());
}

// ── backups ──

#[test]
fn a_backup_left_after_a_good_update_is_shown_then_removed() {
    let lib = library();
    new_version_of_keep(&lib);
    {
        let _armed = fail_points::arm("swap.remove_backup");
        crate::commands::skills::reimport_local_skill_internal(&lib.store, "keep", None).unwrap();
    }
    let backup = starting_with(&lib, ".keep.backup-");
    assert_eq!(backup.len(), 1);
    let new = skill_md(&lib.root.join("keep"));
    assert!(new.contains("New body"));

    // At the next start it is recent: kept, and shown as an old copy.
    assert_eq!(next_start(&lib), vec!["keep"]);
    let folders = orphaned_folders::find(&lib.store).unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!((folders[0].dir_name.as_str(), folders[0].kind), (backup[0].as_str(), FolderKind::Backup));
    assert!(folders[0].note.as_deref().unwrap().contains("is current"));

    // Still locked an hour later: kept and shown, with why.
    {
        let _armed = fail_points::arm("leftovers.remove");
        let report = recover_unlocked(&lib.store, &lib.root, later());
        assert!(report.kept[0].1.contains("could not be removed"), "{:?}", report.kept);
        assert_eq!(listed(&lib).len(), 1);
    }
    let report = recover_unlocked(&lib.store, &lib.root, later());
    assert_eq!(report.removed, vec![lib.root.join(&backup[0])]);
    assert_eq!(skill_md(&lib.root.join("keep")), new, "the live copy is never touched");
    assert_eq!(lib.after_reindex(), vec!["keep"]);
    lib.assert_consistent();
}

#[test]
fn several_backups_of_a_missing_skill_are_not_guessed_between() {
    let lib = library();
    let keep = lib.root.join("keep");
    let first = hidden(&lib, "keep", "backup");
    let second = hidden(&lib, "keep", "backup");
    fs::rename(&keep, &first).unwrap();
    write_skill(&second, "keep");

    let report = recover_unlocked(&lib.store, &lib.root, later());
    assert!(report.restored.is_empty() && report.removed.is_empty());
    assert_eq!(report.kept.len(), 2);
    assert!(report.kept[0].1.contains("which one is current cannot be told"));
    assert!(!keep.exists() && first.is_dir() && second.is_dir());
    let kinds: Vec<FolderKind> = listed(&lib).into_iter().map(|(_, kind)| kind).collect();
    assert_eq!(kinds, vec![FolderKind::Backup, FolderKind::Backup]);
}

#[test]
fn a_backup_no_record_names_is_left_and_shown() {
    let lib = library();
    let backup = hidden(&lib, "stranger", "backup");
    write_skill(&backup, "stranger");

    let report = recover_unlocked(&lib.store, &lib.root, later());
    assert!(report.kept[0].1.contains("no skill record names 'stranger'"), "{:?}", report.kept);
    assert!(backup.is_dir() && !lib.root.join("stranger").exists());
    assert_eq!(listed(&lib)[0].1, FolderKind::Backup);
    assert_eq!(lib.after_reindex(), vec!["keep"]);
}

#[test]
fn a_backup_that_cannot_be_renamed_back_is_shown() {
    let lib = library();
    let backup = hidden(&lib, "keep", "backup");
    fs::rename(lib.root.join("keep"), &backup).unwrap();
    let _armed = fail_points::arm("leftovers.restore");

    let report = recover_unlocked(&lib.store, &lib.root, later());
    assert!(report.kept[0].1.contains("could not be renamed back"), "{:?}", report.kept);
    assert!(backup.is_dir());
    assert_eq!(listed(&lib), vec![(backup.file_name().unwrap().to_string_lossy().to_string(), FolderKind::Backup)]);
}

#[test]
fn without_a_metadata_snapshot_the_database_names_the_skill() {
    let lib = library();
    fs::remove_dir_all(sync_metadata::metadata_dir()).unwrap();
    let backup = hidden(&lib, "keep", "backup");
    fs::rename(lib.root.join("keep"), &backup).unwrap();

    let report = recover_unlocked(&lib.store, &lib.root, SystemTime::now());
    assert_eq!(report.restored, vec![lib.root.join("keep")]);
    assert!(lib.root.join("keep/SKILL.md").is_file());
}

// ── the rest of the library leaves them alone ──

#[test]
fn the_fallback_reconcile_never_takes_a_hidden_copy_for_a_skill() {
    let lib = library();
    fs::remove_dir_all(sync_metadata::metadata_dir()).unwrap();
    write_skill(&hidden(&lib, "keep", "backup"), "keep");
    write_skill(&hidden(&lib, "ghost", "staged"), "ghost");

    crate::commands::git_backup::reconcile_skills_index_unlocked(&lib.store).unwrap();

    let paths: Vec<PathBuf> = lib
        .store
        .get_all_skills()
        .unwrap()
        .into_iter()
        .map(|s| PathBuf::from(s.central_path))
        .collect();
    assert_eq!(paths, vec![lib.root.join("keep")]);
}

#[test]
fn hidden_copies_never_reach_the_backup_commit() {
    if std::process::Command::new("git").arg("--version").output().is_err() {
        return; // no git on this machine
    }
    let lib = library();
    // A library backed up before this change: its .gitignore lacks the lines,
    // which the next commit adds.
    crate::core::git_backup::init_repo_unlocked(&lib.root, "test-device").unwrap();
    let gitignore = lib.root.join(".gitignore");
    let old: String = fs::read_to_string(&gitignore)
        .unwrap()
        .lines()
        .filter(|l| !l.contains(".staged-") && !l.contains(".backup-"))
        .map(|l| format!("{l}\n"))
        .collect();
    fs::write(&gitignore, old).unwrap();
    write_skill(&hidden(&lib, "keep", "backup"), "keep");
    write_skill(&hidden(&lib, "ghost", "staged"), "ghost");
    // A skill's own folder that happens to look like one is its content.
    let own = lib.root.join("keep").join(format!(".cfg.backup-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&own).unwrap();
    fs::write(own.join("settings.txt"), "mine\n").unwrap();

    crate::core::git_backup::commit_all_unlocked(&lib.root, "test").unwrap();

    let out = std::process::Command::new("git")
        .args(["ls-files"])
        .current_dir(&lib.root)
        .output()
        .unwrap();
    let tracked = String::from_utf8_lossy(&out.stdout);
    assert!(tracked.lines().any(|l| l == "keep/SKILL.md"), "{tracked}");
    assert!(!tracked.lines().any(|l| l.starts_with(".keep.") || l.starts_with(".ghost.")), "{tracked}");
    assert!(tracked.lines().any(|l| l.ends_with("settings.txt")), "a skill's own files stay: {tracked}");
}
