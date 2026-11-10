//! `skills orphans` (sm-69q.16): library folders no skill record points at,
//! and `skills orphans adopt` to give one a record back in place.
//!
//! The work is `app_lib::core::orphaned_folders`; this is the CLI's surface.

use super::*;
use app_lib::core::orphaned_folders::{self, AdoptedFolder, OrphanedFolder, SkippedFolder};

#[derive(Args, Debug)]
pub(crate) struct OrphansArgs {
    #[command(subcommand)]
    command: Option<OrphansCommand>,
}

#[derive(Subcommand, Debug)]
enum OrphansCommand {
    /// Give a folder a record in place, as a new local skill with a new id.
    /// Its files are not changed. What it had before (id, source, baseline,
    /// pin, change notes, watched upstreams) cannot be recovered.
    Adopt {
        /// Folder name at the library root, as `skills orphans` lists it
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        dir: Option<String>,
        /// Adopt every listed folder that has a SKILL.md
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Serialize)]
pub(crate) struct OrphansAdoptReport {
    ok: bool,
    adopted: Vec<AdoptedFolder>,
    skipped: Vec<SkippedFolder>,
    /// What adopting cannot bring back, and how to link a git source again.
    cannot_recover: &'static str,
    /// Only when something was adopted: how to give it a git source.
    #[serde(skip_serializing_if = "Option::is_none")]
    next: Option<String>,
}

pub(crate) fn run_orphans(args: OrphansArgs, store: &SkillStore, json: bool) -> anyhow::Result<()> {
    match args.command {
        None => print_json(&list_orphans(store)?, json),
        Some(OrphansCommand::Adopt { dir, all }) => {
            print_json(&adopt_orphans(store, dir.as_deref(), all)?, json)
        }
    }
    Ok(())
}

/// `skills orphans`: the folders, and nothing else.
fn list_orphans(store: &SkillStore) -> anyhow::Result<Vec<OrphanedFolder>> {
    orphaned_folders::find(store)
}

/// `skills orphans adopt <dir>` or `--all`.
fn adopt_orphans(
    store: &SkillStore,
    dir: Option<&str>,
    all: bool,
) -> anyhow::Result<OrphansAdoptReport> {
    let (adopted, skipped) = match (dir, all) {
        (_, true) => {
            let result = orphaned_folders::adopt_all(store).map_err(map_app_err)?;
            (result.adopted, result.skipped)
        }
        (Some(dir), false) => (
            vec![orphaned_folders::adopt(store, dir).map_err(map_app_err)?],
            Vec::new(),
        ),
        (None, false) => bail!("name a folder, or pass --all"),
    };
    let next = adopted.first().map(|first| {
        format!(
            "To link a git source: skills set-source {} --git-url <url> --dry-run, \
             then again without --dry-run",
            first.skill_id
        )
    });
    Ok(OrphansAdoptReport {
        ok: true,
        adopted,
        skipped,
        cannot_recover: orphaned_folders::CANNOT_RECOVER,
        next,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    const PROBE_ENV: &str = "SKILLS_MANAGER_CLI_ORPHANS_PROBE";

    fn parse(args: &[&str]) -> Result<SkillsCommand, clap::Error> {
        let mut full = vec!["skills-manager-cli", "skills"];
        full.extend_from_slice(args);
        match Cli::try_parse_from(full).map(|cli| cli.command) {
            Ok(Commands::Skills(SkillsArgs { command })) => Ok(command),
            Ok(other) => panic!("unexpected command {other:?}"),
            Err(err) => Err(err),
        }
    }

    #[test]
    fn orphans_commands_parse() {
        assert!(matches!(
            parse(&["orphans"]).unwrap(),
            SkillsCommand::Orphans(OrphansArgs { command: None })
        ));
        assert!(matches!(
            parse(&["orphans", "adopt", "my-skill"]).unwrap(),
            SkillsCommand::Orphans(OrphansArgs {
                command: Some(OrphansCommand::Adopt { dir: Some(dir), all: false })
            }) if dir == "my-skill"
        ));
        assert!(matches!(
            parse(&["orphans", "adopt", "--all"]).unwrap(),
            SkillsCommand::Orphans(OrphansArgs {
                command: Some(OrphansCommand::Adopt { dir: None, all: true })
            })
        ));
        assert!(parse(&["orphans", "adopt"]).is_err(), "a folder or --all");
        assert!(parse(&["orphans", "adopt", "x", "--all"]).is_err(), "not both");
    }

    /// The library root is process-wide state, so the scenario runs in a child
    /// test process on a scratch library, as the sm-69q.11 CLI tests do.
    #[test]
    fn list_adopt_and_reopen_on_a_scratch_library() {
        let root = tempdir().unwrap();
        let name = format!(
            "{}::probe_list_adopt_and_reopen",
            module_path!().split_once("::").unwrap().1
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--ignored", "--nocapture", "--test-threads=1"])
            .env(PROBE_ENV, root.path())
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success() && stdout.contains("1 passed"),
            "{stdout}{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// A scratch library, opened the way the CLI opens one (with the startup
    /// reindex).
    fn open_library(root: &Path) -> Arc<SkillStore> {
        central_repo::set_runtime_base_dir_override(Some(root.join("base")));
        central_repo::set_runtime_skills_dir_override(Some(root.join("skills")));
        app_state::initialize_cli_store().unwrap()
    }

    fn files(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut all: Vec<_> = walkdir::WalkDir::new(dir)
            .into_iter()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().is_file())
            .map(|e| {
                (
                    e.path().strip_prefix(dir).unwrap().to_path_buf(),
                    fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        all.sort();
        all
    }

    #[test]
    #[ignore = "probe: run by list_adopt_and_reopen_on_a_scratch_library in a child process"]
    fn probe_list_adopt_and_reopen() {
        let Some(root) = std::env::var_os(PROBE_ENV).map(PathBuf::from) else {
            return;
        };
        let skills = root.join("skills");
        fs::create_dir_all(&skills).unwrap();
        let store = open_library(&root);

        // One managed skill, installed through the CLI's own local install.
        let source = root.join("src/managed");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), "---\nname: managed\n---\nbody\n").unwrap();
        install_local_action(&store, &source.to_string_lossy(), None, None).unwrap();
        // An orphan, a folder that is not a skill, and things that are not folders' business.
        let orphan = skills.join("lost-skill");
        fs::create_dir_all(orphan.join("refs")).unwrap();
        fs::write(orphan.join("SKILL.md"), "---\nname: lost\ndescription: d\n---\nbody\r\n").unwrap();
        fs::write(orphan.join("refs/a.txt"), "edited locally\n").unwrap();
        fs::create_dir_all(skills.join("notes")).unwrap();
        fs::create_dir_all(skills.join(".git")).unwrap();
        fs::create_dir_all(skills.join(".cache")).unwrap();
        let before = files(&orphan);

        let listed: Value = serde_json::to_value(list_orphans(&store).unwrap()).unwrap();
        let names: Vec<_> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|f| (f["dir_name"].as_str().unwrap(), f["has_skill_md"].as_bool().unwrap()))
            .collect();
        assert_eq!(names, vec![("lost-skill", true), ("notes", false)]);
        assert_eq!(listed[0]["name"], "lost");

        let refused = adopt_orphans(&store, Some("notes"), false).unwrap_err();
        assert!(refused.to_string().contains("has no SKILL.md"), "{refused:#}");

        let report = serde_json::to_value(adopt_orphans(&store, Some("lost-skill"), false).unwrap())
            .unwrap();
        assert_eq!(report["ok"], true);
        let id = report["adopted"][0]["skill_id"].as_str().unwrap().to_string();
        assert!(report["cannot_recover"].as_str().unwrap().contains("cannot be recovered"));
        assert!(report["next"].as_str().unwrap().contains(&id));
        assert_eq!(files(&orphan), before, "the folder's files are unchanged");

        // A new process: the startup reindex keeps it, under the same id.
        drop(store);
        let store = open_library(&root);
        let skill = resolve_skill(&store, "lost").unwrap();
        assert_eq!(skill.id, id);
        assert_eq!(PathBuf::from(&skill.central_path), orphan);
        assert_eq!(skill.source_type, "local");
        let listed: Value = serde_json::to_value(list_orphans(&store).unwrap()).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
        assert_eq!(files(&orphan), before);
    }
}
