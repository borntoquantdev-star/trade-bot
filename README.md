# hyperbot

A trading bot for [Hyperliquid](https://hyperliquid.xyz) perpetuals, built so
that the part which can lose money is the part that is easiest to test.

Every order passes through one pure function, `risk_gate()`, which sizes it
from a risk budget and a stop distance and refuses it for one of thirteen
named reasons. The gate performs no I/O and has no clock of its own, so the
same code serves live trading, paper trading and - when it exists - a
backtest.

- [คู่มือภาษาไทย](docs/README.th.md) - install and usage in Thai
- [VS Code and AI assistants](docs/vscode.md) - Codex, Claude Code, Copilot
- [Architecture and the risk contract](docs/architecture.md)
- [Writing a strategy](docs/strategies.md)
- [Coming from grvt_demo_bot](docs/from-grvt-demo-bot.md)

---

## Requirements

| | |
|---|---|
| Python | 3.10 or newer (`python --version`) |
| Git | to clone |
| A Hyperliquid account | **not needed** to install, test or paper trade |

Nothing else. The core package depends on PyYAML alone.

## Install

### Windows (PowerShell)

```powershell
git clone https://github.com/borntoquantdev-star/hyperliquid-bot.git
cd hyperliquid-bot

python -m venv .venv
.venv\Scripts\python.exe -m pip install --upgrade pip
.venv\Scripts\python.exe -m pip install -e ".[dev]"
```

To type `hyperbot` and `pytest` without the long prefix, activate the
environment:

```powershell
.venv\Scripts\Activate.ps1
```

If PowerShell refuses with *"running scripts is disabled on this system"*,
either allow it once for this session:

```powershell
Set-ExecutionPolicy -Scope Process -ExecutionPolicy Bypass
.venv\Scripts\Activate.ps1
```

or skip activation entirely and keep calling `.venv\Scripts\hyperbot.exe` and
`.venv\Scripts\python.exe -m pytest`. Both work the same.

### macOS and Linux

```bash
git clone https://github.com/borntoquantdev-star/hyperliquid-bot.git
cd hyperliquid-bot

python3 -m venv .venv
source .venv/bin/activate
python -m pip install --upgrade pip
python -m pip install -e ".[dev]"
```

Then change one line in `.vscode/settings.json`:

```jsonc
"python.defaultInterpreterPath": "${workspaceFolder}/.venv/bin/python",
```

### Check that it worked

```bash
pytest                      # expect: 236 passed, 8 skipped
hyperbot --list-strategies  # expect: ma_cross and random
hyperbot --dry-run --doctor # expect: "--- all good ---"
```

All three run offline. No credentials, no network, no exchange SDK. If
`pytest` passes, the installation is complete.

The 8 skipped tests are the ones that would place real orders on the testnet;
they need `--run-integration` and credentials.

### Install for live trading

Only when you actually want to send orders:

```bash
pip install -e ".[hyperliquid]"     # adds hyperliquid-python-sdk
```

The SDK is deliberately not part of the default install. The bot imports it
only when a real broker connects, which is why tests and paper trading work
without it.

---

## First run

```bash
hyperbot --dry-run --iterations 5
```

That trades against an offline simulator. Watch what it decided:

```bash
cat logs/audit.jsonl          # PowerShell: Get-Content logs\audit.jsonl
```

One JSON line per iteration. A line with `"order"` is an approved trade; a
line with `"reject"` says which limit stopped it; a line with neither means
the strategy did not want to trade.

**Use `--iterations` while experimenting.** Without it the bot loops forever,
sleeping `loop_interval` seconds (60 by default) between iterations. `Ctrl+C`
always stops it cleanly.

## Configure

```bash
cp config/config.example.yaml config/config.yaml
# PowerShell: Copy-Item config\config.example.yaml config\config.yaml
```

`config/config.yaml` is gitignored and never committed. The example works
as-is for paper trading, so you only need to edit it to change behaviour:

```yaml
trading:
  symbol: "BTC"              # Hyperliquid names perps by coin, not by pair
  timeframe: "5m"            # candles loaded to warm a strategy up
  loop_interval: 60          # lower this to 5 while you are experimenting
  strategy: "random"         # hyperbot --list-strategies
  warmup_candles: 200

risk:
  risk_per_trade_pct: 1.0    # of equity, lost if the stop is hit
  max_positions: 1
  max_daily_loss_pct: 3.0    # equity drop since 00:00 UTC that halts the bot
  max_drawdown_pct: 10.0     # drop from peak equity that halts the bot
  max_leverage: 5            # exposure ceiling = equity x this
  max_slippage_pct: 0.1      # also becomes the limit price on every order
  fee_rate_pct: 0.045        # taker fee per leg, included when sizing
  allow_short: true
```

### The two Hyperliquid identities

This is the one thing worth reading twice, because mixing them up fails
silently:

```yaml
hyperliquid:
  network: "testnet"
  account_address: "0x..."   # the MASTER account. Every query uses this.
  api_wallet_key: "0x..."    # an API wallet's PRIVATE KEY. It only signs.
```

Create an API wallet at [app.hyperliquid.xyz/API](https://app.hyperliquid.xyz/API).
Query the account with the agent address instead of the master address and the
exchange returns an empty state **with no error**, so the config validates the
address shape and refuses a private key in that field.

To keep the key out of the file entirely, reference an environment variable:

```yaml
  api_wallet_key: "${HYPERLIQUID_API_WALLET_KEY}"
```

A `${VARIABLE}` that is not set in the shell is an error, on purpose.

## Trading the testnet

```bash
pip install -e ".[hyperliquid]"
# fill in config/config.yaml, then
hyperbot --doctor            # config, connection, price, instrument, candles
hyperbot --live
```

`network: mainnet` additionally requires `--live` on the command line. Without
it the bot refuses to start and sends nothing.

## Commands

| | |
|---|---|
| `hyperbot --dry-run` | Paper trade offline. No credentials needed. |
| `hyperbot --live` | Send real orders. Required when `network: mainnet`. |
| `hyperbot --doctor` | Check config, connection, price, instrument, account, candles. |
| `hyperbot --list-strategies` | What is registered. |
| `hyperbot --resume` | Clear a halt the risk gate wrote. |
| `hyperbot --close --yes` | Flatten the configured symbol. |
| `hyperbot --iterations N` | Stop after N loops. |
| `hyperbot --log-level DEBUG` | When something is not behaving. |

`hyperbot --help` lists everything.

---

## How a trade is decided

```
broker: mark price, equity, open positions, and candles at start-up
        |
        v
strategy.get_signal(market)  --> Signal (side, ref_price, stop_price, reason)
        |                         a stop is mandatory: it is what sizes the order
        v
risk_gate(signal, state, limits, spec, mark_price, now)
        |
        +--> Reject(code, detail)   logged, nothing is sent
        |
        +--> SizedOrder             qty from risk budget / (stop distance + fees)
                 |                  limit price caps slippage inside the order
                 v
          broker.place_order        the only code that touches the network
```

The engine owns no trading rules. It cannot resize an order, skip a check or
retry with different numbers - whatever the gate returns is sent unchanged or
refused and recorded.

Position size is solved, not guessed. Being stopped out costs the stop
distance plus the fees on both legs, and those fees depend on the quantity, so:

```
qty = risk_budget / (stop_distance + (entry + stop) x fee_rate)
```

Quantity is then floored to the lot step, which can only make the order
smaller than the budget allows. A test asserts that invariant across a range
of stop distances.

Hitting the daily loss or drawdown limit **halts** the bot: it writes the
reason to `logs/state.json` and stops. That survives a restart on purpose, and
only `--resume` clears it.

## Hyperliquid specifics the bot handles for you

- **Prices follow two rules at once:** at most 5 significant figures, *and* at
  most `6 - szDecimals` decimal places, except that whole numbers are always
  accepted. A single tick size cannot express that, so `InstrumentSpec`
  applies both and always rounds in the protective direction.
- **Sizes** are floored to `10 ** -szDecimals`, read from the exchange's perp
  metadata rather than configured by hand.
- **Minimum order value is $10.** Below that the exchange answers "Order must
  have minimum value of $10.", so the gate refuses first with
  `below_min_notional`.
- **Client order ids** are 128-bit. Ours is a `sha256` of the signal, 32 hex
  characters, so the exchange enforces the same no-duplicates rule we do.
- **Candles** prime the strategy's window at start-up, so an indicator needing
  50 points has them on the first iteration instead of waiting 50 loops.

## What it records

```
logs/audit.jsonl    one JSON line per decision: signal, reject or order
logs/state.json     day's starting equity, peak equity, halt flag, sent order ids
logs/hyperbot.log   rotating human-readable log
```

`logs/` is gitignored. Order ids are recorded the moment an order is accepted
and never sent twice.

## Writing your own strategy

Three steps, covered properly in [docs/strategies.md](docs/strategies.md):

1. Subclass `Strategy` in `hyperbot/strategies/`, implement `get_signal`, and
   return a `Signal` with a stop price.
2. Register it in `hyperbot/strategies/__init__.py`.
3. Write tests against synthetic prices, then
   `hyperbot --dry-run --strategy <name>`.

Or let an AI assistant walk you through it: `/strategy-builder` in Claude Code
or Copilot Chat, or ask Codex to read `ai/workflows/strategy-builder.md`. See
[docs/vscode.md](docs/vscode.md).

## Known limitations

Read these before trusting it with anything:

- **No backtester.** Nothing in this repository can tell you whether a
  strategy is profitable. Paper and testnet runs prove the plumbing, nothing
  more.
- **Strategies see closes, not bars.** The window is primed from real candles
  and then extended with observed marks; it is not a full OHLCV series.
- **Optimistic paper fills** at the limit price, immediately.
- **The bot never changes your account leverage.** It caps exposure instead.
- `random` is a coin flip with fees. It exists to exercise the pipeline.

## Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `python: command not found` | Try `py -3` on Windows, `python3` elsewhere. Python 3.10+ is required. |
| *"running scripts is disabled"* on activate | PowerShell execution policy. See the install section, or call `.venv\Scripts\hyperbot.exe` directly. |
| `hyperbot: command not found` | The environment is not activated. Use `.venv\Scripts\hyperbot.exe`, or `python -m hyperbot.cli.main`. |
| `the Hyperliquid SDK is not installed` | Live trading needs `pip install -e ".[hyperliquid]"`. Paper trading does not. |
| `user state carried no accountValue` | You are querying with the API wallet address. `account_address` must be the master account. |
| `BTC_USDT_Perp is not a Hyperliquid perp` | Symbols are coin names: `BTC`, `ETH`, `SOL`. |
| The bot runs but never trades | Read the reject codes: [docs/README.th.md](docs/README.th.md) has the table, or ask an assistant with `/debug-refusal`. |
| `config refers to ${HYPERLIQUID_API_WALLET_KEY}` | That variable is not set in this shell. Export it, or put the value in `config/config.yaml`. |
| It seems to hang | It is sleeping `loop_interval` seconds. Use `--iterations N`, lower `loop_interval`, or `Ctrl+C`. |

## Development

```bash
pytest                                      # offline, the default
pytest --run-integration                    # places real orders on the testnet
ruff check hyperbot tests tools
black hyperbot tests tools
mypy hyperbot
python tools/build_agent_files.py           # after editing anything in ai/
```

`AGENTS.md`, `CLAUDE.md`, `.claude/` and `.github/copilot-instructions.md` are
generated from `ai/conventions.md` and `ai/workflows/`. Edit the sources, then
run the build script; CI fails if they drift apart.

In VS Code, the task **checks: everything before a commit** runs lint, tests
and the generated-file check in sequence.

## License

MIT. Trading perpetuals with leverage can lose more than you put in - this is
educational software, not financial advice.
