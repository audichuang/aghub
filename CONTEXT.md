# aghub

aghub manages AI coding agent configurations — MCP servers, skills, and sub-agents — across many agents, through a CLI, an HTTP API, and a desktop app. This glossary fixes the language that recurs across those surfaces.

## Language

### Skill hashing & locks

**Source hash**:
The SHA-256 aghub computes over a skill's source folder, used to decide whether an installed skill is up to date. It is stored under different keys depending on the lock format (the global lock's `contentHash`, the project lock's `computedHash`), but it is one concept.
_Avoid_: "content hash" / "computed hash" when speaking conceptually — those are the storage keys, not the term.

**Skill folder hash**:
The upstream GitHub tree SHA for a skill folder, written by the npx `skills` toolchain. In aghub's own global lock it is deliberately kept empty; the Source hash supersedes it. Setting a Source hash and clearing the Skill folder hash always happen together — they are never both populated.
_Avoid_: conflating with Source hash.

**Lock entry**:
One skill's record in a lock file. The global lock (npx-compatible v3) and the project lock (v1, intentionally timestamp-free to avoid merge conflicts) carry different fields for the same skill.
_Avoid_: "lock row".

### Skill layout & install

**Universal install**:
A layout where a skill lives once as a shared Master and each agent's skills directory holds a symlink pointing at it. Contrast with an Isolated-copy install, where every agent gets its own independent copy and there is no Master.
_Avoid_: "symlink mode" / "linked skill" as the canonical name.

**Master**:
The single `.agents/skills/<name>` directory that a Universal install's per-agent symlinks resolve to. Renaming or removing it is the operation that must account for every Referrer.
_Avoid_: "canonical dir" (that is the storage key), "source".

**Referrer**:
An agent's skills entry that is a symlink resolving to a Master. When the Master is renamed or removed, its Referrers must be re-pointed or pruned.
_Avoid_: "link", "alias".

**Relink**:
Re-pointing a Master's Referrers after the Master moves: unlink the old-name symlinks and recreate symlinks at the new name. A failed Relink leaves dangling Referrers and is the failure a transactional rename must roll back.
