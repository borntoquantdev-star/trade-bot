"""``tradebot --check``: it must pass what is sound and catch what is not.

The strategies that ship have to pass, or the check is a wall nobody gets
over. And each way a student's strategy goes wrong once it trades has to be
caught by name, or the check is a rubber stamp.
"""

from __future__ import annotations

import json
import math
from datetime import datetime, timedelta, timezone
from pathlib import Path

import pytest

from tradebot.cli.check import (
    FAIL,
    PASS,
    SKIP,
    Report,
    check_contract,
    find_tests,
    run_check,
)
from tradebot.cli.main import main
from tradebot.core.config import load_config
from tradebot.data.store import CandleStore
from tradebot.money import ONE, D
from tradebot.strategies import STRATEGIES, build_strategy
from tradebot.strategies.base import MarketView, Strategy
from tradebot.types import Candle, Signal

SYMBOL = "BTC_USDT_Perp"
ROOT = Path(__file__).resolve().parents[1]


def config():
    return CONFIG


CONFIG = None


def _load():
    return load_config(
        data={
            "exchange": "grvt",
            "trading": {"symbol": SYMBOL, "timeframe": "1m", "strategy": "ping_pong"},
        },
        env={},
    )


CONFIG = _load()


def contract(make) -> dict[str, str]:
    report = Report(strategy="x", class_name="X", source="x.py", fingerprint="0")
    check_contract(report, make, SYMBOL, D("60000"))
    return {finding.name: finding.status for finding in report.findings}


# --- what ships has to pass ---------------------------------------------------


@pytest.mark.parametrize("name", sorted(STRATEGIES))
def test_every_shipped_strategy_keeps_the_contract(name):
    found = contract(lambda: build_strategy(name, config()))
    assert FAIL not in found.values(), found
    assert found["watches its stop"] == PASS


# --- and each way of going wrong is caught by name --------------------------------


class EveryTenth(Strategy):
    """Buys on every tenth price with a 1% stop - unless a subclass breaks it."""

    def signal(self, market: MarketView, **changes) -> Signal:
        fields = dict(
            symbol=market.symbol,
            side="buy",
            ref_price=market.price,
            stop_price=market.price * (ONE - D("0.01")),
            reason="tenth price",
            created_at=market.now,
        )
        fields.update(changes)
        return Signal.build(**fields)

    def fires(self, market: MarketView) -> bool:
        return not market.in_position and len(market.recent_prices) % 10 == 0

    def get_signal(self, market):
        return self.signal(market) if self.fires(market) else None

    def get_exit(self, market):
        held = market.position
        if held and self.stop_hit(market, held.entry_price * (ONE - D("0.01"))):
            return "stop hit"
        return None


class StopOnTheWrongSide(EveryTenth):
    def get_signal(self, market):
        if self.fires(market):
            return self.signal(market, stop_price=market.price * D("1.01"))
        return None


class ReadsTheWallClock(EveryTenth):
    def get_signal(self, market):
        if self.fires(market):
            return self.signal(market, created_at=datetime.now(timezone.utc))
        return None


class ReadsAFile(EveryTenth):
    def get_signal(self, market):
        with open("prices.csv", encoding="utf-8") as handle:
            handle.read()
        return None


class NeverLeaves(EveryTenth):
    def get_exit(self, market):
        return None


class FallsOver(EveryTenth):
    def get_signal(self, market):
        if len(market.recent_prices) == 50:
            raise ZeroDivisionError("average of nothing")
        return super().get_signal(market)


@pytest.mark.parametrize(
    "cls, rule",
    [
        (EveryTenth, None),
        (StopOnTheWrongSide, "stop on the losing side"),
        (ReadsTheWallClock, "dated with market.now"),
        (ReadsAFile, "no network, no files"),
        (NeverLeaves, "watches its stop"),
        (FallsOver, "never crashes"),
    ],
)
def test_each_way_of_going_wrong_is_caught_by_name(cls, rule):
    found = contract(cls)
    failed = {name for name, status in found.items() if status == FAIL}
    assert failed == ({rule} if rule else set()), found


def test_a_strategy_that_never_fires_cannot_pass_the_rules_it_never_used():
    class Silent(EveryTenth):
        def get_signal(self, market):
            return None

    found = contract(Silent)
    assert found["fires on test prices"] == SKIP
    assert found["watches its stop"] == SKIP


# --- the strategy's own tests --------------------------------------------------


def test_the_strategys_own_tests_are_found_by_what_they_name():
    cls = type(build_strategy("ping_pong"))
    names = [path.name for path in find_tests(ROOT, cls)]
    assert "test_ping_pong.py" in names
    assert not any(name.startswith("test_check") for name in names), "ห้ามเรียกตัวเองซ้ำ"


# --- the whole thing --------------------------------------------------------------


def store_candles(folder: Path, bars: int = 400) -> CandleStore:
    store = CandleStore(folder)
    start = datetime.now(timezone.utc).replace(second=0, microsecond=0) - timedelta(
        minutes=bars + 5
    )
    candles = []
    for i in range(bars):
        price = D(f"{60000 * (1 + 0.004 * math.sin(i / 3)):.1f}")
        candles.append(
            Candle(SYMBOL, "1m", start + timedelta(minutes=i), price, price, price, price, D(1))
        )
    store.merge("grvt", candles)
    return store


def window():
    end = datetime.now(timezone.utc)
    return end - timedelta(days=2), end


def test_a_sound_strategy_with_real_candles_passes(tmp_path):
    store_candles(tmp_path)
    report = run_check(
        "ping_pong",
        lambda: build_strategy("ping_pong", config()),
        config(),
        data_dir=tmp_path,
        window=window(),
        fetch="tradebot --live --fetch-candles",
        run_tests=False,
    )
    assert report.passed, report.text()
    assert "PASS - fit for testnet" in report.text()
    assert len(report.fingerprint) == 16


def test_without_candles_the_verdict_waits_and_says_how_to_get_them(tmp_path):
    report = run_check(
        "ping_pong",
        lambda: build_strategy("ping_pong", config()),
        config(),
        data_dir=tmp_path,
        window=window(),
        fetch="tradebot --live --fetch-candles --days 1",
        run_tests=False,
    )
    assert not report.passed
    replay = [f for f in report.findings if f.layer == "replay"]
    assert replay[0].status == SKIP
    assert "tradebot --live --fetch-candles --days 1" in replay[0].detail
    assert "NOT YET" in report.text()


def test_the_command_prints_the_verdict_and_keeps_a_report(tmp_path, capsys):
    store_candles(tmp_path / "candles")
    path = tmp_path / "config.yaml"
    path.write_text(
        "exchange: grvt\ngrvt:\n  env: testnet\n"
        "trading:\n  symbol: BTC_USDT_Perp\n  timeframe: 1m\n  strategy: ping_pong\n",
        encoding="utf-8",
    )
    code = main(
        [
            "--config",
            str(path),
            "--check",
            "--days",
            "2",
            "--data-dir",
            str(tmp_path / "candles"),
            "--log-file",
            str(tmp_path / "logs" / "bot.log"),
        ]
    )
    out = capsys.readouterr().out
    assert code == 0, out
    assert "VERDICT  PASS" in out
    saved = json.loads((tmp_path / "logs" / "check-ping_pong.json").read_text(encoding="utf-8"))
    assert saved["passed"] is True
    assert saved["fingerprint"] in out
