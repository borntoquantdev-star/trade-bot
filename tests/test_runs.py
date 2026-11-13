"""The run log: one line when a run starts, one when it ends.

What the board reads to show a history of runs, so the questions here are
the ones a reader of that history would ask - when did this run start, what
did it do, and did it finish or was it killed.
"""

import json
import random
from decimal import Decimal
from pathlib import Path

from tradebot import runs
from tradebot.cli.main import main


def lines(path: Path) -> list[dict]:
    text = path.read_text(encoding="utf-8")
    return [json.loads(line) for line in text.splitlines() if line.strip()]


def a_start() -> runs.Start:
    return runs.Start(
        strategy="ping_pong",
        exchange="grvt",
        symbol="BTC_USDT_Perp",
        timeframe="1m",
        mode="grvt testnet",
        config="config/testnet.yaml",
        iterations=10,
    )


def test_a_run_writes_an_opening_line_and_a_closing_one(tmp_path):
    path = tmp_path / "runs.jsonl"
    run_id = runs.started(path, a_start())
    runs.finished(
        path,
        run_id,
        ticks=10,
        orders=1,
        exits=1,
        rejects=2,
        first_equity=Decimal("9966.404007"),
        last_equity=Decimal("9957.442304"),
        halted=False,
        reason="finished",
    )
    rows = lines(path)
    assert [row["event"] for row in rows] == ["start", "end"]
    assert rows[0]["id"] == rows[1]["id"] == run_id
    assert rows[0]["strategy"] == "ping_pong"
    assert rows[1]["ticks"] == 10


def test_money_in_the_run_log_keeps_every_digit(tmp_path):
    """A string, like everywhere else the bot writes a number down. A float
    here would disagree with the audit trail in the last decimal place, and
    two screens over one bot that disagree are worse than one screen."""
    path = tmp_path / "runs.jsonl"
    run_id = runs.started(path, a_start())
    runs.finished(
        path,
        run_id,
        ticks=1,
        orders=0,
        exits=0,
        rejects=0,
        first_equity=Decimal("9966.404007"),
        last_equity=Decimal("9957.442304"),
        halted=False,
        reason="finished",
    )
    end = lines(path)[-1]
    assert end["first_equity"] == "9966.404007"
    assert end["last_equity"] == "9957.442304"


def test_a_run_that_was_killed_has_no_closing_line(tmp_path):
    """Not a hole in the writer: a process killed outright never reaches its
    second line, and the reader should be told that is what happened."""
    path = tmp_path / "runs.jsonl"
    runs.started(path, a_start())
    rows = lines(path)
    assert len(rows) == 1
    assert rows[0]["event"] == "start"


def test_two_runs_do_not_share_an_id(tmp_path):
    path = tmp_path / "runs.jsonl"
    first = runs.started(path, a_start())
    second = runs.started(path, a_start())
    assert first != second


def test_a_run_log_that_cannot_be_written_does_not_stop_the_run(tmp_path):
    """Bookkeeping may not break trading. The path here is a directory, so
    every write raises."""
    wrong = tmp_path / "a-directory"
    wrong.mkdir()
    run_id = runs.started(wrong, a_start())
    runs.finished(
        wrong,
        run_id,
        ticks=0,
        orders=0,
        exits=0,
        rejects=0,
        first_equity=None,
        last_equity=None,
        halted=False,
        reason="finished",
    )
    assert run_id, "it still answered with an id"


def test_no_run_log_at_all_is_allowed(tmp_path):
    run_id = runs.started(None, a_start())
    runs.finished(
        None,
        run_id,
        ticks=0,
        orders=0,
        exits=0,
        rejects=0,
        first_equity=None,
        last_equity=None,
        halted=False,
        reason="finished",
    )


CONFIG = """
exchange: grvt
grvt:
  env: testnet
trading:
  symbol: BTC_USDT_Perp
  loop_interval: 1
  warmup_candles: 0
  strategy: random
"""


def test_a_real_run_brackets_itself(tmp_path):
    """Through the CLI, the way a person starts a bot."""
    path = tmp_path / "runs.jsonl"
    config = tmp_path / "config.yaml"
    config.write_text(CONFIG, encoding="utf-8")
    code = main(
        [
            "--config",
            str(config),
            "--iterations",
            "2",
            "--log-file",
            str(tmp_path / "bot.log"),
            "--state-file",
            str(tmp_path / "state.json"),
            "--audit-file",
            str(tmp_path / "audit.jsonl"),
            "--runs-file",
            str(path),
        ]
    )
    assert code == 0
    rows = lines(path)
    assert [row["event"] for row in rows] == ["start", "end"]
    assert rows[0]["strategy"] == "random"
    assert rows[0]["mode"] == "paper", "no --live, so it was the simulator"
    assert rows[1]["ticks"] == 2, "two iterations, two ticks"
    assert rows[1]["reason"] == "finished"
    assert rows[1]["first_equity"], "the run knows what it started with"


# --- --close-at-end -----------------------------------------------------------


def run_with(tmp_path, *extra):
    config = tmp_path / "config.yaml"
    config.write_text(CONFIG, encoding="utf-8")
    return main(
        [
            "--config",
            str(config),
            "--log-file",
            str(tmp_path / "bot.log"),
            "--state-file",
            str(tmp_path / "state.json"),
            "--audit-file",
            str(tmp_path / "audit.jsonl"),
            "--runs-file",
            str(tmp_path / "runs.jsonl"),
            *extra,
        ]
    )


def always_buy(monkeypatch):
    """Make the coin-flip strategy buy on its first look.

    It rolls `random.Random().random()` and buys under 0.3; left alone, two
    ticks miss a signal one time in six, which is a test that fails at
    random rather than a test of anything.
    """
    monkeypatch.setattr(random.Random, "random", lambda self: 0.1)


def test_a_run_told_to_close_at_the_end_leaves_nothing_open(tmp_path, monkeypatch):
    """The strategy opens on the first tick; the run then stops and must not
    leave that position behind."""
    always_buy(monkeypatch)
    code = run_with(tmp_path, "--iterations", "2", "--strategy", "random", "--close-at-end")
    assert code == 0
    audit = lines(tmp_path / "audit.jsonl")
    assert any("order" in row for row in audit), "the run did open something"
    last = audit[-1]
    assert last.get("exit", {}).get("reason") == "run ended (--close-at-end)"
    end = lines(tmp_path / "runs.jsonl")[-1]
    assert end["reason"] == "finished and closed the position"
    assert end["exits"] >= 1


def test_without_the_flag_the_position_is_left_as_it_was(tmp_path, monkeypatch):
    """Unchanged behaviour unless asked: some lessons want to look at the
    open position afterwards."""
    always_buy(monkeypatch)
    run_with(tmp_path, "--iterations", "2", "--strategy", "random")
    audit = lines(tmp_path / "audit.jsonl")
    assert any("order" in row for row in audit), "there was something to leave open"
    assert not any(row.get("exit", {}).get("reason", "").startswith("run ended") for row in audit)
