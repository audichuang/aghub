# Disabled agents, rosters and the real-directory keep rule

Read before changing which agents a flow reads or writes, or what removal keeps.

**Disabled agents** (`agent_settings.rs`, persisted under `paths::app_data_dir()`, set via the API's `/agents/disabled`): aghub does not manage them. Stored as an ALLOW-list, so an agent the saved choice never named (a later release's) is off by default. A missing file means "never chose" and manages everything; the desktop's first run seeds it with only the agents detected then. Lib unit tests read a per-thread override (`agent_settings::test_override::disable`), never the real file.

Three principles decide which roster a question uses:

1. **A flow that picks its OWN agent set and writes to it uses the managed set** — `load_managed_agents` for a writing scan; `plan_repair` leaves a slot only disabled agents use; `plan_removal`'s `all_agents` skips dirs only they read.
2. **Naming an agent is consent** — a disabled agent named in the request is acted on (`unmanaged_skill_dirs(.., requested)`).
3. **"Does anyone still hold this?" uses the FULL roster** (`load_all_agents`, `AgentType::ALL`) — a disabled agent is unmanaged, not absent: it keeps a Master alive, `--all-agents` refuses while its Referrer still holds the Master, and transfer's delete ordering counts it (`slot_reader_count`; slot sharing is structural, and filtering it ties shared and private slots so a valid reconcile fails preflight).

Two places where principle 3 is easy to misapply:

- "Would an UNSELECTED reader lose this shared slot" (`skill_dir_readers_outside`, single-agent / comma-list delete) does not count disabled agents, so an unmanaged agent never names or blocks such a delete.
- Its "is anyone besides the initiator here" shortcut, `unselected_reader_needs_referrer`, DOES count the full roster: the initiator may itself be disabled (`-a <disabled>`), and filtering it would delete a shared Referrer and Master an enabled reader still uses.

A real directory (not a link) is decided by `single_agent_keep_reason(dir, .., scope, requested)` for every surface (CLI, API by-name and by-path), in this order:

- inside `.aghub` → kept;
- any inbound symlink → kept, EXCEPT a link in a requested agent's own private skills dir (that agent's row unlinks it, so the verdict cannot depend on `-a` order; the releasing row plans those links via `plan_owned_inbound_links`, and a requested agent with its OWN link into the directory is released by the same rule before the symlink planner runs);
- any ENABLED reader outside `requested` → kept, including a private directory co-read through a dotfiles-style symlinked skills root (empty `requested` fails closed only inside shared roots);
- a shared-root real directory git TRACKS → kept (`KeepReason::GitTracked`, via `removal::shared_slot_git_keep` over `shape::git_tracked`), and one git cannot answer for → kept (`KeepReason::GitUndecided`, fails closed like `repair`; the probe has a 10s deadline). `--all-agents` asks the SAME `shared_slot_git_keep` per copy — never a second spelling.

Naming every enabled reader deletes it, and since no Master backs it the content and its lock entry are gone for good (`source sync` can only reinstall a fresh copy of the source's version). `reconcile` and the desktop dialogs obey the same rule because they remove through `remove_skill_batch` with their agent set as `requested`; callers that pass no requested set (rename preflight) stay strict. Why: `docs/history/core-removal.md#disabled-agent-blocked-a-single-agent-delete`.
