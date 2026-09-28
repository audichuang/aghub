# Antigravity descriptor notes

Research and known costs behind the skill paths in
`crates/agents/src/agents/antigravity.rs`.

## Global skill dirs

Antigravity's vendor docs moved the global customization root to
`~/.gemini/config/` — skills live at `~/.gemini/config/skills/<name>/SKILL.md`,
and that dir is shared by Antigravity 2.0, the IDE and the CLI. It is the WRITE
dir and goes FIRST: `load_skills_from_dirs` dedups first-dir-wins and the
winner becomes `source_path`, i.e. the path `remove_skill` deletes and `check`
hashes.

Two older dirs stay READ-ONLY so nothing a shipped aghub installed is stranded:

- `.gemini/antigravity/skills` — the IDE-1.x path npx `agents.ts` still names,
  and what aghub wrote up to v2.18.x.
- `.gemini/antigravity-cli/skills` — documented by the CLI's plugin page.

Decision #12 in `docs/specs/2026-08-30-skills-hub-borrow-path.md` was revised
on 2026-09-06 with the evidence; #11 still holds — all three are Antigravity's
own dirs, never another agent's private one.

## Known costs of the read-only legacy dirs

Both are pinned by tests rather than left to be rediscovered. Both beat the
alternative, which was not reading the dirs and stranding the skills outright.

- **`doctor --verify-links` inspects the WRITE slot only**, so a skill an older
  release installed into `.gemini/antigravity/skills` audits as `withheld` even
  though Antigravity really does read it. `aghub repair [name] --yes` clears
  it: `repair` plans WRITE dirs so it never sees the compat dir, but
  `readers_of` asks the READ paths, so the stranded skill puts antigravity in
  `grant_to` and its empty write slot is planned `Create`. `aghub add` is NOT
  the way — the skill already loads, so both its branches refuse
  `resource_exists`. The test
  (`repair.rs::repair_relinks_a_skill_stranded_in_a_read_only_compat_dir`)
  exercises the PROJECT twin `.agent/skills`, which needs no real home; the
  global dirs run the same two functions on the same 2-state.
- **A Referrer parked in one of those dirs cannot be removed for antigravity
  ALONE** — the planner schedules only the write dir, so `delete --yes` answers
  `outcome: kept` with the file in place
  (`npx_skill_path_ownership.rs::a_referrer_in_a_read_only_compat_dir_…`).
  `repair` DETACHES the common instance of this: a stale LINK the write slot
  already covers (`ReferrerAction::Unlink`). What is left is a real DIRECTORY
  or a link to somebody else's content, which repair must not move — and the
  refusal names the path, so it is a hand fix with an address instead of a dead
  end.
