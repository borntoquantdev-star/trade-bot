"""The line tracer: what it records, what it refuses to record, and when it
gets out of the way.

Everything here is offline and takes microseconds.  The one thing to know
before reading: ``sys.settrace`` is a single slot per thread, so these tests
are skipped under a profiler or ``pytest --cov`` rather than fighting it for
the slot - which is exactly what the tracer itself does.
"""

import inspect
import json
import random
import sys
from datetime import datetime, timedelta, timezone
from decimal import Decimal

import pytest

from tradebot.brokers.paper import PaperBroker, fixed_price
from tradebot.core.config import load_config
from tradebot.core.engine import Engine
from tradebot.money import D
from tradebot.strategies.random_strategy import RandomStrategy
from tradebot.trace import SELF, Trace

SYMBOL = "BTC_USDT_Perp"
START = datetime(2026, 11, 13, 12, 0, tzinfo=timezone.utc)

#: Another tracer already holds the slot - coverage.py, a debugger, a
#: profiler.  Nothing here can run, and neither can the feature.
taken = pytest.mark.skipif(
    sys.gettrace() is not None,
    reason="another tracer holds sys.settrace (coverage or a debugger)",
)


def add_a_few() -> Decimal:
    """Run some of the package's own code, in a loop, on purpose."""
    total = D("0")
    for _ in range(3):
        total += D("1.5")
    return total


# ----------------------------------------------------------- the recording


@taken
def test_records_the_lines_the_package_really_ran():
    with Trace() as trace:
        add_a_few()
    out = trace.as_json()
    assert out is not None
    assert "tradebot/money.py" in out["lines"]
    assert any(call["func"] == "D" for call in out["calls"])


@taken
def test_counts_a_loop_once_per_line_rather_than_keeping_every_trip():
    with Trace() as trace:
        add_a_few()
    counts = trace.lines["tradebot/money.py"]
    assert max(counts.values()) >= 3, "the loop ran three times"
    assert len(counts) < 20, "one entry per line, not one per trip"


@taken
def test_records_the_order_the_lines_ran_in_not_only_which_ones_did():
    """A set of lines says what a tick touched; the order says what it did.

    Walking a card down the path is the whole point: counting by itself
    cannot tell a loop from a straight line through the same lines.
    """
    with Trace() as trace:
        add_a_few()
    out = trace.as_json()
    path = out["path"]
    assert len(path) == out["steps"]
    assert all(0 <= index < len(out["files"]) for index, _ in path)
    # The loop body appears more than once, and not all in a row: the `for`
    # line comes back round between trips.
    numbers = [number for _, number in path]
    assert len(numbers) > len(set(numbers)), "some line ran twice"


@taken
def test_the_path_and_the_counts_agree_when_nothing_was_dropped():
    with Trace() as trace:
        add_a_few()
    out = trace.as_json()
    from collections import Counter

    counted = Counter((out["files"][index], str(number)) for index, number in out["path"])
    for file, lines in out["lines"].items():
        for number, count in lines.items():
            assert counted[(file, number)] == count, f"{file}:{number}"


@taken
def test_the_path_has_a_ceiling_of_its_own():
    """The counts survive a tick that spins a huge loop; the path cannot,
    so it stops and says so rather than growing without limit."""
    with Trace(max_steps=5) as trace:
        add_a_few()
    out = trace.as_json()
    assert len(out["path"]) == 5
    assert out["full"] is True
    assert out["steps"] > 5, "the counting carried on"


@taken
def test_keeps_nothing_from_outside_the_package():
    with Trace() as trace:
        json.dumps({"a": 1})
        "x".upper()
    assert all(name.startswith("tradebot/") for name in trace.lines)


@taken
def test_never_records_itself():
    with Trace() as trace:
        add_a_few()
    assert SELF not in trace.lines
    assert all(call["file"] != SELF for call in trace.as_json()["calls"])


# -------------------------------------------------------------- the limits


@taken
def test_the_ceiling_stops_the_row_growing_without_limit():
    with Trace(max_lines=2) as trace:
        add_a_few()
    kept = sum(len(counts) for counts in trace.lines.values())
    assert kept == 2
    assert trace.as_json()["full"] is True


@taken
def test_disabled_records_nothing_and_touches_no_slot():
    with Trace(enabled=False) as trace:
        assert sys.gettrace() is None
        add_a_few()
    assert trace.as_json() is None


@taken
def test_puts_the_slot_back_when_it_is_done():
    with Trace():
        assert sys.gettrace() is not None
    assert sys.gettrace() is None


@taken
def test_stopping_twice_is_not_an_error():
    trace = Trace()
    with trace:
        add_a_few()
    trace.stop()
    assert sys.gettrace() is None


def test_leaves_another_tracer_alone():
    """A debugger has a better claim on the slot than a card does."""

    def other(frame, event, arg):
        return other

    sys.settrace(other)
    try:
        with Trace() as trace:
            add_a_few()
        assert sys.gettrace() is other, "the other tracer is still installed"
        assert trace.as_json() is None
    finally:
        sys.settrace(None)


# ------------------------------------------------------- through the engine


def engine(tmp_path, trace: bool) -> Engine:
    config = load_config(
        data={"trading": {"symbol": SYMBOL, "loop_interval": 1, "warmup_candles": 0}},
        env={},
    )
    ticks = {"now": START}

    def clock():
        ticks["now"] += timedelta(seconds=5)
        return ticks["now"]

    return Engine(
        config=config,
        broker=PaperBroker(fixed_price(100_000), config.instrument, equity=10_000),
        # A strategy from the package, not one defined up here: only the
        # package's own frames are recorded, which is the point.
        strategy=RandomStrategy(signal_chance=1.0, rng=random.Random(1)),
        state_path=tmp_path / "state.json",
        audit_path=tmp_path / "audit.jsonl",
        clock=clock,
        trace=trace,
    )


def rows(tmp_path) -> list[dict]:
    text = (tmp_path / "audit.jsonl").read_text(encoding="utf-8")
    return [json.loads(line) for line in text.splitlines() if line.strip()]


@taken
def test_the_row_carries_the_path_through_the_files_it_names(tmp_path):
    bot = engine(tmp_path, trace=True)
    bot.start()
    bot.tick()
    trace = rows(tmp_path)[-1]["trace"]
    assert trace["path"], "a tick with no steps is not a tick"
    first = trace["path"][0]
    assert trace["files"][first[0]] == "tradebot/core/engine.py", "it starts in the loop"
    assert first[1] >= 1


@taken
def test_the_audit_row_carries_the_lines_that_tick_ran(tmp_path):
    bot = engine(tmp_path, trace=True)
    bot.start()
    bot.tick()
    row = rows(tmp_path)[-1]
    trace = row["trace"]
    lines = trace["lines"]["tradebot/core/engine.py"]
    # `tick` opened the trace, so it was already running when settrace went
    # in.  It is here only because the tracer hooks that frame by hand.
    reading = [call for call in trace["calls"] if call["func"] == "tick"]
    assert reading, "the loop itself is in the picture"
    assert lines, "and so are its lines"
    assert "tradebot/risk.py" in trace["lines"], "the gate ran, so it is in the row"
    assert trace["steps"] > 0


@taken
def test_the_row_says_which_strategy_function_ran(tmp_path):
    bot = engine(tmp_path, trace=True)
    bot.start()
    bot.tick()
    calls = rows(tmp_path)[-1]["trace"]["calls"]
    assert any(call["func"] == "get_signal" for call in calls)


@taken
def test_writing_the_row_is_not_part_of_the_picture_of_the_tick(tmp_path):
    """The tracer stops before the row is built, or it would be mostly a
    picture of the code that builds rows."""
    bot = engine(tmp_path, trace=True)
    bot.start()
    bot.tick()
    row = rows(tmp_path)[-1]
    engine_lines = {int(n) for n in row["trace"]["lines"]["tradebot/core/engine.py"]}
    source, first = inspect.getsourcelines(Engine._record)
    writing = [first + offset for offset, line in enumerate(source) if "handle.write(" in line]
    assert writing, "found the line that writes the row"
    assert not set(writing) & engine_lines, "the writing is not in the picture"


def test_the_state_file_says_which_strategy_and_broker_are_running(tmp_path):
    """So a dashboard shows the code this bot runs, not the code some config
    file on disk happens to name."""
    bot = engine(tmp_path, trace=False)
    bot.start()
    state = json.loads((tmp_path / "state.json").read_text(encoding="utf-8"))
    assert state["strategy_module"] == "tradebot.strategies.random_strategy"
    assert state["broker_module"] == "tradebot.brokers.paper"


def test_what_is_running_is_rewritten_rather_than_remembered(tmp_path):
    """A restart with a different strategy must not leave the old name
    behind: it is a statement about this process, not saved state."""
    first = engine(tmp_path, trace=False)
    first.start()
    (tmp_path / "state.json").write_text(
        json.dumps({"strategy_module": "tradebot.strategies.ma_cross"}),
        encoding="utf-8",
    )
    second = engine(tmp_path, trace=False)
    second.start()
    state = json.loads((tmp_path / "state.json").read_text(encoding="utf-8"))
    assert state["strategy_module"] == "tradebot.strategies.random_strategy"


def test_nothing_is_recorded_when_it_is_switched_off(tmp_path):
    bot = engine(tmp_path, trace=False)
    bot.start()
    bot.tick()
    assert "trace" not in rows(tmp_path)[-1]
    assert sys.gettrace() is None


def test_flattening_with_nothing_open_writes_nothing(tmp_path):
    bot = engine(tmp_path, trace=True)
    bot.start()
    assert bot.flatten("end") is None
    assert not (tmp_path / "audit.jsonl").exists() or not rows(tmp_path)


@taken
def test_the_closing_row_carries_its_own_trace_not_the_last_tick_s(tmp_path):
    """A stale trace on the closing row would show the board the code of the
    tick before it, walking through a decision that was not this one."""
    bot = engine(tmp_path, trace=True)
    bot.start()
    bot.tick()  # random with signal_chance=1.0 opens here
    result = bot.flatten("end")
    assert result is not None
    last = rows(tmp_path)[-1]
    assert last["exit"]["reason"] == "end"
    funcs = {call["func"] for call in last["trace"]["calls"]}
    assert "flatten" in funcs
    assert "get_signal" not in funcs, "not the trace of the tick before"


# --- the trace file -------------------------------------------------------------


def engine_with_file(tmp_path) -> Engine:
    bot = engine(tmp_path, trace=True)
    bot.trace_path = tmp_path / "trace.jsonl"
    return bot


def trace_lines(tmp_path) -> list[dict]:
    text = (tmp_path / "trace.jsonl").read_text(encoding="utf-8")
    return [json.loads(line) for line in text.splitlines() if line.strip()]


@taken
def test_with_a_trace_file_the_audit_row_stays_short_and_points_at_it(tmp_path):
    """The audit trail is read by people, line by line; a trace is fifteen
    times the size of the rest of the row and belongs in its own file."""
    bot = engine_with_file(tmp_path)
    bot.start()
    bot.tick()
    row = rows(tmp_path)[-1]
    assert "trace" not in row, "the heavy part is not in the readable file"
    assert row["traced"] is True
    traces = trace_lines(tmp_path)
    assert traces[-1]["at"] == row["at"], "found by the row's own timestamp"
    assert traces[-1]["trace"]["path"], "and it is the whole trace"
    assert len(json.dumps(row)) < 600, "short enough to read"


@taken
def test_a_trace_file_that_grew_too_big_is_rolled_over(tmp_path, monkeypatch):
    from tradebot.core import engine as engine_module

    monkeypatch.setattr(engine_module, "TRACE_ROTATE_BYTES", 10)
    bot = engine_with_file(tmp_path)
    bot.start()
    bot.tick()
    bot.tick()
    assert (tmp_path / "trace.jsonl.1").exists(), "the full one was moved aside"
    assert len(trace_lines(tmp_path)) == 1, "and a fresh one started"


@taken
def test_a_trace_that_could_not_be_written_leaves_no_mark_on_the_row(tmp_path):
    """A mark pointing at a trace that is not there would have the board
    waiting for something that will never arrive."""
    bot = engine(tmp_path, trace=True)
    (tmp_path / "a-directory").mkdir()
    bot.trace_path = tmp_path / "a-directory"
    bot.start()
    bot.tick()
    row = rows(tmp_path)[-1]
    assert "traced" not in row
    assert "trace" not in row
