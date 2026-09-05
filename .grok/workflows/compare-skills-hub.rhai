let meta = #{
    name: "compare-skills-hub",
    description: "Deep-compare sibling skills-hub vs aghub and rank borrowable optimizations",
    when_to_use: "When comparing skills-hub to aghub for product, UX, agent coverage, or skill-lifecycle optimizations",
    phases: [
        #{ title: "Survey", detail: "six dimensions across both codebases" },
        #{ title: "Synthesize", detail: "ranked opportunities for aghub" },
        #{ title: "Verify", detail: "adversarial check against aghub invariants" },
        #{ title: "Report", detail: "Traditional Chinese comparison report" },
    ],
};

let survey_schema = #{
    "type": "object",
    "properties": #{
        "dimension": #{ "type": "string" },
        "skills_hub_summary": #{ "type": "string" },
        "aghub_summary": #{ "type": "string" },
        "skills_hub_strengths": #{ "type": "array", "items": #{ "type": "string" } },
        "aghub_strengths": #{ "type": "array", "items": #{ "type": "string" } },
        "borrowable": #{
            "type": "array",
            "maxItems": 6,
            "items": #{
                "type": "object",
                "properties": #{
                    "idea": #{ "type": "string" },
                    "evidence": #{ "type": "string" },
                    "conflict": #{ "type": "string" },
                },
            },
        },
        "do_not_copy": #{ "type": "array", "items": #{ "type": "string" } },
    },
};

let opportunities_schema = #{
    "type": "object",
    "properties": #{
        "product_positioning": #{ "type": "string" },
        "opportunities": #{
            "type": "array",
            "maxItems": 8,
            "items": #{
                "type": "object",
                "properties": #{
                    "id": #{ "type": "string" },
                    "title": #{ "type": "string" },
                    "why": #{ "type": "string" },
                    "skills_hub_evidence": #{ "type": "string" },
                    "aghub_gap": #{ "type": "string" },
                    "invariant_risk": #{ "type": "string" },
                    "how_to_fit": #{ "type": "string" },
                    "effort": #{ "type": "string" },
                    "impact": #{ "type": "string" },
                },
            },
        },
        "rejected": #{
            "type": "array",
            "maxItems": 8,
            "items": #{
                "type": "object",
                "properties": #{
                    "idea": #{ "type": "string" },
                    "reason": #{ "type": "string" },
                },
            },
        },
    },
};

let verdict_schema = #{
    "type": "object",
    "properties": #{
        "keep": #{ "type": "boolean" },
        "priority": #{ "type": "string" },
        "reason": #{ "type": "string" },
        "evidence": #{ "type": "string" },
        "fit_notes": #{ "type": "string" },
    },
};

let report_schema = #{
    "type": "object",
    "properties": #{
        "markdown": #{ "type": "string" },
        "headline": #{ "type": "string" },
    },
};

fn field_str(obj, key) {
    if obj == () { return ""; }
    let v = obj[key];
    if v == () { return ""; }
    if type_of(v) == "string" { return v; }
    json_encode(v)
}

fn join_lines(arr) {
    if arr == () { return ""; }
    if type_of(arr) != "array" { return json_encode(arr); }
    let out = "";
    for item in arr {
        out += "- ";
        if type_of(item) == "string" {
            out += item;
        } else {
            out += json_encode(item);
        }
        out += "\n";
    }
    out
}

fn survey_base(dimension) {
    let p = "";
    p += "You are surveying TWO sibling local codebases. Read files with read_file, grep, and list_dir. Do not answer from memory. An empty list is valid only after you have opened the listed files.\n\n";
    p += "aghub: /home/audichuang/research/aghub\n";
    p += "skills-hub: /home/audichuang/research/skills-hub\n";
    p += "Date: 2026-08-30.\n\n";
    p += "DIMENSION: ";
    p += dimension;
    p += "\n\nHard product facts (verify, do not contradict unless files prove otherwise):\n";
    p += "- aghub is a Rust workspace: CLI + HTTP API + Tauri desktop. It manages MCP, skills, sub-agents, inference, Claude Code plugins, remote SSH. Skills are symlink-only to a shared Master at .agents/skills. It stays round-trip compatible with npx `skills` lock files. It reads real agent config files; ConfigManager is the mutation seam. 25 agents in AgentType::ALL (crates/agents/src/models.rs). Adding an agent without wiring registry::get() silently falls back to Claude.\n";
    p += "- skills-hub is a Tauri desktop-only app. SQLite skill_store, central repo default ~/.skillshub, 47 built-in tool adapters for SKILLS ONLY, symlink/junction/copy fallback, custom tool directories, tags, bulk ops, scheduled OS auto-update, Explore (featured + GitHub search), discovery import. No MCP/plugins/inference/CLI.\n";
    p += "- Do NOT propose replacing aghub's file-as-SSOT with SQLite, dropping the npx lock contract, inventing unattested agent skill dirs, or making copy-install the default.\n";
    p += "- Cite evidence as path:symbol or path:line. Return JSON matching the schema.\n";
    p
}

let focus = "all";
if args != () && args.focus != () && type_of(args.focus) == "string" {
    focus = args.focus;
}

phase("Survey");
log("Surveying skills-hub vs aghub across six dimensions (focus=" + focus + ")");

let extra_product = "";
extra_product += "Read these first:\n";
extra_product += "- /home/audichuang/research/skills-hub/README.md\n";
extra_product += "- /home/audichuang/research/skills-hub/AGENTS.md\n";
extra_product += "- /home/audichuang/research/aghub/README.md\n";
extra_product += "- /home/audichuang/research/aghub/AGENTS.md\n";
extra_product += "- /home/audichuang/research/aghub/CONTEXT.md\n";
extra_product += "Then sample desktop entry points: skills-hub src/App.tsx, src/components/Layout.tsx, src/components/skills/*.tsx; aghub crates/desktop/src/App.tsx, pages/, components/app-sidebar.tsx, pages/settings/skills.tsx, pages/skills-sh/.\n";
extra_product += "Compare user journeys: install, organize, update, disable, discover. List unique surfaces each product has. Focus=";
extra_product += focus;
extra_product += ".\n";

let extra_lifecycle = "";
extra_lifecycle += "Read these first:\n";
extra_lifecycle += "- skills-hub: src-tauri/src/core/installer.rs, sync_engine.rs, central_repo.rs, content_hash.rs, git_fetcher.rs, skill_store.rs, auto_update.rs, system_scheduler.rs\n";
extra_lifecycle += "- aghub: crates/core/AGENTS.md (mutation lock + testing), crates/core/src/skills/ (manager, linker, removal), crates/skill/, crates/skill-update/, docs/specs/2026-06-19-symlink-only-skill-install.md, docs/specs/2026-06-20-skills-sources-unification.md, CONTEXT.md (Master/Referrer/Resync)\n";
extra_lifecycle += "Compare: where the canonical skill lives, how hash/update works, enable/disable without delete, scheduled update, lock files vs SQLite, overwrite policy, Windows junction/copy fallback vs symlink-only.\n";

let extra_coverage = "";
extra_coverage += "Read these first:\n";
extra_coverage += "- skills-hub/src-tauri/src/core/tool_adapters/mod.rs (ToolId, path table, custom tools)\n";
extra_coverage += "- skills-hub README supported tools table\n";
extra_coverage += "- aghub/crates/agents/src/models.rs AgentType::ALL\n";
extra_coverage += "- aghub/crates/agents/src/agents/ listing + a few descriptors\n";
extra_coverage += "- aghub/crates/core/src/registry/mod.rs\n";
extra_coverage += "- aghub/crates/desktop/src/pages/settings/custom-agents.tsx (likely a stub)\n";
extra_coverage += "- skills-hub docs/releases/v0.7.0/tool-management-and-custom-tools.md and v0.8.0/custom-tool-management.md if present\n";
extra_coverage += "Produce a coverage gap list: tools skills-hub has that aghub lacks (skill-path only), tools aghub has that skills-hub lacks, and whether each extra tool would need a full AgentDescriptor (MCP/sub-agents) or a skill-path-only descriptor. Check custom-tool vs custom-agent.\n";

let extra_ux = "";
extra_ux += "Read these first:\n";
extra_ux += "- skills-hub: ExplorePage.tsx, SkillsList.tsx, SkillCard.tsx, TagsPage.tsx, ToolsPage.tsx, UpdatesPage.tsx, SettingsPage.tsx, FilterBar.tsx, SkillDetailView.tsx, modals/ (bulk, discovery, import), i18n/resources.ts (scan feature keys), docs/UI-DESIGN-GUIDELINES.md (skim)\n";
extra_ux += "- aghub: crates/desktop/src/pages/settings/skills.tsx, skill-list.tsx, skill-detail.tsx, pages/skills-sh/, components/multi-select-floating-bar.tsx, import-* panels, onboarding-controller.tsx, lib/locales/\n";
extra_ux += "Compare: tags, card/list views, explore/featured/GitHub search vs skills.sh, bulk tag/tool/enable, scheduled update UI, discovery banner, bilingual i18n completeness, skill file-tree preview, install-scope picker at install time.\n";

let extra_arch = "";
extra_arch += "Read these first:\n";
extra_arch += "- skills-hub AGENTS.md architecture section, src-tauri/src/lib.rs, commands/mod.rs (command list), core/mod.rs\n";
extra_arch += "- aghub AGENTS.md crate map, crates/api/src/lib.rs, crates/cli/, crates/desktop/src-tauri, crates/core/src/adapters/mod.rs vs adapter.rs\n";
extra_arch += "Compare: IPC-only vs API+CLI+desktop, SQLite vs reading real configs, test surface, seams (ConfigManager vs SkillStore), how much UI talks past the backend. Depth: is skills-hub's SkillStore a deep module? Is aghub's skill mutation split across too many crates?\n";

let extra_ops = "";
extra_ops += "Read these first:\n";
extra_ops += "- skills-hub: network_proxy.rs, onboarding.rs, github_search.rs, skills_search.rs, featured_skills.rs, cache_cleanup.rs, docs/releases/v0.7.1/unified-network-proxy.md, v0.9.0/discovery-scan-settings.md, v0.7.0/auto-update-optimizations.md\n";
extra_ops += "- aghub: crates/git/, crates/skills-sh/, crates/api credentials, docs/specs/2026-07-17-skill-fetch-partial-github-rest.md, doctor CLI, source check/apply-update\n";
extra_ops += "Compare: GitHub search, proxy, token handling, discovery of unmanaged skills, background scheduled updates while app is closed, cache cleanup, marketplace (skills.sh vs featured JSON).\n";

let jobs = [];
jobs.push(#{
    prompt: survey_base("product-surface") + extra_product,
    label: "survey:product",
    capability_mode: "read-only",
    output_schema: survey_schema,
});
jobs.push(#{
    prompt: survey_base("skill-lifecycle") + extra_lifecycle,
    label: "survey:lifecycle",
    capability_mode: "read-only",
    output_schema: survey_schema,
});
jobs.push(#{
    prompt: survey_base("agent-coverage") + extra_coverage,
    label: "survey:coverage",
    capability_mode: "read-only",
    output_schema: survey_schema,
});
jobs.push(#{
    prompt: survey_base("desktop-ux") + extra_ux,
    label: "survey:ux",
    capability_mode: "read-only",
    output_schema: survey_schema,
});
jobs.push(#{
    prompt: survey_base("architecture") + extra_arch,
    label: "survey:arch",
    capability_mode: "read-only",
    output_schema: survey_schema,
});
jobs.push(#{
    prompt: survey_base("ops-discovery-update") + extra_ops,
    label: "survey:ops",
    capability_mode: "read-only",
    output_schema: survey_schema,
});

let survey_results = parallel(jobs);

let surveys = [];
let i = 0;
for r in survey_results {
    if r != () && r.success && r.output != () {
        surveys.push(r.output);
    } else {
        log("survey slot " + i.to_string() + " failed or empty");
    }
    i += 1;
}

if surveys.len() == 0 {
    complete(#{ summary: "All survey agents failed; no comparison produced.", confirmed: [] });
}

phase("Synthesize");
log("Synthesizing ranked opportunities from " + surveys.len().to_string() + " surveys");

let synth_prompt = "";
synth_prompt += "You synthesize a ranked list of optimizations aghub can borrow from skills-hub. You must use the survey JSON below; also re-open any cited file if a claim looks thin.\n\n";
synth_prompt += "aghub: /home/audichuang/research/aghub\n";
synth_prompt += "skills-hub: /home/audichuang/research/skills-hub\n";
synth_prompt += "Focus: ";
synth_prompt += focus;
synth_prompt += "\n\nInvariants you must not break:\n";
synth_prompt += "1. Skill install stays symlink-only to the shared Master (.agents/skills). Copy/junction is not the default.\n";
synth_prompt += "2. npx skills lock round-trip (global v3 / project v1). Do not replace locks with SQLite as SSOT.\n";
synth_prompt += "3. aghub reads real agent config files; ConfigManager is the mutation seam. Do not invent a parallel skill database as source of truth.\n";
synth_prompt += "4. Do not invent unattested agent skill/MCP paths. registry::get() unknown id silently becomes Claude.\n";
synth_prompt += "5. enable/disable on CLI is MCP-only today; skills enable is dead. Do not pretend otherwise.\n";
synth_prompt += "6. aghub already has MCP, plugins, inference, remote SSH, CLI, doctor, coverage, sources. Do not recommend those as gaps.\n";
synth_prompt += "7. custom-agents.tsx in aghub desktop looks like a stub — verify before claiming a feature exists.\n\n";
synth_prompt += "Return at most 8 opportunities that would actually improve aghub. Each must say how it fits WITHOUT violating invariants (how_to_fit). Put ideas that would break invariants into rejected[] with a reason.\n";
synth_prompt += "Prefer high-leverage product gaps (agent coverage, custom skill dirs, scheduled check, discovery import, tags, explore UX, skill disable-without-delete) over restyling.\n";
synth_prompt += "effort is S/M/L. impact is high/medium/low.\n\n";
synth_prompt += "SURVEY JSON:\n";
synth_prompt += json_encode(surveys);

let synth = agent(synth_prompt, #{
    label: "synthesize",
    capability_mode: "read-only",
    output_schema: opportunities_schema,
});

let opportunities = [];
let rejected = [];
let positioning = "";
if synth != () && synth.success && synth.output != () {
    positioning = field_str(synth.output, "product_positioning");
    if synth.output.opportunities != () && type_of(synth.output.opportunities) == "array" {
        opportunities = synth.output.opportunities;
    }
    if synth.output.rejected != () && type_of(synth.output.rejected) == "array" {
        rejected = synth.output.rejected;
    }
}

if opportunities.len() > 8 {
    log("truncating opportunities from " + opportunities.len().to_string() + " to 8");
    let kept = [];
    let n = 0;
    for o in opportunities {
        if n < 8 { kept.push(o); }
        n += 1;
    }
    opportunities = kept;
}

log("candidates=" + opportunities.len().to_string() + " rejected=" + rejected.len().to_string());

let confirmed = [];
if opportunities.len() > 0 {
    phase("Verify");
    log("Adversarially verifying " + opportunities.len().to_string() + " opportunities");

    let vjobs = [];
    for o in opportunities {
        let title = field_str(o, "title");
        let vp = "";
        vp += "Adversarially verify this proposed aghub optimization borrowed from skills-hub. Default keep=false unless you independently open the cited files and confirm BOTH (a) skills-hub actually has it and (b) aghub actually lacks it or is materially weaker AND (c) it can fit without breaking aghub invariants.\n\n";
        vp += "aghub: /home/audichuang/research/aghub\n";
        vp += "skills-hub: /home/audichuang/research/skills-hub\n\n";
        vp += "Invariants: symlink-only skills; npx lock contract; ConfigManager + real config files as SSOT (no SQLite skill SSOT); no invented agent paths; CLI enable/disable is MCP-only; do not drop CLI/API.\n\n";
        vp += "CANDIDATE JSON:\n";
        vp += json_encode(o);
        vp += "\n\nOpen the evidence files. If aghub already has an equivalent (maybe under another name), keep=false. If the idea requires copy-install, a parallel DB, or fake agent dirs, keep=false. priority is P0/P1/P2/P3/drop. evidence must include paths you opened.\n";
        vjobs.push(#{
            prompt: vp,
            label: "verify:" + title,
            capability_mode: "read-only",
            output_schema: verdict_schema,
        });
    }

    let verdicts = parallel(vjobs);
    let vi = 0;
    for v in verdicts {
        let orig = opportunities[vi];
        if v != () && v.success && v.output != () && v.output.keep == true {
            if v.output.evidence != () && v.output.evidence != "" {
                let pri = field_str(v.output, "priority");
                if pri != "drop" {
                    confirmed.push(#{
                        opportunity: orig,
                        priority: pri,
                        reason: field_str(v.output, "reason"),
                        evidence: field_str(v.output, "evidence"),
                        fit_notes: field_str(v.output, "fit_notes"),
                    });
                }
            } else {
                log("dropped candidate with keep=true but empty evidence: " + field_str(orig, "title"));
            }
        } else {
            log("rejected by skeptic: " + field_str(orig, "title"));
        }
        vi += 1;
    }
    log(confirmed.len().to_string() + "/" + opportunities.len().to_string() + " survived verification");
}

phase("Report");

let report_prompt = "";
report_prompt += "Write a deep comparison report in Traditional Chinese (Taiwan). Markdown only in the markdown field. Be specific: file paths, not vibes. The audience is the aghub maintainer deciding what to steal from skills-hub.\n\n";
report_prompt += "aghub: /home/audichuang/research/aghub (this fork of AkaraChen/aghub, plus npx skills contract).\n";
report_prompt += "skills-hub: /home/audichuang/research/skills-hub (Tauri skills manager, 47 tools, central repo).\n\n";
report_prompt += "Structure the markdown as:\n";
report_prompt += "1. 一句話定位差異\n";
report_prompt += "2. 產品對照表（功能面：skills-hub 有 / aghub 有 / 兩邊都有但形狀不同）\n";
report_prompt += "3. 架構與不變量（為什麼不能整套搬）\n";
report_prompt += "4. 已驗證、可做的優化（P0-P3），每項含：現況、skills-hub 做法、怎麼接到 aghub 而不破壞 symlink-only / npx lock / ConfigManager、粗估工作量\n";
report_prompt += "5. 明確不要搬的東西與原因\n";
report_prompt += "6. 建議的實作順序（3-5 步，不要大爆炸重寫）\n\n";
report_prompt += "If a verified list is empty, still write the comparison from surveys and explain why nothing survived.\n";
report_prompt += "headline: one Traditional Chinese sentence.\n\n";
report_prompt += "POSITIONING:\n";
report_prompt += positioning;
report_prompt += "\n\nSURVEYS:\n";
report_prompt += json_encode(surveys);
report_prompt += "\n\nVERIFIED OPPORTUNITIES:\n";
report_prompt += json_encode(confirmed);
report_prompt += "\n\nSYNTH REJECTED:\n";
report_prompt += json_encode(rejected);

let report = agent(report_prompt, #{
    label: "report",
    capability_mode: "read-only",
    output_schema: report_schema,
});

let markdown = "";
let headline = "";
if report != () && report.success && report.output != () {
    markdown = field_str(report.output, "markdown");
    headline = field_str(report.output, "headline");
}
if markdown == "" {
    markdown = "# skills-hub vs aghub\n\n報告寫入失敗。已驗證機會數：" + confirmed.len().to_string() + "\n";
}

let path = write_scratch_file("skills-hub-vs-aghub.md", markdown);
complete(#{
    path: path,
    headline: headline,
    confirmed_count: confirmed.len(),
    survey_count: surveys.len(),
    report: markdown,
});
