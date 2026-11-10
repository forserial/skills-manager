use super::*;
use crate::core::watched_upstreams::tests::{
    file_url, insert_local_skill, new_watch, test_repo, Upstream,
};
use crate::core::watched_upstreams::{add, NewWatch};
use std::sync::atomic::AtomicUsize;

fn ids(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn check_library_reports_a_moved_watch_and_an_unreachable_one_fails_nothing_else() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "moved", "moved-skill");
    insert_local_skill(&repo.store, "gone", "gone-skill");
    insert_local_skill(&repo.store, "plain", "plain-skill");
    let upstream = Upstream::new(repo.tmp.path());
    add(&repo.store, "moved", new_watch(&upstream.url), None).unwrap();
    // The unreachable watch sits on the same skill as a reachable one, too.
    add(
        &repo.store,
        "gone",
        NewWatch {
            revision: Some("a".repeat(40)),
            ..new_watch(&file_url(&repo.tmp.path().join("nowhere.git")))
        },
        None,
    )
    .unwrap();
    add(&repo.store, "gone", new_watch(&upstream.url), None).unwrap();

    upstream.commit("README.md", "moved\n");
    let summary = check_library(&repo.store, true, None);

    assert_eq!(summary.skills_checked, 2, "{summary:?}");
    assert_eq!(summary.watches_checked, 3);
    assert_eq!(summary.unreachable, 1);
    assert!(summary.failed_skills.is_empty());
    let mut moved = summary.moved_skills.clone();
    moved.sort();
    assert_eq!(moved, ids(&["gone", "moved"]));

    let gone = list("gone").unwrap();
    assert!(gone.iter().any(|w| w.check_error.is_some() && !w.moved));
    assert!(gone.iter().any(|w| w.check_error.is_none() && w.moved));

    let mut markers = markers(&repo.store);
    markers.sort_by(|a, b| a.skill_id.cmp(&b.skill_id));
    assert_eq!(
        markers,
        vec![
            WatchedMarker {
                skill_id: "gone".into(),
                watch_count: 2,
                moved_count: 1
            },
            WatchedMarker {
                skill_id: "moved".into(),
                watch_count: 1,
                moved_count: 1
            },
        ],
        "a skill that watches nothing has no marker"
    );
}

#[test]
fn watches_resolve_concurrently_and_off_the_repo_lock() {
    let repo = test_repo();
    for id in ["a", "b", "c"] {
        insert_local_skill(&repo.store, id, &format!("skill-{id}"));
        for n in 0..2 {
            add(
                &repo.store,
                id,
                NewWatch {
                    revision: Some("c".repeat(40)),
                    ..new_watch(&format!("https://example.invalid/{id}/{n}.git"))
                },
                None,
            )
            .unwrap();
        }
    }
    let in_flight = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let calls = AtomicUsize::new(0);
    let lock_held = AtomicUsize::new(0);
    // One probe at a time, so the workers don't contend with each other.
    let probe = std::sync::Mutex::new(());
    let summary = check_skills_with(&ids(&["a", "b", "c"]), None, |watch| {
        {
            let _one = probe.lock().unwrap();
            // The lock is free while remotes resolve.
            if RepoLock::acquire("probe").is_err() {
                lock_held.fetch_add(1, Ordering::SeqCst);
            }
        }
        let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        peak.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(60));
        in_flight.fetch_sub(1, Ordering::SeqCst);
        calls.fetch_add(1, Ordering::SeqCst);
        (
            watch.id.clone(),
            watch.last_seen_revision.clone(),
            Ok(("d".repeat(40), true)),
        )
    });
    assert_eq!(calls.load(Ordering::SeqCst), 6, "every watch resolved once");
    assert_eq!(
        lock_held.load(Ordering::SeqCst),
        0,
        "resolution must run off the repo lock"
    );
    assert!(peak.load(Ordering::SeqCst) > 1, "resolved concurrently");
    assert_eq!(summary.skills_checked, 3);
    assert_eq!(summary.watches_checked, 6);
    assert!(list("b").unwrap().iter().all(|w| w.moved));
}

#[test]
fn an_unforced_check_skips_skills_checked_within_the_ttl_and_skills_without_watches() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "w", "watching");
    insert_local_skill(&repo.store, "n", "not-watching");
    add(
        &repo.store,
        "w",
        NewWatch {
            revision: Some("e".repeat(40)),
            ..new_watch("https://example.invalid/w.git")
        },
        None,
    )
    .unwrap();
    let calls = AtomicUsize::new(0);
    let resolver = |watch: &WatchedUpstream| {
        calls.fetch_add(1, Ordering::SeqCst);
        (
            watch.id.clone(),
            watch.last_seen_revision.clone(),
            Ok(("e".repeat(40), false)),
        )
    };
    let all = ids(&["w", "n"]);
    let ttl = Some(Duration::from_secs(3600));

    let first = check_skills_with(&all, ttl, resolver);
    assert_eq!((first.skills_checked, calls.load(Ordering::SeqCst)), (1, 1));
    let second = check_skills_with(&all, ttl, resolver);
    assert_eq!(second, BatchSummary::default(), "within the TTL");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // A forced check ignores the TTL.
    check_skills_with(&all, None, resolver);
    assert_eq!(calls.load(Ordering::SeqCst), 2);

    // A skill without watches never reaches the resolver.
    let none = check_skills_with(&ids(&["n"]), None, resolver);
    assert_eq!(none, BatchSummary::default());
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn an_unreadable_watch_file_fails_only_that_skill() {
    let repo = test_repo();
    insert_local_skill(&repo.store, "w", "watching");
    add(
        &repo.store,
        "w",
        NewWatch {
            revision: Some("f".repeat(40)),
            ..new_watch("https://example.invalid/w.git")
        },
        None,
    )
    .unwrap();
    // A file whose watch list can't be parsed fails only that skill.
    insert_local_skill(&repo.store, "bad", "bad-file");
    let bad = crate::core::fork_files::path_for(crate::core::fork_files::ForkConcern::Watch, "bad")
        .unwrap();
    std::fs::create_dir_all(bad.parent().unwrap()).unwrap();
    std::fs::write(&bad, "{ not json").unwrap();

    let summary = check_skills_with(&ids(&["bad", "w"]), None, |watch| {
        (
            watch.id.clone(),
            watch.last_seen_revision.clone(),
            Err("unreachable".to_string()),
        )
    });
    assert_eq!(summary.failed_skills, ids(&["bad"]));
    assert_eq!(summary.skills_checked, 1);
    assert_eq!(summary.unreachable, 1);
    assert_eq!(
        list("w").unwrap()[0].check_error.as_deref(),
        Some("unreachable")
    );
}
