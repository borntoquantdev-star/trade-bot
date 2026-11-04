#!/usr/bin/env python3
"""Generate the per-tool AI assistant files from one source.

The content lives in exactly two places:

    ai/conventions.md          how to work in this repo
    ai/workflows/*.md          guided workflows, with frontmatter

Everything else is generated, because Codex, Claude Code and Copilot each
expect a different filename and a different frontmatter shape, and three
hand-maintained copies of the same instructions drift apart within a week.

Generated:

    AGENTS.md                            Codex and other AGENTS.md readers
    CLAUDE.md                            Claude Code
    .claude/skills/<name>/SKILL.md       Claude Code skills
    .claude/commands/<name>.md           Claude Code slash commands
    .github/copilot-instructions.md      GitHub Copilot
    .github/prompts/<name>.prompt.md     Copilot prompt files

Usage:

    python tools/build_agent_files.py            write the files
    python tools/build_agent_files.py --check    fail if they are stale (CI)

Only the "always loaded" files (AGENTS.md, copilot-instructions.md) inline the
conventions. The workflow adapters are deliberately thin pointers to the one
copy in ai/workflows/, so a workflow is never duplicated.
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONVENTIONS = ROOT / "ai" / "conventions.md"
WORKFLOW_DIR = ROOT / "ai" / "workflows"

BANNER = (
    "<!-- GENERATED FILE - do not edit.\n"
    "     Source: {source}\n"
    "     Regenerate: python tools/build_agent_files.py -->\n"
)


class Workflow:
    """One workflow file: frontmatter plus the path to its body."""

    def __init__(self, path: Path) -> None:
        self.path = path
        self.meta, self.body = _split_frontmatter(path.read_text(encoding="utf-8"))
        self.slug = self.meta.get("name") or path.stem
        self.title = self.meta.get("title") or self.slug.replace("-", " ").title()
        self.description = self.meta.get("description") or ""
        if not self.description:
            raise SystemExit(f"{path}: frontmatter needs a 'description'")

    @property
    def rel(self) -> str:
        return self.path.relative_to(ROOT).as_posix()


def _split_frontmatter(text: str) -> tuple[dict[str, str], str]:
    """Parse ``key: value`` frontmatter without needing a YAML library."""
    if not text.startswith("---"):
        return {}, text
    lines = text.splitlines()
    end = next((i for i, line in enumerate(lines[1:], start=1) if line.strip() == "---"), None)
    if end is None:
        return {}, text
    meta: dict[str, str] = {}
    for line in lines[1:end]:
        if not line.strip() or ":" not in line:
            continue
        key, _, value = line.partition(":")
        meta[key.strip()] = value.strip().strip('"').strip("'")
    return meta, "\n".join(lines[end + 1 :]).lstrip("\n")


def load_workflows() -> list[Workflow]:
    if not WORKFLOW_DIR.is_dir():
        raise SystemExit(f"missing {WORKFLOW_DIR}")
    return [Workflow(p) for p in sorted(WORKFLOW_DIR.glob("*.md"))]


def conventions_body() -> str:
    meta_free, body = _split_frontmatter(CONVENTIONS.read_text(encoding="utf-8"))
    # Drop the "this file is the source" paragraph: in a generated file it
    # would point the reader back to itself.
    lines = body.splitlines()
    # Drop the source file's own H1: the generated file supplies its own.
    if lines and lines[0].startswith("# "):
        lines = lines[1:]
    out: list[str] = []
    skipping = False
    for line in lines:
        if line.startswith("This file is the single source of truth"):
            skipping = True
            continue
        if skipping:
            if line.strip() == "":
                skipping = False
            continue
        out.append(line)
    return "\n".join(out).strip() + "\n"


def workflow_index(link: str) -> str:
    """A markdown list of workflows. *link* picks which path to point at.

    Paths are written relative to the file being generated, which is why the
    "prompt" variant has no ``.github/`` prefix: copilot-instructions.md lives
    inside that directory already.
    """
    rows = []
    for wf in load_workflows():
        target = {
            "source": wf.rel,
            "skill": f".claude/skills/{wf.slug}/SKILL.md",
            "prompt": f"prompts/{wf.slug}.prompt.md",
        }[link]
        rows.append(f"- [{wf.title}]({target}) - {wf.description}")
    return "\n".join(rows) + "\n"


# --- the generated documents ------------------------------------------------


BOTH_SOURCES = "ai/conventions.md + ai/workflows/"


def _join(*parts: str) -> str:
    """Concatenate document sections, keeping each one readable in the source."""
    return "".join(parts)


def agents_md() -> str:
    return _join(
        BANNER.format(source=BOTH_SOURCES),
        "\n# AGENTS.md\n\n",
        conventions_body(),
        "\n## Guided workflows\n\n",
        "When a request matches one of these, read that file and follow it\n",
        "rather than improvising:\n\n",
        workflow_index("source"),
    )


def claude_md() -> str:
    return _join(
        BANNER.format(source=BOTH_SOURCES),
        "\n# CLAUDE.md\n\n",
        "The conventions for this repository are in `AGENTS.md`, imported here\n",
        "so both files stay in step:\n\n",
        "@AGENTS.md\n\n",
        "## Skills and commands\n\n",
        "These are available as skills, and as slash commands of the same name\n",
        "(`/strategy-builder`, `/debug-refusal`):\n\n",
        workflow_index("skill"),
        "\nEach one is a thin pointer to the single copy of the workflow in\n",
        "`ai/workflows/`.\n",
    )


def copilot_instructions_md() -> str:
    return _join(
        BANNER.format(source=BOTH_SOURCES),
        "\n# Copilot instructions\n\n",
        conventions_body(),
        "\n## Prompt files\n\n",
        "Guided workflows are available in Copilot Chat as `/`-prompts:\n\n",
        workflow_index("prompt"),
    )


def skill_md(wf: Workflow) -> str:
    return (
        f"---\nname: {wf.slug}\ndescription: {wf.description}\n---\n\n"
        + BANNER.format(source=wf.rel)
        + f"\n# {wf.title}\n\n"
        f"Read **`{wf.rel}`** and follow it step by step. That file is the only\n"
        "copy of this workflow; this file exists so the skill can be discovered.\n\n"
        "Also read `ai/conventions.md` before changing any code.\n"
    )


def command_md(wf: Workflow) -> str:
    return (
        f"---\ndescription: {wf.description}\n---\n\n"
        + BANNER.format(source=wf.rel)
        + f"\nRead `{wf.rel}` and follow it step by step, together with\n"
        "`ai/conventions.md`.\n\n"
        "Arguments, if any, describe what the user wants to build or debug: $ARGUMENTS\n"
    )


def prompt_md(wf: Workflow) -> str:
    return (
        f"---\nmode: agent\ndescription: {wf.description}\n---\n\n"
        + BANNER.format(source=wf.rel)
        + f"\nRead `{wf.rel}` and follow it step by step, together with\n"
        "`ai/conventions.md`. Ask the user one question at a time.\n"
    )


def planned_files() -> dict[Path, str]:
    files: dict[Path, str] = {
        ROOT / "AGENTS.md": agents_md(),
        ROOT / "CLAUDE.md": claude_md(),
        ROOT / ".github" / "copilot-instructions.md": copilot_instructions_md(),
    }
    for wf in load_workflows():
        files[ROOT / ".claude" / "skills" / wf.slug / "SKILL.md"] = skill_md(wf)
        files[ROOT / ".claude" / "commands" / f"{wf.slug}.md"] = command_md(wf)
        files[ROOT / ".github" / "prompts" / f"{wf.slug}.prompt.md"] = prompt_md(wf)
    return files


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--check", action="store_true", help="exit non-zero if any file is out of date"
    )
    args = parser.parse_args(argv)

    files = planned_files()
    stale: list[str] = []
    for path, content in sorted(files.items()):
        rel = path.relative_to(ROOT).as_posix()
        current = path.read_text(encoding="utf-8") if path.exists() else None
        if current == content:
            continue
        stale.append(rel)
        if not args.check:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content, encoding="utf-8", newline="\n")

    if args.check:
        if stale:
            print("out of date, run python tools/build_agent_files.py:")
            for rel in stale:
                print(f"  {rel}")
            return 1
        print(f"{len(files)} generated files are up to date")
        return 0

    if stale:
        print(f"wrote {len(stale)} file(s):")
        for rel in stale:
            print(f"  {rel}")
    else:
        print(f"{len(files)} generated files were already up to date")
    return 0


if __name__ == "__main__":
    sys.exit(main())
