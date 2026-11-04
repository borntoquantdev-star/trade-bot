<!-- GENERATED FILE - do not edit.
     Source: ai/conventions.md + ai/workflows/
     Regenerate: python tools/build_agent_files.py -->

# CLAUDE.md

The conventions for this repository are in `AGENTS.md`, imported here
so both files stay in step:

@AGENTS.md

## Skills and commands

These are available as skills, and as slash commands of the same name
(`/strategy-builder`, `/debug-refusal`):

- [Why the bot will not trade](.claude/skills/debug-refusal/SKILL.md) - Diagnose why the bot is refusing to place orders, or has halted, by reading the reject code in its audit trail. Use when the bot runs but never trades, or stopped trading.
- [Trading Strategy Builder](.claude/skills/strategy-builder/SKILL.md) - Interview the user about their trading idea, then turn it into a tested Strategy class for this bot. Use when they want to build, convert or change a trading strategy.

Each one is a thin pointer to the single copy of the workflow in
`ai/workflows/`.
