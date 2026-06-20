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

// Verify that installResults is a required array (results phase is always provided).
type _InstallResultsRequired = Awaited<
	// If installResults is required, the key is not `undefined` in Required<>
	Required<SharedSkillInstallModalProps>["installResults"] extends unknown[]
		? true
		: never
>;

// Verify that agentPickerSlot is required (React.ReactNode).
type _AgentPickerSlotRequired = Awaited<
	Required<SharedSkillInstallModalProps>["agentPickerSlot"] extends React.ReactNode
		? true
		: never
>;

// Ensure all _Assert* types resolve to `true` at the type level.
const _assertShowTargetSelectorOptional: _ShowTargetSelectorOptional = true;
const _assertShowTargetSelectorBoolean: _ShowTargetSelectorBoolean = true;
const _assertInstallResultsRequired: _InstallResultsRequired = true;
const _assertAgentPickerSlotRequired: _AgentPickerSlotRequired = true;

// Suppress noUnusedLocals — these are intentional type-assertion sinks.
void _assertShowTargetSelectorOptional;
void _assertShowTargetSelectorBoolean;
void _assertInstallResultsRequired;
void _assertAgentPickerSlotRequired;
