"""``tradebot --check``: is this strategy fit to send orders to testnet?

One command, four layers, and a machine that gives the verdict - not the
person who wrote the strategy, and not the AI that helped them. Each layer
answers a different way a strategy goes wrong once it trades:

1. **Contract** - the rules every strategy must keep, probed on a dozen
   synthetic price series: it does not crash, does not touch the network or
   the disk, puts every stop on the losing side, dates its signal with
   ``market.now``, and - the one most often missing - asks to close a
   position whose stop has been crossed. The live loop only knows the price
   it just read; if the strategy does not watch its own stop, nothing does.
2. **Tests** - the strategy's own tests, which say whether it does what its
   author designed. The contract cannot know that; only they can.
3. **Replay** - the stored real candles through the real risk gate: does it
   fire at all on real prices, and are its signals refused, and why?
4. **Paper** - a short run of the real engine on the paper broker: the
   wiring, end to end, with nothing to lose.

The verdict is about **correctness, not profit**. A strategy that passes does
what it says without breaking anything; whether what it says makes money is
the backtest's question and the market's, and the report says so.

The fingerprint is a hash of the strategy's source file. A pass belongs to
that exact file: change one character and it has to be checked again.
"""

from __future__ import annotations

import builtins
import contextlib
import hashlib
import inspect
import io
import json
import logging
import math
import random
import socket
import subprocess
import sys
import tempfile
from collections.abc import Callable, Iterator
from dataclasses import dataclass, field, replace
from datetime import datetime, timedelta, timezone
from decimal import Decimal
from pathlib import Path
from typing import Any

from ..backtest import run_backtest
from ..backtest.runner import BacktestError
from ..brokers.paper import PaperBroker, random_walk
from ..core.config import BotConfig
from ..core.engine import Engine
from ..data.store import CandleStore
from ..money import D
from ..strategies.base import MarketView, Strategy
from ..types import Position, Side, Signal

PASS = "pass"
WARN = "warn"
FAIL = "FAIL"
SKIP = "not run"

#: Ticks the paper layer runs. Enough for a window-based strategy to warm up
#: from the broker's candles and act a few times.
PAPER_TICKS = 30

#: How many prices a strategy sees at once, as the engine gives it.
HISTORY = 200

T0 = datetime(2026, 1, 1, tzinfo=timezone.utc)
EQUITY = D("10000")


@dataclass
class Finding:
    layer: str
    name: str
    status: str
    detail: str = ""


@dataclass
class Report:
    strategy: str
    class_name: str
    source: str
    fingerprint: str
    #: "exchange symbol timeframe": the replay layer only vouched for this one.
    market: str = ""
    findings: list[Finding] = field(default_factory=list)

    def add(self, layer: str, name: str, status: str, detail: str = "") -> None:
        self.findings.append(Finding(layer, name, status, detail))

    @property
    def failed(self) -> list[Finding]:
        return [f for f in self.findings if f.status == FAIL]

    @property
    def missing(self) -> list[Finding]:
        return [f for f in self.findings if f.status == SKIP]

    @property
    def passed(self) -> bool:
        """Nothing failed and nothing was left unchecked."""
        return not self.failed and not self.missing

    def to_dict(self) -> dict[str, Any]:
        return {
            "strategy": self.strategy,
            "class": self.class_name,
            "source": self.source,
            "fingerprint": self.fingerprint,
            "market": self.market,
            "checked_at": datetime.now(timezone.utc).isoformat(),
            "passed": self.passed,
            "findings": [vars(f) for f in self.findings],
        }

    def text(self) -> str:
        where = f"  on {self.market}" if self.market else ""
        lines = [
            f"CHECK  {self.strategy}  ({self.class_name}, {self.source})",
            f"       fingerprint {self.fingerprint}{where}",
        ]
        titles = {
            "contract": "1 CONTRACT  rules every strategy must keep",
            "tests": "2 TESTS     the strategy's own tests",
            "replay": "3 REPLAY    real candles through the real risk gate",
            "paper": "4 PAPER     the real engine on the paper broker",
        }
        for layer, title in titles.items():
            rows = [f for f in self.findings if f.layer == layer]
            if not rows:
                continue
            lines += ["", title]
            for row in rows:
                lines.append(f"  {row.status:<7} {row.name}")
                if row.detail:
                    lines.append(f"          {row.detail}")
        lines.append("")
        if self.passed:
            warns = sum(1 for f in self.findings if f.status == WARN)
            lines.append(
                "VERDICT  PASS - fit for testnet"
                + (f" ({warns} warning(s) worth reading)" if warns else "")
            )
            lines.append(
                "         This says the strategy does what it says without breaking "
                "anything. It says nothing about profit: see docs/backtest.md."
            )
        else:
            parts = []
            if self.failed:
                parts.append(f"{len(self.failed)} failed")
            if self.missing:
                parts.append(f"{len(self.missing)} not run")
            lines.append(f"VERDICT  NOT YET - {', '.join(parts)}. Fix those before --live.")
        return "\n".join(lines)


# --- the contract --------------------------------------------------------


def scenarios(base: Decimal) -> dict[str, list[Decimal]]:
    """Price series that make most rules speak: trends, waves, jumps, noise."""
    b = float(base)
    series: dict[str, list[float]] = {
        "flat": [b] * 120,
        "steady rise": [b * (1 + 0.10 * i / 299) for i in range(300)],
        "steady fall": [b * (1 - 0.10 * i / 299) for i in range(300)],
        "slow wave": [b * (1 + 0.02 * math.sin(i / 8)) for i in range(300)],
        "fast wave": [b * (1 + 0.003 * math.sin(i / 1.5)) for i in range(300)],
        "jump up": [b] * 150 + [b * 1.08] * 150,
        "jump down": [b] * 150 + [b * 0.92] * 150,
        "one spike": [b] * 100 + [b * 1.2] + [b] * 99,
    }
    for seed in (1, 2, 3):
        rng = random.Random(seed)
        price, walk = b, []
        for _ in range(400):
            price *= 1 + rng.gauss(0, 0.002)
            walk.append(price)
        series[f"random walk {seed}"] = walk
    return {name: [D(f"{p:.2f}") for p in prices] for name, prices in series.items()}


class Forbidden(Exception):
    """Raised inside the strategy when it reaches for the network or a file."""


@contextlib.contextmanager
def no_io(attempts: list[str]) -> Iterator[None]:
    """Make network and file access fail loudly, and remember who tried."""
    real_open, real_io_open = builtins.open, io.open
    real_connect, real_create = socket.socket.connect, socket.create_connection

    def refuse(what: str) -> Callable[..., Any]:
        def blocked(*args: Any, **kwargs: Any) -> Any:
            attempts.append(what)
            raise Forbidden(f"{what} is not allowed in a strategy")

        return blocked

    builtins.open = refuse("opening a file")  # type: ignore[assignment]
    io.open = refuse("opening a file")  # type: ignore[assignment]
    socket.socket.connect = refuse("a network connection")  # type: ignore[method-assign]
    socket.create_connection = refuse("a network connection")  # type: ignore[assignment]
    try:
        yield
    finally:
        builtins.open, io.open = real_open, real_io_open
        socket.socket.connect = real_connect  # type: ignore[method-assign]
        socket.create_connection = real_create


@dataclass
class Walk:
    """What a strategy did along one series, and what it got wrong."""

    signals: list[tuple[MarketView, Signal]] = field(default_factory=list)
    decisions: list[tuple[Any, ...]] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)
    chart_errors: list[str] = field(default_factory=list)
    wrong_side: list[str] = field(default_factory=list)
    wrong_time: list[str] = field(default_factory=list)
    wrong_symbol: list[str] = field(default_factory=list)
    far_ref: list[str] = field(default_factory=list)
    no_reason: int = 0
    while_holding: int = 0
    stop_ignored: list[str] = field(default_factory=list)


def _losing_side_ok(signal: Signal) -> bool:
    if signal.side is Side.BUY:
        return signal.stop_price < signal.ref_price
    return signal.stop_price > signal.ref_price


def walk(strategy: Strategy, prices: list[Decimal], symbol: str, chart: bool) -> Walk:
    """Feed *prices* one at a time, holding whatever the strategy opens.

    The position is played the way the live loop plays it: ``get_exit``
    first while holding, then ``get_signal``. A position whose stop is
    crossed without the strategy asking out is recorded and then dropped,
    so one miss does not hide what happens later in the series.
    """
    out = Walk()
    held: Position | None = None
    # None when the signal put its stop on the wrong side: that is its own
    # failure, and "did it watch a stop that makes no sense" is not a question.
    stop: Decimal | None = None
    try:
        strategy.initialize()
    except Exception as exc:
        out.errors.append(f"initialize() raised {type(exc).__name__}: {exc}")

    def call(name: str, fn: Callable[[MarketView], Any], market: MarketView) -> Any:
        try:
            return fn(market)
        except Forbidden:
            return None
        except Exception as exc:  # the point is to catch anything
            out.errors.append(f"{name}() raised {type(exc).__name__}: {exc}")
            return None

    for index, price in enumerate(prices):
        window = tuple(prices[max(0, index + 1 - HISTORY) : index + 1])
        now = T0 + timedelta(minutes=index)
        market = MarketView(symbol, price, now, EQUITY, held, window)

        if held is not None:
            reason = call("get_exit", strategy.get_exit, market)
            crossed = stop is not None and (
                price <= stop if held.side is Side.BUY else price >= stop
            )
            if reason:
                out.decisions.append((index, "exit"))
                held = None
            elif crossed:
                out.stop_ignored.append(f"held {held.side.value} with stop {stop}, price {price}")
                held = None
            market = replace(market, position=held)

        signal = call("get_signal", strategy.get_signal, market)
        if isinstance(signal, Signal):
            if held is not None:
                out.while_holding += 1
            else:
                out.decisions.append((index, signal.side.value, str(signal.stop_price)))
                out.signals.append((market, signal))
                if not _losing_side_ok(signal):
                    out.wrong_side.append(
                        f"{signal.side.value} at {signal.ref_price} with stop {signal.stop_price}"
                    )
                if signal.created_at != market.now:
                    out.wrong_time.append(f"{signal.created_at} for a market at {market.now}")
                if signal.symbol != market.symbol:
                    out.wrong_symbol.append(signal.symbol)
                if abs(signal.ref_price - price) > price * D("0.005"):
                    out.far_ref.append(f"ref {signal.ref_price} with the market at {price}")
                if not signal.reason.strip():
                    out.no_reason += 1
                held = Position(symbol, signal.side, D(1), signal.ref_price)
                stop = signal.stop_price if _losing_side_ok(signal) else None
        elif signal is not None:
            out.errors.append(f"get_signal() returned {type(signal).__name__}, not a Signal")

        if chart:
            try:
                levels = strategy.chart(market)
                for key, value in (levels or {}).items():
                    D(value)  # must be a number
                    str(key)
            except Forbidden:
                pass
            except Exception as exc:
                out.chart_errors.append(f"chart() raised {type(exc).__name__}: {exc}")
    return out


def _overrides_chart(strategy: Strategy) -> bool:
    return type(strategy).chart is not Strategy.chart


def check_contract(
    report: Report, make: Callable[[], Strategy], symbol: str, base: Decimal
) -> None:
    add = lambda *a: report.add("contract", *a)  # noqa: E731
    try:
        make()
    except Exception as exc:
        add("builds from the config", FAIL, f"{type(exc).__name__}: {exc}")
        return
    add("builds from the config", PASS)

    attempts: list[str] = []
    series = scenarios(base)
    walks: dict[str, Walk] = {}
    charts = _overrides_chart(make())
    with no_io(attempts):
        for name, prices in series.items():
            walks[name] = walk(make(), prices, symbol, chart=charts)
        # Warm-up: nothing at all to look at must not crash it.
        empty = walk(make(), [base], symbol, chart=False)
        walks["one price only"] = empty

    every = list(walks.values())
    errors = [f"{name}: {e}" for name, w in walks.items() for e in w.errors]
    add(
        "never crashes",
        FAIL if errors else PASS,
        (
            errors[0] + (f" (and {len(errors) - 1} more)" if len(errors) > 1 else "")
            if errors
            else f"{len(walks)} price series, {sum(len(series[n]) for n in series)} prices"
        ),
    )
    add(
        "no network, no files",
        FAIL if attempts else PASS,
        (
            f"tried {attempts[0]}; a strategy reads only the MarketView it is given"
            if attempts
            else ""
        ),
    )

    signals = sum(len(w.signals) for w in every)
    if signals == 0:
        add(
            "fires on test prices",
            SKIP,
            "no signal on any of the test series, so the rules below could not be "
            "checked here; the replay layer still tries real prices",
        )
        for name in ("stop on the losing side", "dated with market.now", "watches its stop"):
            add(name, SKIP, "needs at least one signal")
        return
    add("fires on test prices", PASS, f"{signals} signal(s) across {len(walks)} series")

    def first(attr: str) -> str:
        for w in every:
            found = getattr(w, attr)
            if found:
                return f"{len(found)} case(s), e.g. {found[0]}"
        return ""

    wrong = first("wrong_side")
    add(
        "stop on the losing side",
        FAIL if wrong else PASS,
        wrong + "; below the entry for a buy, above it for a sell" if wrong else "",
    )
    late = first("wrong_time")
    add(
        "dated with market.now",
        FAIL if late else PASS,
        late + "; pass created_at=market.now, never the wall clock" if late else "",
    )
    stray = first("wrong_symbol")
    if stray:
        add("trades the market it was shown", FAIL, stray)
    far = first("far_ref")
    if far:
        add(
            "ref_price is the market price",
            WARN,
            far + "; the gate measures slippage from it",
        )

    # The stop the live loop will not enforce on its own. Probed directly on
    # a fresh strategy as well as seen along the way.
    sound = [(m, s) for w in every for (m, s) in w.signals if _losing_side_ok(s)]
    if not sound:
        add("watches its stop", SKIP, "needs a signal with its stop on the losing side")
        return
    market, signal = sound[0]
    beyond = signal.stop_price * (D("0.99") if signal.side is Side.BUY else D("1.01"))
    probe = MarketView(
        symbol,
        beyond,
        market.now + timedelta(minutes=1),
        EQUITY,
        Position(symbol, signal.side, D(1), signal.ref_price),
        tuple(market.recent_prices) + (beyond,),
    )
    try:
        with no_io(attempts):
            asked = make().get_exit(probe)
    except Exception as exc:
        asked = None
        errors.append(f"get_exit() raised {type(exc).__name__}: {exc}")
    ignored = first("stop_ignored")
    if not asked or ignored:
        add(
            "watches its stop",
            FAIL,
            (
                ignored
                or f"held {signal.side.value} at {signal.ref_price}, price {beyond} past "
                f"the stop {signal.stop_price}, get_exit() returned {asked!r}"
            )
            + ". The live loop does not stop you out: get_exit() must, e.g. "
            "if self.stop_hit(market, stop): return 'stop hit'",
        )
    else:
        add("watches its stop", PASS, f"asked out with {asked!r}")

    holding = sum(w.while_holding for w in every)
    if holding:
        add(
            "quiet while holding",
            WARN,
            f"{holding} signal(s) while a position was open; the gate refuses them, "
            "but return None when market.in_position",
        )
    unexplained = sum(w.no_reason for w in every)
    if unexplained:
        add("says why", WARN, f"{unexplained} signal(s) with an empty reason")

    # The same prices twice, to two fresh copies, must give the same answers.
    # Then once more with chart() asked every tick: drawing must not change it.
    same = [walk(make(), series[n], symbol, chart=False) for n in ("random walk 1", "slow wave")]
    again = [walk(make(), series[n], symbol, chart=False) for n in ("random walk 1", "slow wave")]
    steady = all(a.decisions == b.decisions for a, b in zip(same, again, strict=True))
    add(
        "same prices, same answers",
        PASS if steady else WARN,
        (
            ""
            if steady
            else "two fresh copies decided differently on identical prices (random numbers? "
            "the wall clock?); a backtest of it cannot be repeated"
        ),
    )
    if charts:
        drawn = [
            walk(make(), series[n], symbol, chart=True) for n in ("random walk 1", "slow wave")
        ]
        if steady and any(a.decisions != b.decisions for a, b in zip(same, drawn, strict=True)):
            add(
                "chart() only draws",
                FAIL,
                "asking chart() every tick changed what the strategy decided; "
                "chart() must not change the strategy's state",
            )
        else:
            broken = next((e for d in drawn for e in d.chart_errors), "")
            add(
                "chart() only draws",
                WARN if broken else PASS,
                broken + " (the board will draw nothing)" if broken else "",
            )


# --- the strategy's own tests ---------------------------------------------


def find_tests(root: Path, cls: type) -> list[Path]:
    """Test files that name this strategy's class or import its module."""
    module = cls.__module__
    short = module.rsplit(".", 1)[-1]
    hits = []
    for path in sorted((root / "tests").glob("test_*.py")):
        # The check's own tests name every strategy; running them from inside
        # a check would run the check again.
        if path.name.startswith("test_check"):
            continue
        text = path.read_text(encoding="utf-8", errors="replace")
        if cls.__name__ in text or module in text or f"strategies.{short}" in text:
            hits.append(path)
    return hits


def check_tests(report: Report, root: Path, cls: type) -> None:
    if not (root / "tests").is_dir():
        report.add("tests", "the strategy's tests", SKIP, f"no tests/ folder under {root}")
        return
    files = find_tests(root, cls)
    if not files:
        report.add(
            "tests",
            "the strategy's tests",
            FAIL,
            f"no file in tests/ mentions {cls.__name__}. Write tests/test_<name>.py: "
            "quiet while warming up, fires exactly on its rule, not one step "
            "before, stop on the losing side, quiet while holding",
        )
        return
    names = ", ".join(path.name for path in files)
    try:
        done = subprocess.run(
            [sys.executable, "-m", "pytest", "-p", "no:cacheprovider"]
            + [str(path) for path in files],
            cwd=root,
            capture_output=True,
            text=True,
            timeout=600,
        )
    except (OSError, subprocess.TimeoutExpired) as exc:
        report.add("tests", "the strategy's tests", FAIL, f"could not run pytest: {exc}")
        return
    # The project's addopts already ask for -q, so the count is on the last
    # line that names one; the dots above it say nothing.
    summary = next(
        (
            line
            for line in reversed(done.stdout.splitlines())
            if any(word in line for word in (" passed", " failed", " error", "no tests ran"))
        ),
        "no summary from pytest",
    )
    report.add(
        "tests",
        "the strategy's tests",
        PASS if done.returncode == 0 else FAIL,
        f"{names}: {summary.strip('= ')}",
    )


# --- real candles ------------------------------------------------------------


def check_replay(
    report: Report,
    make: Callable[[], Strategy],
    config: BotConfig,
    data_dir: Path,
    window: tuple[datetime, datetime],
    fetch: str,
    log: logging.Logger,
) -> Decimal | None:
    """Returns the last real price, for the paper layer to start from."""
    symbol, interval = config.trading.symbol, config.trading.timeframe
    candles = CandleStore(data_dir).load(config.exchange, symbol, interval, *window)
    where = f"{config.exchange} {symbol} {interval}"
    if not candles:
        report.add(
            "replay",
            "real candles",
            SKIP,
            f"none stored for {where} in that period. Fetch them: {fetch}",
        )
        return None
    try:
        result = run_backtest(candles, make(), config, log=log, exchange=config.exchange)
    except BacktestError as exc:
        report.add("replay", "replays", FAIL, str(exc))
        return candles[-1].close
    trades, refused = len(result.trades), sum(result.rejects.values())
    report.add(
        "replay",
        "fires on real prices",
        PASS if result.signals else FAIL,
        (
            f"{result.signals} signal(s), {trades} trade(s) over {result.bars} bars of {where}"
            if result.signals
            else f"no signal in {result.bars} bars of {where}: the rule never comes true "
            "on this market at this timeframe"
        ),
    )
    if result.signals and not trades and refused:
        code, count = result.rejects.most_common(1)[0]
        report.add(
            "replay",
            "some signals get through",
            FAIL,
            f"all {result.signals} refused, mostly {code} ({count}); "
            "see ai/workflows/debug-refusal.md",
        )
    elif refused:
        top = ", ".join(f"{code} {n}" for code, n in result.rejects.most_common(3))
        report.add("replay", "some signals get through", PASS, f"refused: {top}")
    if result.bars and result.signals > result.bars / 2:
        report.add(
            "replay",
            "not every bar",
            WARN,
            f"signals on {result.signals} of {result.bars} bars; fees will eat that",
        )
    if result.halted_at:
        report.add(
            "replay",
            "stays inside the risk limits",
            WARN,
            f"the gate halted it at {result.halted_at:%Y-%m-%d %H:%M} UTC: {result.halt_reason}",
        )
    return candles[-1].close


# --- the real engine, on paper ---------------------------------------------------


def check_paper(
    report: Report,
    make: Callable[[], Strategy],
    config: BotConfig,
    start: Decimal,
    log: logging.Logger,
) -> None:
    with tempfile.TemporaryDirectory() as folder:
        broker = PaperBroker(
            price_source=random_walk(start, "0.08"),
            instrument=config.instrument,
            equity=config.starting_equity,
            logger=log,
        )
        engine = Engine(
            config=config,
            broker=broker,
            strategy=make(),
            logger=log,
            state_path=Path(folder) / "state.json",
            audit_path=Path(folder) / "audit.jsonl",
        )
        errors: list[str] = []
        try:
            engine.start()
            for _ in range(PAPER_TICKS):
                tick = engine.tick()
                if tick.error:
                    errors.append(tick.error)
        except Exception as exc:
            errors.append(f"{type(exc).__name__}: {exc}")
        finally:
            with contextlib.suppress(Exception):
                engine.strategy.cleanup()
    report.add(
        "paper",
        f"{PAPER_TICKS} ticks without an error",
        FAIL if errors else PASS,
        (
            errors[0]
            if errors
            else f"{engine.orders} order(s), {engine.exits} exit(s), {engine.rejects} refused"
        ),
    )


# --- all of it ---------------------------------------------------------------


def fingerprint(cls: type) -> tuple[str, str]:
    """The strategy's source file, relative where possible, and its hash."""
    path = Path(inspect.getsourcefile(cls) or "")
    digest = hashlib.sha256(path.read_bytes()).hexdigest()[:16] if path.is_file() else "unknown"
    root = Path(__file__).resolve().parents[2]
    try:
        shown = path.resolve().relative_to(root).as_posix()
    except ValueError:
        shown = str(path)
    return shown, digest


def run_check(
    name: str,
    make: Callable[[], Strategy],
    config: BotConfig,
    *,
    data_dir: Path,
    window: tuple[datetime, datetime],
    fetch: str,
    root: Path | None = None,
    run_tests: bool = True,
    log: logging.Logger | None = None,
) -> Report:
    quiet = log or logging.getLogger("tradebot.check")
    cls = type(make())
    source, digest = fingerprint(cls)
    report = Report(
        strategy=name,
        class_name=cls.__name__,
        source=source,
        fingerprint=digest,
        market=market_of(config),
    )
    base = D("60000")
    check_contract(report, make, config.trading.symbol, base)
    if run_tests:
        check_tests(report, root or Path(__file__).resolve().parents[2], cls)
    last = check_replay(report, make, config, data_dir, window, fetch, quiet)
    check_paper(report, make, config, last or base, quiet)
    return report


def market_of(config: BotConfig) -> str:
    return f"{config.exchange} {config.trading.symbol} {config.trading.timeframe}"


def report_path(folder: Path, name: str) -> Path:
    return folder / f"check-{name}.json"


def write(report: Report, folder: Path) -> Path:
    folder.mkdir(parents=True, exist_ok=True)
    path = report_path(folder, report.strategy)
    path.write_text(json.dumps(report.to_dict(), indent=2), encoding="utf-8")
    return path


# --- the gate in front of --live ---------------------------------------------


def clearance(name: str, cls: type, config: BotConfig, folder: Path) -> str | None:
    """Why a live run of this strategy may not start yet; None when it may.

    The bot itself asks, not only the plugin that tells people what to type:
    a command typed by hand, or copied from an old chat, goes through here too.
    A pass counts only for the file it read (its fingerprint) and the market
    its replay used, so editing the strategy or pointing it at another market
    needs a fresh check. There is no flag to skip this; the way past it is the
    check, which is offline and takes seconds.
    """
    path = report_path(folder, name)
    try:
        saved = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return f"'{name}' has never passed --check (no {path.as_posix()})"
    except (OSError, ValueError) as exc:
        return f"cannot read {path.as_posix()}: {exc}"
    if not isinstance(saved, dict):
        return f"{path.as_posix()} is not a check report"
    if saved.get("passed") is not True:
        return f"the last --check of '{name}' did not pass (see {path.as_posix()})"
    _, digest = fingerprint(cls)
    if saved.get("fingerprint") != digest:
        return (
            f"'{name}' changed since it passed --check (fingerprint {saved.get('fingerprint')} "
            f"then, {digest} now)"
        )
    market = market_of(config)
    if saved.get("market") != market:
        then = saved.get("market") or "a market this version did not record"
        return f"'{name}' passed --check on {then}, not on {market}"
    return None
