# Private project skill grants

A Master in `.aghub` must not grant a skill by itself. A supported private
Referrer directory is preferred over a vendor's default shared directory.
Shared read compatibility remains necessary to discover old installs.

| Agent       | Project write directory | Evidence                                                                                                                                                              |
| ----------- | ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Codex       | `.codex/skills`         | Local codex-cli 0.154.0 `app-server` → `skills/list` found an inert fixture here as `scope: repo`, `enabled: true`; `.agents/skills` was a separate positive control. |
| Antigravity | `.agent/skills`         | [Vendor documents supported alias](https://antigravity.google/docs/skills).                                                                                           |
| Gemini      | `.gemini/skills`        | [Vendor discovery locations](https://geminicli.com/docs/cli/using-agent-skills/).                                                                                     |
| Cline       | `.cline/skills`         | [Vendor recommended location](https://docs.cline.bot/customization/skills); `.clinerules/skills` remains a native compatibility read path.                            |
| Copilot     | `.github/skills`        | Existing supported read path; [vendor skills documentation](https://docs.github.com/en/copilot/concepts/agents/about-agent-skills).                                   |
| Kimi CLI    | `.kimi/skills`          | [Kimi CLI brand group](https://moonshotai.github.io/kimi-cli/en/customization/skills.html); this descriptor targets kimi-cli, not the separate kimi-code product.     |
| Warp        | `.warp/skills`          | [Vendor project locations](https://docs.warp.dev/agents/capabilities/skills/).                                                                                        |

This change targets project grants; global write directories are unchanged.
Amp still uses its existing shared slot. It is outside this optimization.

## Migration

`repair` first grants private Referrers to existing readers. It preserves a
shared slot while it remains another supported agent's write slot. A user who
explicitly excludes that agent may retire the shared Referrer only after all
retained readers have verified private Referrers to the same Master. Do not
remove a real authored directory or a foreign link as shared-slot cleanup.

A surviving shared Referrer still grants to all its readers. Merely changing
descriptors or creating private links does not provide isolation until that
shared entry is retired. Native cross-agent compatibility settings outside
aghub's modeled paths can also expose skills; directory grants are not a
filesystem security boundary.

## Verification

`project_private_grants_can_be_removed_independently` installs through the real
ConfigManager for every changed agent, checks the holder set and absence of a
shared entry, then removes that agent while preserving Claude and the Master.
Reverting Codex's write path makes that test fail at the shared-entry assertion.
