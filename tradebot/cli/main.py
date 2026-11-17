"""Command line entry point.

    tradebot --dry-run                 paper trade, no credentials needed
    tradebot --doctor                  check the config and the connection
    tradebot --list-strategies         what is registered
    tradebot --fetch-candles           download price history into data/candles
    tradebot --backtest                replay that history, offline
    tradebot --check                   is the strategy fit for testnet? (0 = pass)
    tradebot --live                    real orders (prod requires this flag);
                                       only after --check passed on this file
                                       and this market
    tradebot --resume                  clear a paper halt (add --live for an exchange's)
    tradebot --close --yes             flatten the configured symbol

``--backtest`` never opens a connection: it reads the stored candles and runs
them through the same risk gate the live engine uses.  ``--fetch-candles``
connects only to read - it cannot place an order - but it still needs
``--live`` to reach the exchange: without it the paper broker answers, and
its invented candles are stored under ``paper``, apart from the real ones.

The paper broker and an exchange keep separate state files (``LIVE_STATE``,
``PAPER_STATE``), so a dry run can never spend or reset an account's daily
budget.  ``--resume`` clears the halt of whichever one the same flags select.

``--dry-run`` is the default when ``hyperliquid.network`` is ``testnet`` and no ``--live``
flag is given, so the accident path leads to the paper broker rather than to
the exchange.
"""

from __future__ import annotations

import argparse
import inspect
import json
import logging
import sys
from collections.abc import Sequence
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from .. import __version__, runs
from ..backtest import run_backtest, summary, trade_table, write_json
from ..backtest.runner import BacktestError
from ..brokers.base import BrokerError
from ..brokers.paper import PaperBroker, fixed_price, random_walk
from ..brokers.public_price import live_mark_price
from ..core.config import BotConfig, ConfigError, describe, load_config
from ..core.engine import Engine
from ..data.fetch import download
from ..data.store import DEFAULT_ROOT, CandleStore
from ..money import D
from ..strategies import STRATEGIES, UnknownStrategy, build_strategy, describe_strategies
from ..utils.logger import setup_logger
from .check import clearance as check_clearance
from .check import run_check
from .check import write as write_check

DEFAULT_CONFIG = "config/config.yaml"

# The paper broker keeps its own daily budget and halt flag. Sharing one
# file, a paper run on 16 Nov opened the Thai day at the simulator's 10000
# and the testnet account then counted its 9789 as a 210 loss it never made,
# and halted. Each broker answers only for its own money.
LIVE_STATE = "logs/state.json"
PAPER_STATE = "logs/state.paper.json"

# Where invented candles go, instead of under an exchange's name.
PAPER_SHELF = "paper"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="tradebot",
        description="Trading bot for Hyperliquid perpetuals with a pure, testable risk gate.",
        epilog="Read docs/vscode.md for the editor setup and docs/architecture.md for the design.",
    )
    parser.add_argument("--version", action="version", version=f"tradebot {__version__}")
    parser.add_argument(
        "--config", default=DEFAULT_CONFIG, help=f"config file (default: {DEFAULT_CONFIG})"
    )
    parser.add_argument(
        "--strategy", default=None, help="override trading.strategy from the config"
    )
    parser.add_argument("--log-file", default="logs/tradebot.log", help="log file path")
    parser.add_argument(
        "--log-level", default="INFO", choices=["DEBUG", "INFO", "WARNING", "ERROR"]
    )
    parser.add_argument(
        "--state-file",
        default=None,
        help=f"where the daily budget and halt flag live (default: {LIVE_STATE} "
        f"for an exchange, {PAPER_STATE} for the paper broker)",
    )
    parser.add_argument(
        "--audit-file", default="logs/audit.jsonl", help="one JSON line per decision"
    )
    parser.add_argument(
        "--iterations", type=int, default=None, help="stop after N loops (default: run forever)"
    )
    parser.add_argument(
        "--trace-file",
        default="logs/trace.jsonl",
        help="where each tick's trace goes, so the audit trail stays readable",
    )
    parser.add_argument(
        "--stop-file",
        default="logs/stop.request",
        help="end the run politely when this file appears (the board's stop button)",
    )
    parser.add_argument(
        "--close-at-end",
        action="store_true",
        help="close the position when the run stops (after --iterations, or Ctrl+C)",
    )
    parser.add_argument(
        "--runs-file",
        default="logs/runs.jsonl",
        help="one line when a run starts and one when it ends",
    )
    parser.add_argument(
        "--trace",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="record which lines of the bot ran, into the audit trail (default: on)",
    )

    mode = parser.add_argument_group("mode")
    mode.add_argument(
        "--dry-run",
        action="store_true",
        help="paper broker: offline, deterministic, no credentials",
    )
    mode.add_argument(
        "--live",
        action="store_true",
        help="send real orders; required on grvt prod or hyperliquid mainnet. "
        "A trading run needs a passed --check of this strategy file on this market",
    )
    mode.add_argument("--paper-price", default="100000", help="starting price for the paper broker")
    mode.add_argument(
        "--paper-live",
        action="store_true",
        help="paper trade on the real market price (public feed, no credentials)",
    )
    mode.add_argument(
        "--paper-walk",
        nargs="?",
        const="0.08",
        default=None,
        metavar="PCT",
        help="let the paper price wander by up to PCT%% per iteration "
        "(default 0.08), so a dry run actually moves",
    )

    history = parser.add_argument_group("price history and backtesting")
    history.add_argument(
        "--fetch-candles",
        action="store_true",
        help="download candle history for the configured symbol, then exit",
    )
    history.add_argument(
        "--backtest",
        action="store_true",
        help="replay the stored candles through the risk gate, then exit",
    )
    history.add_argument(
        "--days", type=int, default=365, help="how far back to fetch or test (default: 365)"
    )
    history.add_argument(
        "--since", default=None, metavar="YYYY-MM-DD", help="start date, instead of --days"
    )
    history.add_argument(
        "--data-dir", default=str(DEFAULT_ROOT), help=f"candle store (default: {DEFAULT_ROOT})"
    )
    history.add_argument(
        "--refresh", action="store_true", help="re-download bars the store already has"
    )
    history.add_argument("--trades", action="store_true", help="list the backtest's round trips")
    history.add_argument(
        "--backtest-out",
        default="logs/backtest.json",
        help="where to write the machine-readable result",
    )

    actions = parser.add_argument_group("one-shot actions")
    actions.add_argument(
        "--list-strategies", action="store_true", help="print the registry and exit"
    )
    actions.add_argument(
        "--json",
        action="store_true",
        help="with --list-strategies: machine-readable, with whether each one passed "
        "--check on --config (what the board's strategy picker reads)",
    )
    actions.add_argument(
        "--doctor", action="store_true", help="check config and connectivity, then exit"
    )
    actions.add_argument("--resume", action="store_true", help="clear a halt and exit")
    actions.add_argument(
        "--check",
        action="store_true",
        help="is the strategy fit for testnet? contract, its tests, real candles, paper - "
        "offline, then exit (0 = pass)",
    )
    actions.add_argument(
        "--close", action="store_true", help="flatten the configured symbol and exit"
    )
    actions.add_argument("--yes", action="store_true", help="confirm an action that moves money")
    return parser


def _wrong_folder_hint(path: str) -> str:
    """Say where the command ran and where it should have.

    A relative --config is read from the folder the command runs in, and the
    usual way to miss it is to be one folder off - in board/, say - with the
    file sitting right there in the bot's folder.
    """
    here = Path.cwd()
    bot = Path(__file__).resolve().parents[2]
    if not Path(path).is_absolute() and (bot / path).exists() and bot != here:
        return (
            f"this command ran in {here}, but {path} is in {bot}. "
            f'Run it from there:  cd "{bot}"'
        )
    return f"this command ran in {here}; --config is read relative to that folder"


def _load(args: argparse.Namespace, log: logging.Logger) -> BotConfig | None:
    path: str | None = args.config
    if path and not Path(path).exists():
        if args.config != DEFAULT_CONFIG:
            log.error("config file not found: %s", path)
            log.error("%s", _wrong_folder_hint(path))
            return None
        log.warning(
            "no %s yet - running on built-in defaults. "
            "Copy config/config.example.yaml to config/config.yaml to change anything.",
            DEFAULT_CONFIG,
        )
        path = None
    try:
        return load_config(path)
    except ConfigError as exc:
        # A testnet config with its keys not filled in yet is still fine for
        # everything that never reaches the exchange: paper, --check,
        # --backtest. Refusing it there stopped a student with no keys from
        # checking their own strategy (16 Nov). --live still refuses.
        if args.live or "environment variable is not set" not in str(exc):
            log.error("%s", exc)
            return None
        log.warning("%s - fine without --live, which is the only thing that needs it", exc)
    try:
        return load_config(path, allow_missing_env=True)
    except ConfigError as exc:
        log.error("%s", exc)
        return None


def _make_broker(
    config: BotConfig,
    paper: bool,
    log: logging.Logger,
    paper_price: str = "100000",
    paper_walk: str | None = None,
    paper_live: bool = False,
):
    if paper:
        if paper_live:
            # Real prices, paper fills, no credentials: the public feed the
            # board already draws from.
            source = live_mark_price(config.exchange, config.venue_name, logger=log)
        elif paper_walk:
            source = random_walk(D(paper_price), paper_walk)
        else:
            source = fixed_price(D(paper_price))
        return PaperBroker(
            price_source=source,
            instrument=config.instrument,
            equity=config.starting_equity,
            logger=log,
        )
    from ..brokers import build_live_broker  # imported late: needs the exchange SDK

    config.require_live_credentials()
    return build_live_broker(config, log)


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    if args.list_strategies:
        if args.json:
            print(json.dumps(_strategy_listing(args), ensure_ascii=True))
            return 0
        print("Registered strategies:\n")
        print(describe_strategies())
        return 0

    log = setup_logger("tradebot", args.log_file, getattr(logging, args.log_level))
    log.info("tradebot %s", __version__)

    config = _load(args, log)
    if config is None:
        return 2

    if args.check:
        # Before the broker too: the check never connects. Its paper layer
        # builds a paper broker of its own.
        return _check(config, args, log)

    if args.backtest:
        # Deliberately before the broker is built: a backtest reads the disk
        # and nothing else, so it works with no credentials and no network.
        return _backtest(config, args, log)

    # Decide paper vs live, and refuse to touch production by accident.
    paper = args.dry_run or not args.live
    if config.is_live and not args.live:
        log.error(
            "%s points at real money (%s) but --live was not passed. Nothing was "
            "sent. Run with --dry-run to paper trade, or add --live if you really "
            "mean real money on a real account.",
            config.exchange,
            config.venue_name,
        )
        return 2
    if args.dry_run and args.live:
        log.error("--dry-run and --live contradict each other; pick one")
        return 2
    # Only a run that lets the strategy send orders is gated. Reading
    # candles, the doctor, a halt reset and flattening never ask it anything.
    trades = not (args.fetch_candles or args.doctor or args.resume or args.close)
    if not paper and trades:
        refusal = _cleared(config, args, log)
        if refusal is not None:
            return refusal

    log.info("configuration:\n%s", describe(config))
    log.info(
        "broker: %s",
        "paper (offline)" if paper else f"{config.exchange} {config.venue_name}",
    )
    if (Path(__file__).resolve().parents[2] / "dashboard" / "server.js").exists():
        log.info("dashboard: node dashboard/server.js --open  (reads logs/, never trades)")

    try:
        broker = _make_broker(
            config, paper, log, args.paper_price, args.paper_walk, args.paper_live
        )
    except (BrokerError, ConfigError) as exc:
        log.error("%s", exc)
        return 2

    if args.fetch_candles:
        return _fetch_candles(config, broker, args, log, paper)

    try:
        strategy = build_strategy(args.strategy or config.trading.strategy, config, log)
    except UnknownStrategy as exc:
        log.error("%s", exc)
        return 2

    engine = Engine(
        config=config,
        broker=broker,
        strategy=strategy,
        logger=log,
        state_path=Path(args.state_file or (PAPER_STATE if paper else LIVE_STATE)),
        audit_path=Path(args.audit_file) if args.audit_file else None,
        trace=args.trace,
        trace_path=Path(args.trace_file) if args.trace_file else None,
        stop_path=Path(args.stop_file) if args.stop_file else None,
    )

    if args.resume:
        engine.resume()
        log.info("halt cleared; the next run may trade again")
        return 0

    try:
        engine.start()
    except BrokerError as exc:
        log.error("could not start: %s", exc)
        return 2

    if args.doctor:
        return _doctor(engine, log, paper=paper, has_keys=config.has_credentials)

    if args.close:
        if not args.yes:
            log.error("--close moves money. Re-run with --close --yes to confirm.")
            return 2
        receipt = broker.close_position(config.trading.symbol)
        log.info("close result: %s", receipt or "nothing was open")
        return 0

    if not paper:
        log.warning(
            "LIVE MODE on %s %s. The bot caps exposure at %sx equity; the account's "
            "own leverage setting is separate (tradebot does not change it).",
            config.exchange,
            config.venue_name,
            config.risk.max_leverage,
        )

    # A run is bracketed rather than inferred from gaps in the audit
    # trail: a gap means "stopped and restarted" only if you already know
    # the loop interval, and the machine did not sleep, and the clock did
    # not move.
    runs_path = Path(args.runs_file) if args.runs_file else None
    run_id = runs.started(
        runs_path,
        runs.Start(
            strategy=args.strategy or config.trading.strategy,
            exchange=config.exchange,
            symbol=config.trading.symbol,
            timeframe=config.trading.timeframe,
            mode="paper" if paper else f"{config.exchange} {config.venue_name}",
            config=args.config,
            iterations=args.iterations,
        ),
    )
    reason = "stopped"
    try:
        code = engine.run(max_iterations=args.iterations)
        reason = (
            "halted"
            if engine.state.halted
            else "stopped from the board" if engine.stopped_on_request else "finished"
        )
        # Asked for up front, when the run was started, rather than with a
        # --yes at the end: the person chose this run and its ending in the
        # same command. Only on a normal return - after an exception the
        # broker's state is exactly what is in doubt.
        if args.close_at_end:
            closed = engine.flatten("run ended (--close-at-end)")
            if closed is not None:
                reason += " and closed the position"
        return code
    except BaseException as exc:  # including KeyboardInterrupt
        reason = f"{type(exc).__name__}: {exc}"
        raise
    finally:
        runs.finished(
            runs_path,
            run_id,
            ticks=engine.ticks,
            orders=engine.orders,
            exits=engine.exits,
            rejects=engine.rejects,
            first_equity=engine.first_equity,
            last_equity=engine.last_equity,
            halted=engine.state.halted,
            reason=reason,
        )


def _window(args: argparse.Namespace, log: logging.Logger) -> tuple[datetime, datetime] | None:
    """The period to fetch or test, as UTC bounds."""
    end = datetime.now(timezone.utc)
    if args.since:
        try:
            start = datetime.fromisoformat(args.since).replace(tzinfo=timezone.utc)
        except ValueError:
            log.error("--since wants a date like 2025-01-31, got %r", args.since)
            return None
        if start >= end:
            log.error("--since %s is not in the past", args.since)
            return None
        return start, end
    if args.days <= 0:
        log.error("--days must be positive, got %s", args.days)
        return None
    return end - timedelta(days=args.days), end


def _fetch_candles(
    config: BotConfig,
    broker,
    args: argparse.Namespace,
    log: logging.Logger,
    paper: bool,
) -> int:
    """Download price history into the candle store.  Reads only."""
    window = _window(args, log)
    if window is None:
        return 2
    start, end = window
    symbol = config.trading.symbol
    interval = config.trading.timeframe

    # Invented candles are shelved under "paper", never under the exchange's
    # name. Stored as "grvt" they were indistinguishable from the real thing,
    # and a backtest on one flat price simply never traded (16 Nov).
    shelf = PAPER_SHELF if paper else config.exchange
    real = _command(args, f"--live --fetch-candles --days {args.days}")
    if paper:
        log.warning(
            "the paper broker invents its candles (one flat price). They exercise "
            "the download path and tell you NOTHING about the market, so they are "
            "kept apart, under '%s', where a backtest will not read them. "
            "For real history: %s",
            PAPER_SHELF,
            real,
        )

    try:
        broker.connect()
    except (BrokerError, ConfigError) as exc:
        log.error("could not connect: %s", exc)
        return 2

    store = CandleStore(args.data_dir)
    log.info(
        "fetching %s %s from %s to %s",
        symbol,
        interval,
        start.date().isoformat(),
        end.date().isoformat(),
    )
    try:
        report = download(
            broker,
            store,
            symbol,
            interval,
            start,
            end,
            exchange=shelf,
            log=log,
            refresh=args.refresh,
            on_page=lambda r: log.info("  %s bars so far", r.fetched),
        )
    except (BrokerError, TypeError, ValueError) as exc:
        log.error("fetch failed: %s", exc)
        return 2

    log.info("%s", report.describe())
    log.info("stored at %s", store.path_for(shelf, symbol, interval))
    if report.gaps:
        log.warning(
            "the history has holes. A backtest will skip them rather than "
            "invent prices, but the result covers less time than it looks."
        )
    if paper:
        log.info("next: real history first: %s", real)
    else:
        log.info("next: %s", _command(args, f"--backtest --days {args.days}"))
    return 0


def _check(config: BotConfig, args: argparse.Namespace, log: logging.Logger) -> int:
    """Run the four-layer check and print the verdict.  Offline."""
    name = args.strategy or config.trading.strategy
    # The strategy talks a lot while it is being probed a few thousand
    # times; none of it belongs in the bot's log.
    quiet = logging.getLogger("tradebot_check_quiet")
    quiet.propagate = False
    if not quiet.handlers:
        quiet.addHandler(logging.NullHandler())

    def make():
        return build_strategy(name, config, quiet)

    try:
        make()
    except UnknownStrategy as exc:
        log.error("%s", exc)
        return 2
    window = _window(args, log)
    if window is None:
        return 2
    report = run_check(
        name,
        make,
        config,
        data_dir=Path(args.data_dir),
        window=window,
        fetch=_command(args, f"--live --fetch-candles --days {args.days}"),
        log=quiet,
    )
    print(report.text())
    folder = Path(args.log_file).parent if args.log_file else Path("logs")
    print()
    print(f"report: {write_check(report, folder)}")
    return 0 if report.passed else 1


def _strategy_listing(args: argparse.Namespace) -> dict[str, Any]:
    """Every registered strategy, its source file, and whether it may trade live.

    The board shows this as a picker, so a student's own strategy appears next
    to the worked examples as soon as it is registered. ``check`` comes from
    the same :func:`check_clearance` that ``--live`` asks, and is ``None`` when
    the config cannot be read - the picker then says "unknown", never "passed".
    """
    config: BotConfig | None = None
    path = args.config if args.config and Path(args.config).exists() else None
    try:
        config = load_config(path, allow_missing_env=True)
    except ConfigError:
        config = None
    folder = Path(args.log_file).parent if args.log_file else Path("logs")
    rows = []
    for name, cls in sorted(STRATEGIES.items()):
        source = inspect.getsourcefile(cls)
        try:
            file = Path(source).resolve().relative_to(Path.cwd().resolve()).as_posix()
        except (TypeError, ValueError):
            file = source or ""
        check = None
        if config is not None:
            why = check_clearance(name, cls, config, folder)
            check = {"passed": why is None, "why": why or ""}
        rows.append(
            {"name": name, "description": cls.description or "", "file": file, "check": check}
        )
    return {"config": args.config if path else None, "strategies": rows}


def _cleared(config: BotConfig, args: argparse.Namespace, log: logging.Logger) -> int | None:
    """None if this strategy may trade live; otherwise the exit code, refusal logged."""
    name = args.strategy or config.trading.strategy
    try:
        cls = type(build_strategy(name, config, log))
    except UnknownStrategy as exc:
        log.error("%s", exc)
        return 2
    folder = Path(args.log_file).parent if args.log_file else Path("logs")
    why = check_clearance(name, cls, config, folder)
    if why is None:
        log.info("--check: '%s' passed on this file and this market", name)
        return None
    log.error("not sent: %s. Nothing was sent to %s.", why, config.exchange)
    log.error("  run the check first (offline, seconds): %s", _command(args, "--check"))
    log.error("  it must say VERDICT  PASS; then this same command will start.")
    return 2


def _command(args: argparse.Namespace, rest: str) -> str:
    """The command to type next, naming the config this run used.

    Without ``--config`` the next command silently reads the default config,
    which may name another strategy and another venue than this run did.
    """
    if args.config == DEFAULT_CONFIG:
        return f"tradebot {rest}"
    return f"tradebot --config {args.config} {rest}"


def _backtest(config: BotConfig, args: argparse.Namespace, log: logging.Logger) -> int:
    """Replay stored candles through the real risk gate.  Offline."""
    window = _window(args, log)
    if window is None:
        return 2
    start, end = window
    symbol = config.trading.symbol
    interval = config.trading.timeframe

    store = CandleStore(args.data_dir)
    candles = store.load(config.exchange, symbol, interval, start, end)
    if not candles:
        coverage = store.coverage(config.exchange, symbol, interval)
        log.error("no stored candles for %s %s in that period.", symbol, interval)
        log.error("  store: %s", coverage.describe())
        log.error("  fix:   %s", _command(args, f"--live --fetch-candles --days {args.days}"))
        return 2

    try:
        strategy = build_strategy(args.strategy or config.trading.strategy, config, log)
    except UnknownStrategy as exc:
        log.error("%s", exc)
        return 2

    try:
        result = run_backtest(candles, strategy, config, log=log, exchange=config.exchange)
    except BacktestError as exc:
        log.error("%s", exc)
        return 2

    print(summary(result))
    if args.trades:
        print()
        print(trade_table(result.trades))
    if args.backtest_out:
        written = write_json(result, args.backtest_out)
        print()
        print(f"machine-readable result: {written}")
    return 0


def _doctor(
    engine: Engine,
    log: logging.Logger,
    paper: bool = True,
    has_keys: bool = False,
) -> int:
    """Read-only health check: config, connection, market, account.

    Says which broker it asked.  Without that line a run with no credentials
    answers "all good" about the paper broker, and the person reading it
    believes their exchange account is connected - the one conclusion this
    command exists to prevent.
    """
    symbol = engine.config.trading.symbol
    problems = 0
    log.info("--- doctor ---")
    if paper:
        log.info("broker       paper (offline simulator), not %s", engine.config.venue_name)
        if not has_keys:
            log.warning(
                "no credentials in this config, so nothing here proves your %s "
                "account works. Fill the keys in, then run: tradebot --doctor "
                "--live --config <file>",
                engine.config.exchange,
            )
    else:
        log.info(
            "broker       %s %s (live connection)",
            engine.config.exchange,
            engine.config.venue_name,
        )
    try:
        price = engine.broker.fetch_mark_price(symbol)
        log.info("price        %s = %s", symbol, price)
    except BrokerError as exc:
        log.error("price        FAILED: %s", exc)
        problems += 1
    try:
        spec = engine.broker.fetch_instrument(symbol)
        log.info(
            "instrument   tick %s, lot step %s, min notional %s",
            spec.tick_size,
            spec.lot_step,
            spec.min_notional,
        )
    except BrokerError as exc:
        log.error("instrument   FAILED: %s", exc)
        problems += 1
    try:
        log.info("equity       %s", engine.broker.fetch_equity())
    except BrokerError as exc:
        log.error("equity       FAILED: %s", exc)
        problems += 1
    try:
        positions = engine.broker.fetch_positions()
        log.info("positions    %s", positions or "none")
    except BrokerError as exc:
        log.error("positions    FAILED: %s", exc)
        problems += 1
    fetch = getattr(engine.broker, "fetch_candles", None)
    if callable(fetch):
        try:
            candles = fetch(symbol, engine.config.trading.timeframe, 5)
            log.info(
                "candles      %s %s bars available, last close %s",
                len(candles),
                engine.config.trading.timeframe,
                candles[-1].close if candles else "-",
            )
        except BrokerError as exc:
            log.error("candles      FAILED: %s", exc)
            problems += 1
    else:
        log.warning("candles      this broker has no candle feed")
    if engine.state.halted:
        log.error(
            "state        HALTED: %s (clear it with %s)",
            engine.state.halt_reason,
            "--resume" if paper else "--live --resume",
        )
        problems += 1
    else:
        log.info(
            "state        ready (day %s, peak equity %s)",
            engine.state.day or "-",
            engine.state.peak_equity,
        )
    log.info("--- %s ---", "all good" if problems == 0 else f"{problems} problem(s)")
    return 0 if problems == 0 else 1


if __name__ == "__main__":
    sys.exit(main())
