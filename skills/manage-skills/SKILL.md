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

`skills update` never replaces a file the user changed without asking, and never
silently deletes a file the new version no longer ships. When it would, it
changes nothing and reports why: see [Reading an update report](#reading-an-update-report).

## Adapted skills: local changes that survive updates

A git or skills.sh skill can be edited in the library and still be updated
from upstream. Skills Manager records a **baseline** for each such skill: the
upstream revision its files came from, with a per-file manifest of what
upstream shipped there. Comparing the library with the baseline tells the
user's changes apart from upstream's, so an update can keep the first and take
the second. Line-ending differences are never a change.

The baseline is recorded on install, update, merge and `set-source`. A skill
installed before baselines existed, or by a build of Skills Manager without
them, has none until it is backfilled (see [Baselines](#baselines-and-backfill)).

### What an update does with local changes

`skills update <skill>`, for **one named skill**, merges file by file against
the baseline:

- a file changed on one side only takes that side's bytes, line endings
  included;
- a file only the library has (the user or the skill wrote it) is kept and
  needs no approval;
- a text file changed on both sides is merged three-way; the library file's
  CRLF endings are kept. When the edits clash it is a conflict;
- a binary file changed on both sides, a file changed on one side and deleted
  on the other, and a file both sides added differently are conflicts;
- a file deleted upstream and unchanged locally is a removal, held for
  approval.

A clean merge applies and moves the baseline to the new revision (origin
`merge`); the edits stay local changes against it. With any conflict nothing
is changed and the update is held (see [Resolving a held merge](#resolving-a-held-merge)).

**Merging needs a person.** `skills update --all`, the app's "update all" and
"update selected", and the automatic updater (even with apply on) never merge.
They update a skill only when it has no local changes, or when upstream has not
moved since the baseline (then there is nothing to take, and they report
nothing). A locally changed skill whose upstream moved is held, not merged.
A skill whose baseline is unknown or suspect is never merged by anyone: every
file the update would change is held.

### Reading an update report

`skills update --json` returns one row per skill. `refreshed: true` means files
changed. These fields are omitted when empty, so test for their presence:

```jsonc
{ "name": "a", "refreshed": true,  "merged": ["SKILL.md"] }            // merged three-way
{ "name": "b", "refreshed": false, "pinned": true }                     // --all skipped a pin
{ "name": "c", "refreshed": false, "held_back_removals": ["library: templates/mine.pptx"] }
{ "name": "d", "refreshed": false, "held_back_overwrites": ["library: SKILL.md"],
  "held_back_reason": "local_changes" }
{ "name": "e", "refreshed": false, "merge_conflicts": [{ "path": "SKILL.md", "kind": "text" }] }
```

- A held row (`held_back_removals`, `held_back_overwrites`, `merge_conflicts`)
  or a `pinned` row is **not a failure and not something to retry**: the skill
  is untouched and still on its old version. Show the user the paths and why.
- `held_back_removals`: the new version drops these paths (prefixed with where
  they live: `library`, or an agent key for a copy-mode deployment). Only the
  app can approve deleting them; there is no CLI flag, because only a person
  can say they are expendable.
- `held_back_overwrites`: the update would replace these locally changed
  files. `held_back_reason` is `local_changes` (`--all` or the automatic
  updater met a locally changed skill: offer `skills update <skill>`, which
  merges), `baseline_not_backfilled` (no baseline yet: `skills baseline
  rebuild <skill>` narrows the list to the real edits),
  `baseline_unrecoverable` (no baseline, and its revision is gone upstream, so
  a rebuild cannot help) or `baseline_suspect` (a library sync conflict on the
  skill is pending: resolve it first). For the last three every file the
  update would change is listed. Approving in the app takes upstream's version
  of those files once and records a new baseline; there is no CLI flag.
- `merge_conflicts`: see the next section.

### Resolving a held merge

The pending merge is kept outside the library (`<app data>/pending-merges/<id>/`,
not synced). Show the user each conflict and ask; never pick a side for them.

```bash
"$SM" --json skills merge status <skill>      # conflicts, choices so far, versions_dir per file
"$SM" --json skills merge resolve <skill> SKILL.md --take local      # keep mine
"$SM" --json skills merge resolve <skill> notes.md --take upstream   # take theirs
"$SM" --json skills merge resolve <skill> notes.md --file fixed.md   # supply resolved content
"$SM" --json skills merge apply <skill>       # finish the update with those choices
"$SM" --json skills merge abort <skill>       # drop it; the library was never touched
```

- `kind` is `text`, `binary`, `deleted_upstream`, `deleted_locally`,
  `added_both`, `base_gone` (the baseline revision is gone upstream, so its
  content cannot be had: choose a version) or `base_unavailable` (the baseline
  content could not be fetched now: update again later, or choose).
- `versions_dir` holds `<name>.mine.<ext>`, `<name>.theirs.<ext>` and, when
  known, `<name>.base.<ext>`. To hand-merge, edit a copy and pass it with
  `--file`; content that still has conflict markers is refused.
- A choice binds the exact two versions it was made for. If upstream moved or
  the library file was edited since, `merge apply` holds again and asks only
  about what changed.
- The app shows "N files merged automatically, M need a decision", with keep
  mine, take theirs, or open both versions for each file.

### Local changes

Against its baseline, every skill reads one of: `clean`, `changed`,
`baseline_unknown` (a git skill with no baseline yet), `baseline_suspect` (a
library sync conflict on it is pending, so its baseline may describe the other
device's content) or `untracked` (a local skill: no upstream, no baseline).
Treat `baseline_suspect` like `baseline_unknown`.

```bash
"$SM" --json skills status <skill>     # baseline {revision, origin, recorded_at, file_count};
                                       # local_changes {state, baseline_revision, files: [{path, state}]}
"$SM" --json skills list --modified    # only edited skills; rows gain local_change_count,
                                       # baseline_known and local_state
"$SM" --json skills diff <skill> --local   # the library copy against its baseline, file by file
```

- File states are `modified`, `added` (not in the baseline) and `deleted` (in
  the baseline, gone from the library).
- `local_change_count` is `null` when the baseline is not known: an unknown
  baseline is not "no changes".
- `diff --local` takes the baseline content from this device's cache, else
  fetches the baseline revision. When neither works, `entries` is `null` and
  `files` still lists the changed paths, with the reason in `message`.
- The app shows the same: a "Your changes" section in the detail panel, an
  "Edited" marker on the card, and a quieter "Baseline unknown" marker.

### Baselines and backfill

```bash
"$SM" --json skills baseline rebuild --all       # skills without a baseline
"$SM" --json skills baseline rebuild <skill>     # one skill, even if it has one
```

- Backfill fetches each skill's recorded source revision and rebuilds the
  manifest. It writes only the baseline files. The report counts `rebuilt`,
  `skipped`, `unreachable` and `failed`, and exits zero when some are
  unreachable. `unreachable` means the server no longer has that revision
  ("not our ref"): retrying will not help. `failed` is any other failure:
  retry later.
- `skipped` covers skills with no recorded source revision. After a backup
  clone every skill on the new device has none, so **run backfill on the
  original device**; the baseline files then travel with the library.
- Each rebuilt skill reports `differing_files`. Backfill trusts the recorded
  revision, which can be older than the content when another device updated
  the skill, so a large count is worth a look.

### Pin and review

**Pin** a git or skills.sh skill to hold it at the revision the user reviewed.
A pinned skill is still checked, but nothing moves it: `skills update <skill>`
refuses with a message naming `skills unpin`, and `--all`, the app's update
buttons and the automatic updater skip it and report it.

```bash
"$SM" --json skills pin <skill> --note "reviewed 2026-11"   # note is optional
"$SM" --json skills unpin <skill>
"$SM" --json skills list --pinned
"$SM" --json skills diff <skill>             # library copy against the latest source (= --upstream)
"$SM" --json skills diff <skill> --commits   # upstream commits since the baseline that touch the skill
```

- `status` reports `pin: { pinned, revision, pinned_at, note }` or `null`.
  `check` carries `pinned: true`; with `update_status: "update_available"` that
  reads "update available (pinned)".
- After `unpin` the next update is an ordinary one. Local and website skills
  have no upstream revision and cannot be pinned.
- `--upstream` returns `{ revision, entries: [{ relative_path, status,
  content_kind, original_text, updated_text, ... }] }`; `original_text` is the
  library copy.
- `--commits` returns `{ base_revision, base_from, remote_revision, subpath,
  reachable, message, commits: [{ sha, date, author, subject }], truncated }`,
  newest first, starting at the baseline revision (`base_from: "baseline"`),
  or the recorded source revision without one. `reachable: false` means the
  base revision is gone or no longer an ancestor; `message` says which. Not an
  error to retry.

### Change notes and patch export

Each locally changed file can carry a one-line reason. When the user asks why
a file was changed, or to record why, use these instead of a side file.

```bash
"$SM" --json skills changes <skill>     # files: [{ path, state, note, note_updated_at }]
"$SM" --json skills changes note <skill> refs/a.md "Windows paths"
"$SM" --json skills changes note <skill> refs/a.md --clear
"$SM" skills changes export <skill> > local.patch
"$SM" --json skills changes export <skill> --output local.patch   # prints a summary
```

- Only a file that differs from a known baseline takes a note. While the
  baseline is unknown or suspect, `note` is refused and stored notes are kept
  under `held_notes`.
- A note drops by itself once its file no longer differs from the baseline
  (reverted, or upstream caught up). It survives merges while the file still
  differs.
- The patch covers changed files only. Its paths start at the upstream
  repository root (`path_prefix` is the skill's directory there), so `git
  apply local.patch` works at the root of a checkout of `baseline_revision`.
  Each note is a `# <path>: <note>` line above its file's `diff --git` header.
- Export needs the baseline content (this device's cache, else a fetch). It
  fails with a message when neither works, when the baseline is unknown or
  suspect, or when there is nothing to export. Don't retry "revision is gone
  upstream".
- In the app: a note button per file in "Your changes", and "Export patch".

### Borrowed skills: watched upstreams and fork

For a skill that will never merge with a source again (a rewrite, or the
user's own skill that borrowed an idea), record where it came from as a
**watched upstream**. It is only checked for movement: nothing is fetched into
the library and nothing is merged. It works for any source type and does not
change the skill's own source.

```bash
"$SM" --json skills watch add <skill> --git-url owner/repo --subpath skills/x --note "lifted the retry loop"
"$SM" --json skills watch add <skill> --git-url https://github.com/o/r/tree/main/skills/x --revision <sha>
"$SM" --json skills watch list <skill> --check      # `moved` per entry; --check resolves the remotes first
"$SM" --json skills watch seen <skill> <watch-id>   # "I have looked": moved clears
"$SM" --json skills watch remove <skill> <watch-id>
"$SM" --json skills fork <skill> --as <new-name>    # a local copy watching the original's git source
```

- Without `--revision`, `watch add` counts the current head as seen. With
  `--subpath`, only commits that change that directory count as movement. An
  unreachable upstream is reported in its entry's `check_error`.
- `skills check` carries `watched_moved` for every skill (and `watched` when it
  watches something), so one `skills check --all` covers updates and borrowed
  sources.
- In the app, "Check all", a single skill's "Check for update" and the
  background auto-updater check watched upstreams too, and a skill card shows
  "Upstream moved" until the user marks it seen. An unreachable upstream never
  fails those checks. The detail panel's "Borrowed from" section can add and
  remove watches (adding needs the location reachable, since its current head
  counts as seen; use the CLI's `--revision` otherwise).
- `fork` refuses a name that is taken. The new skill is `local` with no source
  folder, tags, presets or deployments; the original is untouched. Content is
  copied byte for byte except the `name:` line of the `SKILL.md` frontmatter.
  The watch starts at the original's baseline revision (else its source
  revision); a local, imported or website original has no git source, so its
  fork watches nothing. `skills update --all` reports a fork with an error
  (`missing its original source path`), like any local skill with no source
  folder; that is expected.

### Across devices, and known limitations

Baselines, pins, watched upstreams and notes live in
`.skills-manager/fork-<baseline|pin|watch|notes>.<skill_id>.json` and travel
with the library through Git backup. Nothing is written inside a skill folder.
Never edit or delete these files by hand; removing a skill removes them.

- An edit made on one device survives an update on another: the second device
  reads the baseline from the synced file and fetches the base content it needs.
  On a device that received the library by clone, `skills check` reports
  `unknown` for a skill until it is updated there, because the recorded source
  revision does not sync (the baseline's does).
- When two devices change the same skill's pin, watches or notes at once, the
  later commit wins.
- When one device edits a skill while another updates it, the library sync
  reports a conflict, and until it is resolved the skill reads
  `baseline_suspect`: every update holds and notes are refused. Resolving it
  sets the baseline to match the content kept (keep local: this device's
  earlier baseline; use remote: the other device's; when that cannot be found,
  the baseline is dropped and reads unknown). The next update then neither
  loses the edit nor keeps stale upstream content.
- Pending merges (`<app data>/pending-merges/`) and the base-content cache are
  per device and do not sync.
- **A device running an upstream build of Skills Manager** keeps the fork files
  untouched, but its updates overwrite local edits and ignore pins, and a sync
  then carries the overwrite to the other devices (inferred). Its updates leave
  baselines stale, so upstream's changes read as local changes here until the
  next update on this build. Keep such devices out of a sync group that holds
  adapted skills.
- **First connect by clone.** When an existing library first clones a backup
  remote (or re-clones it), the remote's metadata stays and what only the local
  library has is merged in, file by file. A skill only the local library has
  keeps its library record, source, tags, baseline, pin, notes and watches. A
  local-only preset is kept, and a local preset whose name the remote already
  uses is merged into the remote's. Memberships that involve a local-only skill
  or preset come along. A skill present on both sides ends with the remote's
  record and state (a local baseline, pin, note or watch for it is not
  carried). One whose content differs stops the clone until the app sets the
  local copy aside. Local metadata that cannot be read stops the clone and
  leaves the library as it was. **An upstream build of Skills Manager still
  drops local-only skills on first connect:** their folders stay but lose
  their records. Connect such a device by cloning into an empty library.

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
this path has **no** `held_back_removals` or local-change check and never
merges: edited files are replaced too, nothing is withheld, and nothing asks. `--dry-run` cannot tell you which files are at stake, only that something
differs. Never pass `--force` on the user's behalf — report `content_changed:
true`, say that proceeding overwrites the library copy wholesale, and let them
decide.

## Unmanaged folders in the library

A folder at the library root that no skill record points at is invisible: not
listed, not updated, not deployed. It happens after a first connect by clone on
an older or upstream build, with a folder copied in by hand, or when a metadata
file was lost. `skills orphans` finds them; it only reads.

```bash
# Folders with no record, and nothing else. Never moves, deletes or writes anything.
"$SM" --json skills orphans

# Give one a record in place, as a new local skill (its files are not changed)
"$SM" --json skills orphans adopt <dir_name>

# Every listed folder that has a SKILL.md
"$SM" --json skills orphans adopt --all
```

- Left out of the list: `.git`, `.skills-manager`, every other dot-folder, the
  clone's set-aside folders and whatever the library's `.gitignore` ignores at
  the root. Folders the backup keeps out for their size are still listed.
- A folder without a `SKILL.md` is listed (`has_skill_md: false`) but adoption
  refuses it; add a `SKILL.md` or move it out of the library.
- The skill is named from its `SKILL.md` `name:`, else the folder name — the
  same rule the startup reindex uses, so the name sticks.
- This is not `skills adopt`: that copies skills in from agent directories;
  `orphans adopt` registers a folder already in the library, where it is.

**Say plainly what cannot be recovered.** The adopted skill gets a new id. The
folder's old id, source, baseline, pin, change notes and watched upstreams are
gone for good, and it starts with no tags, presets or deployments (an agent
directory may still hold an old copy or link of it). The adopt report carries
this as `cannot_recover`. To link it to git again use `skills set-source`
(above): the baseline is then upstream's **current** revision, not the one the
folder came from, so which changes were local can no longer be told apart.
set-source refuses when the folder differs from upstream and `--force`
replaces the folder with upstream's copy, so run it with `--dry-run` first and
let the user decide. Afterwards, `skills diff --upstream <skill>` compares the
skill with upstream.

In the app, the skills view shows "N folders in the library are not managed"
when there are any, with a list that adopts one at a time and the same warning.

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

Each run records the upstream revision it judged: `source_revision` (the commit its files came from), `baseline_revision` (the skill's baseline then) and `upstream_revision` (where upstream is now; it differs from `source_revision` once the skill has moved on). `subject` is `library` for the library copy and `incoming` for an update evaluated before applying it.

Evaluate an update before applying it: `skills evaluate <skill> --incoming` judges the version `skills update <skill>` would install (upstream's head, with local changes merged as the update would merge them), staged outside the library and removed afterwards. The library, its git status and the skill's record do not change. It prints the run (the v1 `result`, `subject: "incoming"`, `source_revision` = the incoming commit) plus `incoming` (`revision`, `installed_revision`, `merged`, `kept_local`, `conflicts`: files an update would hold, judged as upstream ships them) and `latest_library`, the newest successful run of the current copy, to compare with. It is a paid run like any other: ask first. The run is kept in `skills evaluations` but never becomes the skill's badge; once the update is applied it stops reading `stale`.

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

Report which skills actually refreshed (`refreshed: true` in the JSON) vs which were already up-to-date. Skills reported with `pinned: true` were skipped on purpose; mention them, and don't unpin them unless the user asks. Skills with `held_back_removals` or `held_back_overwrites` were held for the user's decision; list the paths and say why (see [Reading an update report](#reading-an-update-report)). `--all` never merges local changes: for a skill held with `held_back_reason: "local_changes"`, offer `skills update <skill>`, which does.

## Pitfalls

- **Install succeeded but skill doesn't appear in the agent** → install defaults to library-only. Use `skills deploy <skill> --agent <key>`.
- **Preset membership changed but agent files did not** → membership is organization only. Follow with `presets deploy` or `skills deploy` when the user also asked to make it visible.
- **No active preset** only affects legacy `skills sync` / `presets apply`; additive deploy commands do not require one.
- **Adopted skills can't be `update`d from git** → `npx skills add` and manual `git clone` don't leave source metadata, so adopt has to treat them as `local`. Re-point them with `skills set-source`. Do **not** reach for `adopt --git-url` here: adopt only ever creates new library entries, and it fails *late* — `--dry-run` returns `ok: true` with the skill sitting in `skipped`, and only the real run errors with `--git-url requires exactly one adoptable skill, found 0`. Do **not** remove-then-reinstall either — that drops the skill id, and with it the tags, preset membership and every per-agent deployment.
- Use `--dry-run` before bulk remove, tag delete, preset delete, deploy, or undeploy operations. Use `check` before `update`.
