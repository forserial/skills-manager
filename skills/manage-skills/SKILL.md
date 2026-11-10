---
name: manage-skills
description: Manage the user's shared agent-skill library via skills-manager-cli — install, update, remove, deploy or undeploy skills per agent, manage presets, organize tags, search, and adopt existing skills. Use this whenever the user wants Claude Code, Codex, Cursor, or another agent to gain or lose a skill, wants to organize the central library, or asks what is installed or deployed. Prefer this over direct agent-folder installs because Skills Manager preserves source metadata, preset membership, updates, and cross-agent deployment state.
---

## Before doing anything

1. **Resolve the CLI first, then use the path it prints.** Run this once (POSIX
   shell):

   ```bash
   D="$HOME/.skills-manager/bin"
   B="$D/skills-manager-cli"; [ -e "$B" ] || B="$B.exe"   # .exe on Windows
   if [ -s "$D/.version" ] && [ -x "$B" ]; then
     echo "$B"
   elif [ -s "$D/.version" ] || [ -e "$B" ]; then
     echo BRIDGE_BROKEN
   else
     P="$(command -v skills-manager-cli 2>/dev/null || true)"
     [ -x "$P" ] && echo "$P"
   fi
   ```

   **Substitute the printed path into every command below**, wherever the
   examples write `$SM`. Do not carry `$SM` as a shell variable: each command
   you run is a new shell, so an assignment made here is gone by the next one.

   The three outcomes:

   - **A path under `~/.skills-manager/bin`** — the desktop app published this
     copy, and the `.version` stamp appears only after it has been verified, so
     it always matches the app the user is running. Use it.
   - **`BRIDGE_BROKEN`** — something the app left behind is here but does not
     add up: an unstamped binary, or a stamp with no binary beside it. Either is
     what a copy that failed half-way leaves. **Stop.** Do not go looking
     for another CLI: that binary may predate a safety fix, and the machine has
     a desktop app whose version nothing here can match. Ask the user to open
     the Skills Manager app once, which republishes it.
   - **A path from PATH** — nothing was ever published here, so there is no
     stale copy to worry about: this is a CLI-only machine (a server install, a
     standalone download, a hand-built binary). Use it, but note it can be
     older than a desktop app if one is also installed.

   If nothing is printed at all, this skill doesn't apply — fall back to
   find-skills, or tell the user to install Skills Manager.
2. **Always pass `--json` when you parse output yourself.** Pretty-printed output is for the user; JSON is for you. Errors include `ok=false`, a stable `code`, and `message` on stderr with a non-zero exit code.

```bash
"$SM" --json skills list
```

### When a deployment is refused

A deploy that would overwrite something that is not ours is refused outright —
nothing at those paths is deleted, and nothing else in the batch is applied.
That failure is machine-readable, so report the actual paths rather than the
sentence:

```json
{"ok": false, "code": "TARGET_CONFLICT", "kind": "target_conflict",
 "message": "Refusing to deploy: 1 of 2 target(s) …",
 "details": {"conflicts": [{"path": "/Users/me/.claude/skills/db",
                            "reason": "is not a managed deployment"}]}}
```

Tell the user which path is in the way, that its contents are untouched, and
offer the two ways out: adopt it into the library (`skills adopt`), or move it
aside and retry. Never delete it for them.

## Mental model

There's **one central library** at `~/.skills-manager/skills/` that all agents share. Each skill has source metadata, preset membership, tags, and zero or more real deployments in agent directories. A **preset** is a reusable group; several presets may be deployed at the same time.

Keep these three states separate:
- **Library**: install/remove controls whether Skills Manager owns the skill.
- **Preset membership**: `presets add-skill/remove-skill` organizes the library only.
- **Deployment**: `skills deploy/undeploy` and `presets deploy/undeploy` control what an agent can actually see.

Internally, presets are still stored as scenarios for backward-compatible Git Backup. The CLI and UI call them presets.

## Install

```bash
# From skills.sh marketplace
"$SM" skills install vercel-labs/agent-skills@react-best-practices

# Any git URL (use /tree/branch/subpath form when the skill lives in a sub-directory)
"$SM" skills install https://github.com/anthropics/skills.git
"$SM" skills install https://github.com/foo/bar/tree/main/skills/baz

# Local folder
"$SM" skills install ./my-skill

# Force a source type when the ref is ambiguous
"$SM" skills install foo/bar --skillssh
"$SM" skills install ./looks-like/owner-repo --local
```

**Default is library-only** — the skill enters the DB but doesn't appear in any agent yet. Prefer an explicit follow-up deployment so scope is unambiguous:

```bash
"$SM" skills deploy <skill> --agent claude_code --agent codex
```

`--sync` and `--sync-preset` remain legacy shortcuts for the exclusive active-preset workflow.

**Ref resolution** is deterministic, no path-existence guessing:
1. Starts with `./`, `../`, `/`, or `~/` → local path
2. Contains `://`, ends in `.git`, or starts with `git@` → git URL
3. Matches `owner/repo`, `owner/repo/skill`, or `owner/repo@skill` → skillssh
4. Otherwise → error; pass `--local` / `--git` / `--skillssh` to disambiguate

**Always verify after install** with `skills list` or `skills show <name>` so you can confirm the skill landed and report the preset / sync state back to the user.

## Search

```bash
"$SM" --json skills search "react performance" --limit 5
```

Each result has `install_ref` (paste straight into `skills install`), `installs` (popularity proxy), and `skills_sh_url`. Show the top 1–3 with install counts before installing — anything with 10K+ installs is battle-tested; anything under 100 needs a careful look at the source repo.

## Update / Check

```bash
# Re-fetch one skill (git/skillssh re-clones, local/import re-imports source dir)
"$SM" skills update <skill-name-or-id>

# Re-fetch all eligible skills
"$SM" skills update --all

# Just probe remote revisions, don't touch files
"$SM" skills check --all
```

`check` is the dry-run partner of `update`. A skill installed from a local folder is compared with that folder, not with any Git remote: `update_available` means the folder changed since it was imported, and `source_missing` means the folder is gone. This check always runs; the cache that spares Git remotes a round-trip within an hour (bypass it with `--force`) does not apply to it. Website skills have nothing to compare against and come back `skipped: true`.

**An update replaces the skill's directory wholesale**, so anything written inside it that the new version does not have would be destroyed. When the CLI detects that, it applies nothing and reports the paths instead:

```jsonc
{ "name": "ppt-master", "refreshed": false,
  "held_back_removals": ["library: templates/mine.pptx"] }
```

The field is omitted entirely when nothing is held back, so test for its presence rather than for an empty array. `refreshed: false` *with* `held_back_removals` is **not a failure and not something to retry** — the skill is untouched and still on its old version. Show the user the listed paths and ask. There is no CLI flag to override this; only the desktop app can confirm and proceed, because only a person can say those files are expendable. The paths are prefixed with where they live (`library`, or an agent key for a deployed copy).

### Local edits are not overwritten silently

An edited file is never replaced without asking. `skills update <skill>` for one named skill merges local changes onto the new version (see [Merge on update](#merge-on-update)). Where it cannot merge (`--all`, the automatic updater, or a baseline that is unknown or suspect), an update that would replace a library file that differs from its baseline (see [Baselines](#baselines)) applies nothing and reports the files beside any removals:

```jsonc
{ "name": "ppt-master", "refreshed": false,
  "held_back_overwrites": ["library: SKILL.md"],
  "held_back_reason": "local_changes" }
```

- Handle it exactly like `held_back_removals`: the skill is untouched, it is not an error, don't retry. Both fields are omitted when empty. There is still no CLI flag to approve; the app's update dialog lists the edits in their own group and one confirmation covers both lists.
- Without merging, it holds even when upstream changed a *different* file, because the update would otherwise put upstream's old copy of the edited file back. An edit that already matches upstream's new content is not held.
- `held_back_reason` says why: `local_changes` (the listed files were edited, added or deleted locally), or, when the baseline cannot tell which files the user changed, every file the update would change is listed and the reason is `baseline_not_backfilled` (no baseline yet; `skills baseline rebuild` narrows the list), `baseline_unrecoverable` (no baseline and its revision is gone upstream, so a rebuild cannot help; approving takes upstream's version once) or `baseline_suspect` (a library sync conflict on the skill is pending; resolve it first).
- `skills update --all`, the app's "update all" and the automatic updater (even with apply on) never approve: such skills are left alone and reported as held back.
- **Limitation:** a device in the same sync group that runs an upstream build of Skills Manager does not know about baselines, so its updates still overwrite local edits (inferred, not tested).

### Merge on update

`skills update <skill>` for one named git or skills.sh skill with local changes merges them onto the new upstream version, file by file against the baseline:

- a file changed on one side only takes that side's bytes, line endings included; a file only the library has (added locally) is carried over and needs no approval;
- a text file changed on both sides is merged three-way (line endings are ignored while merging and the library file's CRLF is kept); when the edits clash it is a conflict;
- a binary file changed on both sides, a file changed on one side and deleted on the other, and a file both sides added differently are conflicts;
- a file deleted upstream and unchanged locally is still a removal, held for approval as above.

A clean merge applies and reports what it merged; the baseline moves to the new revision (origin `merge`), so the edits remain local changes against it:

```jsonc
{ "name": "ppt-master", "refreshed": true, "merged": ["SKILL.md"] }
```

With conflicts **nothing is changed** and the update is held. The report lists them (`merge_conflicts: [{ path, kind, ... }]`), and a pending merge is kept outside the library (`<app data>/pending-merges/<id>/`, not synced):

```bash
"$SM" --json skills merge status <skill>      # conflicts, choices so far, and versions_dir per file
"$SM" --json skills merge resolve <skill> SKILL.md --take local      # keep mine
"$SM" --json skills merge resolve <skill> notes.md --take upstream   # take theirs
"$SM" --json skills merge resolve <skill> notes.md --file fixed.md   # supply resolved content
"$SM" --json skills merge apply <skill>       # finish the update with those choices
"$SM" --json skills merge abort <skill>       # drop it; the library was never touched
```

- `kind` is `text`, `binary`, `deleted_upstream`, `deleted_locally`, `added_both`, `base_gone` (the baseline revision is gone upstream, so its content cannot be had: choose a version) or `base_unavailable` (the baseline content could not be fetched now: update again later, or choose a version).
- `versions_dir` holds `<name>.mine.<ext>`, `<name>.theirs.<ext>` and, when known, `<name>.base.<ext>`. To hand-merge, edit a copy and pass it with `--file`; content that still has conflict markers is refused.
- A choice binds the exact two versions it was made for. `merge apply` holds again, asking only about what changed, if upstream moved or the library file was edited since. Show the user each conflict and ask; never pick a side for them.
- Merging needs a person: `skills update --all`, the app's "update all" and the automatic updater never merge, and a skill whose baseline is unknown or suspect is never merged (it holds as above).
- In the app, the update shows "N files merged automatically, M need a decision" with keep mine, take theirs, or open both versions per file.

## Pin and review

**Pin** a git or skills.sh skill to hold it at the revision the user reviewed. A pinned skill is still checked, but nothing moves it: `skills update <skill>` refuses with a message naming `skills unpin`, and `skills update --all`, the app's "update all" and the automatic updater skip it and report it. Pinning is also the safety measure for a skill that was clean when assessed and may not stay so.

```bash
"$SM" --json skills pin <skill> --note "reviewed 2026-11"   # note is optional
"$SM" --json skills unpin <skill>
"$SM" --json skills list --pinned
```

- `skills status --json` reports `pin: { pinned, revision, pinned_at, note }`, or `null` when not pinned. `pinned_at` is Unix milliseconds.
- `skills check --json` carries `pinned: true` for a pinned skill. With `update_status: "update_available"` that reads "update available (pinned)": upstream moved, and the skill stays put until unpinned.
- `skills update --all --json` lists a pinned skill with `pinned: true` and `refreshed: false`. That is not a failure and not something to retry.
- After `skills unpin`, the next update is an ordinary one.
- Local and website skills have no upstream revision and cannot be pinned.
- Pins live in `.skills-manager/fork-pin.<skill_id>.json` and sync with the library; the newest change wins when two devices change the same pin. Never edit them by hand.

Before deciding to move a skill, **review** what an update would bring:

```bash
# The library copy against the latest source: the same entries as the app's diff view
"$SM" --json skills diff <skill>                # same as --upstream
# Upstream commits since the baseline revision that touch the skill's directory
"$SM" --json skills diff <skill> --commits
```

- `--upstream` returns `{ revision, entries: [{ relative_path, status, content_kind, original_text, updated_text, ... }] }`. `original_text` is the library copy, `updated_text` the source. `status` is `added`, `removed` or `modified`.
- `--commits` returns `{ base_revision, base_from, remote_revision, subpath, reachable, message, commits: [{ sha, date, author, subject }], truncated }`, newest first. It starts from the skill's baseline revision (`base_from: "baseline"`), or from its recorded source revision when it has no baseline. It fetches history over the network, blobless, without locking the library.
- `reachable: false` means the list cannot be made: the base revision is gone upstream, or upstream history was rewritten so it is no longer an ancestor of the head. `message` says which. That is not an error to retry.

## Remove

```bash
# Always preview first when removing more than one
"$SM" skills remove <skill> --dry-run

# --yes is required for the actual delete; --json mode does NOT auto-confirm
"$SM" skills remove <skill> --yes
```

Remove deletes the central-library copy, all synced targets across agents, and the DB row. It's not reversible without re-installing.

## Deploy / Undeploy

```bash
"$SM" skills deploy <skill> --agent claude_code
"$SM" skills undeploy <skill> --agent codex
"$SM" skills deploy <skill-a> <skill-b> --agent codex --dry-run
"$SM" skills deploy <skill> --agent claude_code --agent codex
"$SM" --json skills status <skill>
```

These commands change real managed deployments without deleting the central-library copy or changing preset membership. `skills enable/disable` are deprecated compatibility commands and do not change deployment; never use them.

`skills deploy` and `skills undeploy` always require at least one explicit `--agent`, whether the command names one skill or several. `skills status` also reports target rows left by a custom agent that is no longer registered, so stale deployments stay visible and can be cleaned with an explicit undeploy while the row exists.

## Legacy exclusive sync

```bash
# Sync current active preset to all enabled agents
"$SM" skills sync

# Preview the target list — safe, no writes
"$SM" skills sync --dry-run

# Switch the one legacy active preset, then sync
"$SM" skills sync --preset "Web Dev"

# Only sync to a single agent (useful when one agent's directory got out of sync)
"$SM" skills sync --tool claude_code
```

## Adopt skills installed elsewhere

When skills already live in an agent's directory (e.g. installed via `npx skills add` or manual `git clone`) but aren't in the central library, pull them in:

```bash
# Dry-run scan first — lists candidates without writing
"$SM" skills adopt ~/.claude/skills --dry-run

# Adopt everything found — each becomes source_type=local (can't auto-update from git)
"$SM" skills adopt ~/.claude/skills

# Adopt a single skill and pin it to a git source so `update` works later
"$SM" skills adopt ~/.claude/skills/react-best-practices \
  --git-url https://github.com/vercel-labs/agent-skills/tree/main/react-best-practices

# Or pass --git-subpath explicitly when the URL is just the repo root
"$SM" skills adopt ~/.claude/skills/react-best-practices \
  --git-url https://github.com/vercel-labs/agent-skills \
  --git-subpath react-best-practices

# Skill lives at the repo root? Pass an empty subpath
"$SM" skills adopt ~/.claude/skills/my-skill \
  --git-url https://github.com/me/my-skill --git-subpath ""
```

`adopt` auto-excludes anything already in the DB or already a sync target, so it's safe to re-run. `--git-url` requires either a URL with a subpath (`/tree/branch/path`) or an explicit `--git-subpath` — without that, future `update` would re-clone the wrong directory, so the CLI refuses to guess.

`--git-url` only applies at the moment of adoption, while the directory is still unmanaged. Once a skill is in the library, use `set-source` below.

## Re-point a skill at a git source

```bash
# Preview: resolves the source and reports whether content differs. It clones to
# a temp dir, but writes nothing to the library or the DB.
"$SM" --json skills set-source <skill> --git-url you/skills --subpath my-skill --dry-run

# A GitHub /tree/ URL carries the branch and subpath already
"$SM" skills set-source <skill> --git-url https://github.com/you/skills/tree/main/my-skill
```

This is how a `local` skill becomes git-backed so `update` works, and how a
skill pointed at the wrong repo gets corrected. It updates the row **in place**,
so the skill id survives and the tags, preset membership and per-agent
deployments keyed to it all stay intact.

- The flag is `--subpath` here, not `--git-subpath` — that one belongs to `adopt`. Pass `--subpath ""` when the skill is at the repo root, which must itself hold a `SKILL.md`.
- `--branch` overrides a branch encoded in the URL.
- The report carries `content_changed` — a single boolean, **not** a file list. It compares the new source against the hash currently recorded for the library copy, not a fresh hash of the directory on disk, so edits made inside the central copy afterwards do not register as a difference. When it is `false` the library copy is left untouched and those edits survive; copy-mode deployments are re-synced either way.

**`--force` is destructive, and nothing stands between it and the user's files.**
A content difference is refused without it. With it, the whole skill directory is
replaced — staged, swapped in, and the old copy deleted — so anything in the
library copy that the new source does not ship is gone. Unlike `skills update`,
this path has **no** `held_back_removals` check: nothing is withheld, and nothing
asks. `--dry-run` cannot tell you which files are at stake, only that something
differs. Never pass `--force` on the user's behalf — report `content_changed:
true`, say that proceeding overwrites the library copy wholesale, and let them
decide.

## Baselines

A git or skills.sh skill carries a **baseline**: the upstream revision its files
came from and a per-file manifest of what upstream shipped there. It is recorded
on install, update and `set-source`, and lets later features tell local edits
apart from upstream changes. `skills show` and `skills status --json` report it
as `baseline: { revision, origin, recorded_at, file_count }`, or `null` when it
is unknown. Recording it changes no update behaviour yet.

```bash
# Backfill skills installed before baselines existed (fetches each skill's
# recorded source revision; network, but writes only the baseline files)
"$SM" --json skills baseline rebuild --all
# One skill, even if it already has a baseline
"$SM" --json skills baseline rebuild <skill>
```

- The report counts `rebuilt`, `skipped`, `unreachable` and `failed`, and exits zero when some are unreachable. `unreachable` means the server no longer has that revision ("not our ref"): nothing is written and retrying will not help. `failed` is any other failure ("could not fetch now"): retry later.
- `skipped` covers skills with no recorded source revision. After a backup clone every skill on the new device has none, so **run backfill on the original device**; the baseline files then travel with the library.
- Each rebuilt skill reports `differing_files`: library files that differ from the rebuilt baseline. Backfill trusts the recorded revision, which can be older than the content when another device updated the skill, so a large count is worth a look.
- Baselines live in `.skills-manager/fork-baseline.<skill_id>.json` and sync with the library. Never edit or delete them by hand; removing a skill removes its baseline.
- **Known limitations.** A device running an upstream build of Skills Manager keeps these files but its updates do not touch them, so in a mixed group of devices a baseline can go stale. When an existing library first connects to a remote by clone, the remote's metadata folder replaces the local one and local baselines are dropped; those skills read `baseline: null` until rebuilt. After a library sync conflict is resolved, the baseline follows the kept content, or is dropped when that cannot be determined.

### Local changes

Against its baseline, every skill reads one of: `clean`, `changed`,
`baseline_unknown` (a git skill with no baseline yet), `baseline_suspect` (a
library sync conflict on it is pending, so its baseline may describe the other
side's content) or `untracked` (a local skill: no upstream, no baseline). Treat
`baseline_suspect` like `baseline_unknown`. Line-ending differences are never
a change.

```bash
# Which files of a skill differ from what upstream shipped
"$SM" --json skills status <skill>     # local_changes: { state, baseline_revision, files: [{ path, state }] }
# Only the skills with local edits; every list row gains local_change_count,
# baseline_known and local_state
"$SM" --json skills list --modified
# The library copy against its baseline, file by file
"$SM" --json skills diff <skill> --local
```

- File states are `modified`, `added` (not in the baseline) and `deleted` (in the baseline, gone from the library).
- `local_change_count` is `null` when the baseline is not known: an unknown baseline is not "no changes".
- `skills diff --local` takes the baseline content from this device's cache, else fetches the baseline revision. When neither works, `entries` is `null` and `files` still lists the changed paths, with the reason in `message`.
- The app shows the same: a "Your changes" section in the skill's detail panel, an "Edited" marker on its card, and a quieter "Baseline unknown" marker on git skills without a baseline. Edits made on disk show without a restart.

### Change notes and patch export

Each locally changed file can carry a one-line reason ("removed the usage
ping", "Windows paths"). Notes sync with the library. When the user asks why a
file was changed, or to record why, use these instead of a side file.

```bash
# The local changes with state and note: files: [{ path, state, note, note_updated_at }]
"$SM" --json skills changes <skill>
# Set a changed file's note (path relative to the skill directory), or clear it
"$SM" --json skills changes note <skill> refs/a.md "Windows paths"
"$SM" --json skills changes note <skill> refs/a.md --clear
# A unified diff of the library copy against the baseline, to stdout or a file
"$SM" skills changes export <skill> > local.patch
"$SM" --json skills changes export <skill> --output local.patch   # prints a summary
```

- Only a file that differs from a known baseline takes a note. On `baseline_unknown` or `baseline_suspect`, `note` is refused and stored notes are kept under `held_notes`.
- A note is dropped by itself once its file no longer differs from the baseline: the file was reverted, or an update brought upstream level with it. It stays with its path while the file still differs, whatever the baseline revision.
- The patch covers the changed files only. Its paths start at the upstream repository root (`path_prefix` in the JSON is the skill's directory there), so `git apply local.patch` works at the root of a checkout of the baseline revision (`baseline_revision`). Each note is a `# <path>: <note>` line directly above its file's `diff --git` header. The library side takes the baseline file's line endings, since endings are never a change.
- Export needs the baseline content: this device's cache, else a fetch of the baseline revision. It fails with a message when neither works, when the baseline is unknown or suspect, or when there is nothing to export. Don't retry a "revision is gone upstream" failure.
- The app shows the notes in "Your changes": a note button on each file (Enter saves, Esc cancels, empty removes) and an "Export patch" button that asks where to save.

## Borrowed skills: watched upstreams and fork

For a skill that will never merge with a source again — a rewrite, or the
user's own skill that borrowed an idea — record where it came from as a
**watched upstream**. It is only checked for movement: nothing is fetched into
the library and nothing is merged. It works for skills of any source type and
does not change the skill's own source.

```bash
# Watch a location. Without --revision, its current head counts as already seen.
"$SM" --json skills watch add <skill> --git-url owner/repo --subpath skills/x --note "lifted the retry loop"
"$SM" --json skills watch add <skill> --git-url https://github.com/o/r/tree/main/skills/x --revision <sha>

# List: `moved` per entry, as of the last check. --check resolves the remotes first.
"$SM" --json skills watch list <skill> --check

# "I have looked": the upstream's current revision becomes the seen one; moved clears.
"$SM" --json skills watch seen <skill> <watch-id>
"$SM" --json skills watch remove <skill> <watch-id>

# Copy a library skill into a new local skill that watches the original's git source
"$SM" --json skills fork <skill> --as <new-name>
```

- `skills check` carries `watched_moved` for every skill (and a `watched` list
  when it watches something), whatever its source type, so one
  `skills check --all` covers both updates and borrowed sources.
- With `--subpath`, only commits that change that directory count as movement.
  An upstream that can't be reached is reported in `check_error` on its entry;
  the rest of the check goes on.
- `fork` refuses a name that is already taken. The new skill is `local` with no
  source folder, no tags, no preset membership and no deployments; the original
  is untouched. Its content is copied byte for byte except the `name:` line of
  the `SKILL.md` frontmatter, which becomes the new name (the library names a
  skill after that line). The watch starts at the revision the original's
  content came from (its baseline, else `source_revision`); when neither is
  known it starts unseen and the first check reports it as moved. A local,
  imported or website original has no git source, so its fork watches nothing.
- Watches live in `.skills-manager/fork-watch.<skill_id>.json` and travel with
  the library through Git backup; nothing is written inside a skill folder.
  When two devices change the same skill's watches at once, the later commit
  wins. Deleting the skill deletes its watches.

## Tag

```bash
"$SM" skills tag add <skill> web frontend
"$SM" skills tag remove <skill> frontend
"$SM" skills tag set <skill> web frontend
"$SM" skills tag rename frontend web
"$SM" skills tag delete obsolete --dry-run
"$SM" skills tag delete obsolete --yes
"$SM" skills tag list <skill>   # tags on one skill
"$SM" skills tag list           # all distinct tags
```

Useful organization queries:

```bash
"$SM" --json skills list --untagged
"$SM" --json skills list --no-preset
"$SM" --json skills list --tag frontend
"$SM" --json skills list --preset "Web Dev"
"$SM" --json skills list --deployed-to codex
```

Evaluate: `skills evaluate <skill> [--evaluator skill-judge] [--agent claude_code] [--model <m>]` runs a paid agent review (about $1, a few minutes; ask first), and `skills evaluations <skill>` lists stored runs newest first with `stale: true` once the skill changed.

## Presets

```bash
"$SM" presets list
"$SM" presets current
"$SM" presets show "Web Dev"
"$SM" presets create "Web Dev" --description "Frontend work"
"$SM" presets update "Web Dev" --name "Frontend"
"$SM" presets delete "Old" --dry-run
"$SM" presets delete "Old" --yes

"$SM" presets add-skill <preset> <skill>...
"$SM" presets remove-skill <preset> <skill>...

"$SM" presets deploy <preset>                  # all enabled coding agents
"$SM" presets deploy <preset> --agent codex
"$SM" presets undeploy <preset> --agent claude_code
"$SM" presets undeploy <preset>                # every agent with target rows for this preset
"$SM" --json presets status <preset>
```

`deploy/undeploy` are additive and match the app's Preset pills. Explicit `presets apply/deactivate` commands remain for the legacy exclusive active-preset model; do not use them for normal "turn this preset on/off" requests.

The no-`--agent` defaults intentionally differ: deploy targets all installed, enabled coding agents; undeploy discovers the preset's actual target rows and removes them even when an agent is now disabled, uninstalled, or no longer registered. Use the no-agent undeploy for "turn this preset off everywhere."

Preset create/update/delete and add-skill/remove-skill are organization-only CLI operations. They never deploy or undeploy agent files implicitly.

## Health check

When sync misbehaves or a command errors in a confusing way:

```bash
"$SM" --json repo status   # base dir, skill / preset counts, active preset
"$SM" --json agents list  # detected agents and their target paths
"$SM" agents enable codex
"$SM" agents disable claude_code
```

`repo status` and `agents list` are read-only and are the first checks for "why isn't this skill showing up in Cursor" questions. `agents disable` is a real mutation: it removes every managed deployment for that agent. `agents enable` makes the agent globally available again and re-syncs the legacy active preset, if one exists; use explicit skill or preset deployment afterward when the requested state is additive.

Use `agents disable <agent>` when the user wants the whole Agent integration turned off or wants every managed skill removed from it. If they only want one skill or preset removed while keeping the Agent available for future deployments, use `skills undeploy` or `presets undeploy` instead.

## Typical workflows

### "Find me a skill for X" / "Install a skill that does X"

1. `skills search "X" --limit 5` — show the top 1–3 hits with install counts and source.
2. If a clear winner: `skills install <install_ref>`.
3. If ambiguous: ask the user to pick.
4. Deploy it to the agent(s) the user requested with `skills deploy`.
5. `skills status <name>` to confirm the library and deployment state.

### "What skills do I have?"

```bash
"$SM" --json skills list
```

The `preset_ids`, `presets`, `deployed_to`, `tags`, and `source_type` fields are usually the most informative. The legacy `enabled` field is not deployment state.

### "Pull in the skills already installed in my agent directories"

1. `skills adopt ~/.claude/skills --dry-run` (and any other agent dirs the user mentions) — show the candidate list.
2. After user confirms: `skills adopt ~/.claude/skills`.
3. For any adopted skill where the user knows the original repo, restore the update link with `skills set-source <skill> --git-url ... --subpath ...`.

### "Update everything"

```bash
"$SM" skills check --all     # see what has upstream changes
"$SM" skills update --all    # apply
```

Report which skills actually refreshed (`refreshed: true` in the JSON) vs which were already up-to-date. Skills reported with `pinned: true` were skipped on purpose; mention them, and don't unpin them unless the user asks. Skills with `held_back_removals` or `held_back_overwrites` were held for the user's decision; list the paths and say why. `--all` never merges local changes: for a locally changed skill, offer `skills update <skill>`, which does.

## Pitfalls

- **Install succeeded but skill doesn't appear in the agent** → install defaults to library-only. Use `skills deploy <skill> --agent <key>`.
- **Preset membership changed but agent files did not** → membership is organization only. Follow with `presets deploy` or `skills deploy` when the user also asked to make it visible.
- **No active preset** only affects legacy `skills sync` / `presets apply`; additive deploy commands do not require one.
- **Adopted skills can't be `update`d from git** → `npx skills add` and manual `git clone` don't leave source metadata, so adopt has to treat them as `local`. Re-point them with `skills set-source`. Do **not** reach for `adopt --git-url` here: adopt only ever creates new library entries, and it fails *late* — `--dry-run` returns `ok: true` with the skill sitting in `skipped`, and only the real run errors with `--git-url requires exactly one adoptable skill, found 0`. Do **not** remove-then-reinstall either — that drops the skill id, and with it the tags, preset membership and every per-agent deployment.
- Use `--dry-run` before bulk remove, tag delete, preset delete, deploy, or undeploy operations. Use `check` before `update`.
