# VS Code, and the three AI assistants

This repository used to carry an Antigravity-only workflow folder. The
instructions now live in one place and are published in the format each tool
expects, so Codex, Claude Code and Copilot all read the same rules.

## 1. Open the project

```bash
git clone https://github.com/borntoquantdev-star/trade-bot.git
cd trade-bot
code .
```

VS Code will offer the recommended extensions from `.vscode/extensions.json`.
The Python ones are needed; the three AI extensions are optional — install
whichever you use.

## 2. Create the environment

Run the task **setup: create venv** (`Ctrl+Shift+P` → *Tasks: Run Task*), or:

```bash
python -m venv .venv
```

Then `Ctrl+Shift+P` → **Python: Select Interpreter** → `.venv`. On macOS or
Linux also change `python.defaultInterpreterPath` in `.vscode/settings.json`
from `.venv/Scripts/python.exe` to `.venv/bin/python`.

Install with the task **setup: install for development**, or:

```bash
pip install -e ".[dev]"
```

The Hyperliquid SDK is a separate extra (`pip install -e ".[hyperliquid]"`) and is only
needed to send real orders. Everything else — tests, paper trading, writing a
strategy — works without it.

## 3. Check it works

Open the **Testing** panel: 189 tests should be discovered and pass. They are
all offline, so running them cannot touch an exchange. Tests that do place
real orders are skipped unless you pass `--run-integration`.

Then press `F5` and pick **Bot: dry run (20 iterations)**. Breakpoints in
`tradebot/risk.py` are a good way to watch an order being sized.

## 4. Useful tasks

`Ctrl+Shift+P` → *Tasks: Run Task*:

| Task | What it does |
|---|---|
| `test` | The offline suite (also `Ctrl+Shift+P` → *Run Test Task*) |
| `test: with coverage` | Adds a coverage report |
| `lint` / `format` | ruff and black |
| `bot: dry run` | 20 paper iterations |
| `bot: doctor` | Checks config, connection, price, account |
| `ai files: rebuild` | Regenerates the assistant files from `ai/` |
| `checks: everything before a commit` | lint, test, and the generated-file check |

Launch configurations (`F5`) cover a dry run, a long dry run, the doctor, a
testnet live run, and pytest on the current file.

---

## How the AI assistants are wired

One source, three outputs:

```
ai/conventions.md        the rules: layers, what must not change, house style
ai/workflows/*.md        guided workflows, each with frontmatter
        │
        │  python tools/build_agent_files.py
        ▼
AGENTS.md                           Codex and anything else reading AGENTS.md
CLAUDE.md                           Claude Code (imports AGENTS.md)
.claude/skills/<name>/SKILL.md      Claude Code skills
.claude/commands/<name>.md          Claude Code slash commands
.github/copilot-instructions.md     Copilot Chat
.github/prompts/<name>.prompt.md    Copilot prompt files
```

The two "always loaded" files inline the conventions. The workflow adapters
are thin pointers to the single copy in `ai/workflows/`, so no workflow text
is ever duplicated. `python tools/build_agent_files.py --check` fails if a
generated file is stale, and CI runs it.

**Never edit a generated file.** Each one says so at the top. Edit the source
in `ai/`, re-run the build, and commit both.

### Codex

The Codex extension reads `AGENTS.md` from the repository root, so the
conventions are in context with no setup. For a workflow, ask for it by name:

> อ่าน ai/workflows/strategy-builder.md แล้วพาผมสร้างกลยุทธ์ตามนั้น

Codex's own saved prompts live in your home directory (`~/.codex/prompts`),
not in the repository, which is why the workflows are kept as repository files
that any tool can read.

### Claude Code

`CLAUDE.md` is read automatically and imports `AGENTS.md`. The workflows are
available two ways:

- as skills — Claude picks one up when a request matches its description,
- as slash commands: `/strategy-builder`, `/debug-refusal`.

### GitHub Copilot

`.github/copilot-instructions.md` is picked up by Copilot Chat in recent
versions. In Copilot Chat, the workflows appear as prompts:

```
/strategy-builder
/debug-refusal
```

If they do not show up, check these VS Code settings:

- `chat.promptFiles` — enables `.github/prompts/*.prompt.md`
- `github.copilot.chat.codeGeneration.useInstructionFiles` — enables
  `.github/copilot-instructions.md`

## Adding a workflow

1. Write `ai/workflows/my-workflow.md` with frontmatter:

   ```markdown
   ---
   name: my-workflow
   title: Human readable title
   description: One sentence; this is what the assistant matches against.
   ---
   ```

2. Run `python tools/build_agent_files.py`.
3. Commit the source and the six generated files it touched.

The `description` matters more than it looks: it is the only thing a skill or
prompt picker sees when deciding whether your workflow is relevant.

## Working on this repository with an assistant

The conventions file states what must not change — `risk_gate` stays pure and
synchronous, position size comes only from the risk budget and the stop
distance, money stays `Decimal`, secrets never get logged. If an assistant
proposes a change that crosses one of those lines, that is the answer being
wrong, not the rule.

Ask for `pytest` and `ruff check` to be run before any change is called done.
The suite is offline and takes about four seconds, so there is no reason to
skip it.
