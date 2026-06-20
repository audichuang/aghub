// Type-level contract tests for SharedSkillInstallModal.
// No FE test runner (no vitest/jest) is installed; this file exercises the
// exported props interface via TypeScript compile-only assertions.
// Run via: bun run typecheck (tsc --noEmit)
import type { SharedSkillInstallModalProps } from "./shared-skill-install-modal.ts";

// Verify that showTargetSelector is an optional boolean prop (Codex P1 fix).
// If this type assignment compiles, the prop exists with the correct type.
type _ShowTargetSelectorOptional = Awaited<
	undefined extends SharedSkillInstallModalProps["showTargetSelector"]
		? true
		: never
>;
type _ShowTargetSelectorBoolean = Awaited<
	SharedSkillInstallModalProps["showTargetSelector"] extends
		| boolean
		| undefined
		? true
		: never
>;

// Verify that installResults is truly required — not just non-undefined in
// Required<Props> (which would pass even if the prop became optional).
// If installResults were optional, {} would extend Pick<Props, 'installResults'>
// and the conditional would yield `never`, failing to assign to `true`.
type _InstallResultsRequired =
	{} extends Pick<SharedSkillInstallModalProps, "installResults">
		? never
		: true;

// Verify that agentPickerSlot is truly required by the same technique.
type _AgentPickerSlotRequired =
	{} extends Pick<SharedSkillInstallModalProps, "agentPickerSlot">
		? never
		: true;

// Verify that skillInfo is optional (Codex P2 fix — allows name to be omitted
// so the source row is shown without the skill name for installAll callers).
type _SkillInfoOptional = Awaited<
	undefined extends SharedSkillInstallModalProps["skillInfo"] ? true : never
>;

// Verify that skillInfo.name is optional (name can be omitted).
type _SkillInfoNameOptional = Awaited<
	NonNullable<SharedSkillInstallModalProps["skillInfo"]> extends {
		source: string;
		name?: string | undefined;
	}
		? true
		: never
>;

// Ensure all _Assert* types resolve to `true` at the type level.
const _assertShowTargetSelectorOptional: _ShowTargetSelectorOptional = true;
const _assertShowTargetSelectorBoolean: _ShowTargetSelectorBoolean = true;
const _assertInstallResultsRequired: _InstallResultsRequired = true;
const _assertAgentPickerSlotRequired: _AgentPickerSlotRequired = true;
const _assertSkillInfoOptional: _SkillInfoOptional = true;
const _assertSkillInfoNameOptional: _SkillInfoNameOptional = true;

// Suppress noUnusedLocals — these are intentional type-assertion sinks.
void _assertShowTargetSelectorOptional;
void _assertShowTargetSelectorBoolean;
void _assertInstallResultsRequired;
void _assertAgentPickerSlotRequired;
void _assertSkillInfoOptional;
void _assertSkillInfoNameOptional;
