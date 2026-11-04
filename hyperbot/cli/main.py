"""Command line entry point.

    hyperbot --dry-run                 paper trade, no credentials needed
    hyperbot --doctor                  check the config and the connection
    hyperbot --list-strategies         what is registered
    hyperbot --live                    real orders (prod requires this flag)
    hyperbot --resume                  clear a halt written by the risk gate
    hyperbot --close --yes             flatten the configured symbol

``--dry-run`` is the default when ``hyperliquid.network`` is ``testnet`` and no ``--live``
flag is given, so the accident path leads to the paper broker rather than to
the exchange.
"""

from __future__ import annotations

import argparse
import logging
import sys
from collections.abc import Sequence
from pathlib import Path

from .. import __version__
from ..brokers.base import BrokerError
from ..brokers.paper import PaperBroker, fixed_price
from ..core.config import BotConfig, ConfigError, describe, load_config
from ..core.engine import Engine
from ..money import D
from ..strategies import UnknownStrategy, build_strategy, describe_strategies
from ..utils.logger import setup_logger

DEFAULT_CONFIG = "config/config.yaml"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="hyperbot",
        description="Trading bot for Hyperliquid perpetuals with a pure, testable risk gate.",
        epilog="Read docs/vscode.md for the editor setup and docs/architecture.md for the design.",
    )
    parser.add_argument("--version", action="version", version=f"hyperbot {__version__}")
    parser.add_argument(
        "--config", default=DEFAULT_CONFIG, help=f"config file (default: {DEFAULT_CONFIG})"
    )
    parser.add_argument(
        "--strategy", default=None, help="override trading.strategy from the config"
    )
    parser.add_argument("--log-file", default="logs/hyperbot.log", help="log file path")
    parser.add_argument(
        "--log-level", default="INFO", choices=["DEBUG", "INFO", "WARNING", "ERROR"]
    )
    parser.add_argument(
        "--state-file",
        default="logs/state.json",
        help="where the daily budget and halt flag live",
    )
    parser.add_argument(
        "--audit-file", default="logs/audit.jsonl", help="one JSON line per decision"
    )
    parser.add_argument(
        "--iterations", type=int, default=None, help="stop after N loops (default: run forever)"
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
        help="send real orders; required when hyperliquid.network is 'mainnet'",
    )
    mode.add_argument("--paper-price", default="100000", help="starting price for the paper broker")

    actions = parser.add_argument_group("one-shot actions")
    actions.add_argument(
        "--list-strategies", action="store_true", help="print the registry and exit"
    )
    actions.add_argument(
        "--doctor", action="store_true", help="check config and connectivity, then exit"
    )
    actions.add_argument("--resume", action="store_true", help="clear a halt and exit")
    actions.add_argument(
        "--close", action="store_true", help="flatten the configured symbol and exit"
    )
    actions.add_argument("--yes", action="store_true", help="confirm an action that moves money")
    return parser


def _load(args: argparse.Namespace, log: logging.Logger) -> BotConfig | None:
    path: str | None = args.config
    if path and not Path(path).exists():
        if args.config != DEFAULT_CONFIG:
            log.error("config file not found: %s", path)
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
        log.error("%s", exc)
        return None


def _make_broker(config: BotConfig, paper: bool, log: logging.Logger, paper_price: str = "100000"):
    if paper:
        return PaperBroker(
            price_source=fixed_price(D(paper_price)),
            instrument=config.instrument,
            equity=config.starting_equity,
            logger=log,
        )
    from ..brokers.hyperliquid import HyperliquidBroker  # imported late: needs the SDK

    config.require_live_credentials()
    return HyperliquidBroker(
        settings=config.hyperliquid,
        logger=log,
        min_notional=config.instrument.min_notional,
    )


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)

    if args.list_strategies:
        print("Registered strategies:\n")
        print(describe_strategies())
        return 0

    log = setup_logger("hyperbot", args.log_file, getattr(logging, args.log_level))
    log.info("hyperbot %s", __version__)

    config = _load(args, log)
    if config is None:
        return 2

    # Decide paper vs live, and refuse to touch production by accident.
    paper = args.dry_run or not args.live
    if config.hyperliquid.is_live and not args.live:
        log.error(
            "hyperliquid.network is 'mainnet' but --live was not passed. Nothing "
            "was sent. Run with --dry-run to paper trade, or add --live if you "
            "really mean real money on a real account."
        )
        return 2
    if args.dry_run and args.live:
        log.error("--dry-run and --live contradict each other; pick one")
        return 2

    log.info("configuration:\n%s", describe(config))
    log.info(
        "broker: %s",
        "paper (offline)" if paper else f"Hyperliquid {config.hyperliquid.network}",
    )

    try:
        broker = _make_broker(config, paper, log, args.paper_price)
    except (BrokerError, ConfigError) as exc:
        log.error("%s", exc)
        return 2

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
        state_path=Path(args.state_file) if args.state_file else None,
        audit_path=Path(args.audit_file) if args.audit_file else None,
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
        return _doctor(engine, log)

    if args.close:
        if not args.yes:
            log.error("--close moves money. Re-run with --close --yes to confirm.")
            return 2
        receipt = broker.close_position(config.trading.symbol)
        log.info("close result: %s", receipt or "nothing was open")
        return 0

    if not paper:
        log.warning(
            "LIVE MODE on %s. The bot caps exposure at %sx equity; the account's "
            "own leverage setting is separate (hyperbot does not change it unless "
            "you call set_leverage yourself).",
            config.hyperliquid.network,
            config.risk.max_leverage,
        )

    return engine.run(max_iterations=args.iterations)


def _doctor(engine: Engine, log: logging.Logger) -> int:
    """Read-only health check: config, connection, market, account."""
    symbol = engine.config.trading.symbol
    problems = 0
    log.info("--- doctor ---")
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
        log.error("state        HALTED: %s (clear it with --resume)", engine.state.halt_reason)
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
