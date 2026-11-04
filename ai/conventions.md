# Project conventions for AI assistants

This file is the single source of truth for how to work in this repository.
`AGENTS.md`, `CLAUDE.md` and `.github/copilot-instructions.md` are generated
from it — edit this file, then run `python tools/build_agent_files.py`.

## What this project is

A trading bot for GRVT and Hyperliquid perpetual futures, chosen with one config line. It is used to
teach people how an automated strategy is built safely, so clarity and
guardrails matter more than features.

```
tradebot/
  money.py        Decimal helpers. Money never touches float.
  types.py        Immutable value types: Signal, SizedOrder, Reject, limits.
  risk.py         risk_gate() — the one place that approves an order.
  strategies/     Market data in, Signal or None out. No I/O.
  brokers/        The only code allowed to touch the network.
    paper.py      Offline simulator: powers --dry-run and the tests.
    grvt.py       GRVT. Imports its SDK lazily.
    hyperliquid.py  Hyperliquid. Imports its SDK lazily.
    __init__.py   build_live_broker(): picks one from config.exchange.
  core/
    config.py     YAML + environment variables, validated.
    engine.py     The loop. Owns no trading rules.
  cli/main.py     Argument parsing and one-shot actions.
```

The dependency direction is one-way: `cli → core → {strategies, brokers} →
{risk, types, money}`. Nothing in the inner layers may import an outer one.

## Rules that must not be broken without asking the user first

1. **`risk_gate()` stays pure.** No network, no file access, no clock of its
   own, no randomness, no LLM call. `now` is a parameter. This is what lets
   the same function serve live trading, paper trading and a future backtest
   without a second implementation.
2. **`risk_gate()` is never `async`.**
3. **Position size comes from the risk budget and the stop distance only.** A
   strategy may not influence size — not through a `confidence` field, not
   through a suggested quantity, not by any other route.
4. **Every `Signal` carries a stop price.** There is no code path that opens a
   position without one.
5. **Money is `Decimal`.** The only permitted `float()` calls are at the SDK
   boundaries inside `brokers/`, where the exchange libraries demand them.
6. **Venue quirks live in the broker.** Symbol shape, price rounding rules,
   client order id format, minimum size - a broker absorbs all of it.
   `risk_gate` must never learn an exchange's name, and no `if exchange ==`
   may appear outside `brokers/` and `core/config.py`.
7. **The engine does not second-guess the gate.** Whatever the gate returns is
   sent unchanged, or refused and logged. No resizing, no retry with different
   numbers, no bypass flag.
8. **Secrets are `Secret` objects.** Never log, print, format or write a
   revealed credential. Call `.reveal()` only where the value is used.
9. **The default test run is offline.** `pytest` must pass with no network, no
   credentials and no exchange SDK installed. Anything that sends a real order
   is marked `@pytest.mark.integration` and needs `--run-integration`.
10. **Live trading needs an explicit flag.** `grvt.env: prod` or
    `hyperliquid.network: mainnet` without `--live` must refuse to run.
11. **The two Hyperliquid identities stay apart.** `account_address` is the
    master account and is what every query uses; `api_wallet_key` only signs.
    Querying with the agent address returns an empty state with no error, so
    the config validates the address shape and refuses a key in that field.

## When you add a feature

- A new refusal reason: add to `RejectCode` **in severity order** (most
  serious first — the enum's order defines `severity`), add the check to
  `risk_gate` in the same position, and add a test that triggers it.
- A new strategy: subclass `Strategy`, implement `get_signal`, register it in
  `tradebot/strategies/__init__.py`, and write tests against synthetic prices.
  Never let a strategy call a broker.
- A new exchange: implement the `Broker` protocol in `tradebot/brokers/`, keep
  reply parsing in pure module-level `read_*` functions, and test those with
  payload dictionaries rather than a live connection.
- A new config key: add it to `DEFAULTS`, parse and validate it in `_build`,
  document it in `config/config.example.yaml`, and cover the invalid case in
  `tests/test_config.py`.

## House style

- Python 3.10+, `from __future__ import annotations` at the top of each module.
- Type hints on public functions. Docstrings explain *why*, not *what*.
- Line length 100. `black` for formatting, `ruff` for linting.
- Test names are sentences: `test_refuses_when_price_has_run_away_from_the_signal`.
- Comments earn their place by explaining a decision or a trap, never by
  narrating the next line.
- Python sources stay ASCII-only: no emoji, no em dashes, no smart quotes.
  A Windows console is often not UTF-8, and a log line that renders as
  `�` is a log line nobody reads. Thai text belongs in `docs/`, not in
  code or log strings.

## Before you say you are done

```bash
pytest                              # must pass offline
ruff check tradebot tests           # must be clean
black --check tradebot tests
python tools/build_agent_files.py --check   # generated files up to date
```

## Things that are deliberately missing

Do not "fix" these without discussing it — each is a known limitation with a
reason, documented in `docs/architecture.md`:

- The strategy window is primed from real candles at start-up, then extended
  with observed marks. It is not a full candle series: strategies see closes,
  not OHLCV bars.
- There is no backtester yet. The gate is pure precisely so one can be added
  without touching it.
- The paper broker fills optimistically at the limit price. It proves the
  plumbing works; it says nothing about profitability.
- Leverage: `HyperliquidBroker.set_leverage` exists, but the engine never
  calls it. The bot caps *exposure* through `risk.max_leverage`, which is a
  different thing and the one it can enforce by itself.
