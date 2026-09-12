# Desktop UI regression scenarios

Run `bulk-agent-layout.json` with the existing `.claude/skills/verify-desktop-ui/scripts/run-scenarios.mjs` harness (see that skill for runtime prerequisites). Use the real frontend and an isolated API HOME/XDG environment; never use your normal agent configuration.

Fixture: at least two available agents with different display-name lengths (for example Claude Code and OpenAI Codex). Create `.claude/skills` and `.codex/skills` inside the isolated HOME, and put this `.claude.json` there:

```json
{
	"mcpServers": {
		"heroui-review-fixture": { "command": "echo", "args": ["fixture"] }
	}
}
```

The scenarios support English and Traditional Chinese UI labels. Start Vite at the CORS-allowed `http://localhost:1420`, set `API_PORT` to your isolated API port, and run:

```sh
node .claude/skills/verify-desktop-ui/scripts/run-scenarios.mjs scripts/ui-scenarios/bulk-agent-layout.json --out /tmp/aghub-bulk-layout
```

The scenarios select the fixture and open Manage agents without applying changes. They throw unless at least two rows exist, Content fills each row's inner width, controls are inside Content, and checkbox left edges/count right edges align at 1024px and 1400px. Check screenshots and console errors as required by the harness. Removing `w-full` from the bulk dialog's Checkbox.Content must make both scenarios fail.
