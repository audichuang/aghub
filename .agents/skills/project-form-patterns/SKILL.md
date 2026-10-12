---
name: project-form-patterns
description: House rules for forms in aghub's desktop app (crates/desktop/src) — HeroUI v3 fields under react-hook-form, validationBehavior, FieldError vs ErrorMessage, key/value and other custom editors. Use when building or refactoring MCP forms, settings dialogs or create/edit panels, not for general HeroUI component APIs (heroui-react).
---

# AGHub Form Patterns

## Source of truth

When a HeroUI form API is in question, check the official v3 docs, not memory:
`https://v3.heroui.com/docs/react/components/{form,text-field,field-error,select}`.
Never assume a prop exists — `TextField` has no `errorMessage` prop (the
`errorMessage` props in this repo belong to our own components).

## Stack

- `react-hook-form` owns state, rules and submit blocking. One form state: no
  parallel `validationErrors` state for the same fields.
- `Controller` for HeroUI `Select` and every custom widget (`AgentSelector`,
  `EnvEditor`, `HttpHeaderEditor`, `KeyPairEditor`).
- Every HeroUI field under RHF gets `validationBehavior="aria"`. The native
  default steals focus and the submit flow and bypasses the RHF error UI.

## Errors

Field anatomy:

```tsx
<TextField isInvalid={Boolean(fieldState.error)} validationBehavior="aria">
	<Label>Name</Label>
	<Input {...inputProps} />
	{fieldState.error && <FieldError>{fieldState.error.message}</FieldError>}
</TextField>
```

**`FieldError` renders nothing outside an invalid field.** It reads the field's
`FieldErrorContext` (provided by TextField, Select, ComboBox, NumberField,
Checkbox, RadioGroup, Switch, date fields…) and returns `null` without it —
`TagGroup` and a plain `<div>` provide none. So composite/custom editors
(key/value editors, tag selectors) must use HeroUI `ErrorMessage`, as
`key-pair-editor.tsx` does. Note `agent-selector.tsx` currently renders a
`FieldError` outside any field — that message cannot appear.

## Custom and key/value editors

- Validate from the current field value and show ONE aggregated `ErrorMessage`
  below the whole editor, not a hand-rolled `<p>`.
- Key/value rows stay two inputs + one delete button, no extra wrappers that
  change flex behaviour. Do not push per-row errors into that single-line row;
  if row-level errors are needed, redesign the layout first.

## Practical

- HeroUI buttons: `onPress`; non-submit buttons inside a form: `type="button"`.
- Do not restyle a form while adding validation.
- Verify with `bun run typecheck` in `crates/desktop` (`bun run build` for a
  full tsc + vite pass); separate pre-existing failures from yours.
