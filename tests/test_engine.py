"""Engine tests: the loop, the state file, the audit trail, idempotency.

Everything runs against :class:`PaperBroker` with an injected clock, so the
tests are offline and do not sleep.
"""

import json
from datetime import datetime, timedelta, timezone
from decimal import Decimal

import pytest

from hyperbot.brokers.base import BrokerError
from hyperbot.brokers.paper import PaperBroker, fixed_price, price_series
from hyperbot.core.config import load_config
from hyperbot.core.engine import Engine, EngineState, TickResult
from hyperbot.money import D
from hyperbot.strategies.base import MarketView, Strategy
from hyperbot.types import RejectCode, Signal

SYMBOL = "BTC"
START = datetime(2026, 11, 4, 12, 0, tzinfo=timezone.utc)


class ScriptedStrategy(Strategy):
    """Returns the same signal every tick (or nothing), for predictable tests."""

    def __init__(self, side: str | None = "buy", stop_pct: Decimal | None = None, raises=False):
        super().__init__()
        self.side = side
        self.stop_pct = D("0.01") if stop_pct is None else stop_pct
        self.raises = raises
        self.rejects: list = []
        self.placed: list = []

    def get_signal(self, market: MarketView) -> Signal | None:
        if self.raises:
            raise RuntimeError("strategy exploded")
        if self.side is None:
            return None
        offset = market.price * self.stop_pct
        stop = market.price - offset if self.side == "buy" else market.price + offset
        return Signal.build(
            symbol=market.symbol,
            side=self.side,
            ref_price=market.price,
            stop_price=stop,
            reason="scripted",
            created_at=market.now,
        )

    def on_reject(self, reject):
        self.rejects.append(reject)

    def on_order_placed(self, receipt):
        self.placed.append(receipt)


def make_engine(
    tmp_path,
    strategy=None,
    price=100_000,
    equity=10_000,
    clock_start=START,
    config_data=None,
    prices=None,
    history=None,
):
    # Warm-up is off by default here so a test's price series is not consumed
    # before the first tick; test_warm_up_* below switch it on deliberately.
    trading = {"symbol": SYMBOL, "loop_interval": 1, "warmup_candles": 0}
    data = {"trading": trading}
    for section, patch in (config_data or {}).items():
        data.setdefault(section, {}).update(patch)
    config = load_config(data=data, env={})
    source = price_series(prices) if prices else fixed_price(price)
    broker = PaperBroker(source, config.instrument, equity=equity, history=history)
    ticks = {"now": clock_start}

    def clock():
        return ticks["now"]

    engine = Engine(
        config=config,
        broker=broker,
        strategy=strategy or ScriptedStrategy(),
        state_path=tmp_path / "state.json",
        audit_path=tmp_path / "audit.jsonl",
        clock=clock,
    )
    engine.start()
    return engine, broker, ticks


# --- the happy path ----------------------------------------------------------


def test_a_signal_becomes_a_paper_fill(tmp_path):
    engine, broker, _ = make_engine(tmp_path)
    result = engine.tick()
    assert result.order is not None
    assert result.receipt is not None
    assert result.receipt.simulated is True
    assert broker.fetch_positions()[SYMBOL].qty == result.order.qty
    assert engine.strategy.placed


def test_doing_nothing_is_a_valid_tick(tmp_path):
    engine, broker, _ = make_engine(tmp_path, strategy=ScriptedStrategy(side=None))
    result = engine.tick()
    assert result.signal is None and result.decision is None and result.error is None
    assert broker.fetch_positions() == {}


def test_the_engine_never_resizes_what_the_gate_approved(tmp_path):
    engine, broker, _ = make_engine(tmp_path)
    result = engine.tick()
    assert broker.fills[-1].qty == result.order.qty
    assert broker.fills[-1].price == result.order.limit_price


# --- refusals ----------------------------------------------------------------


def test_a_refusal_sends_nothing(tmp_path):
    engine, broker, _ = make_engine(tmp_path)
    engine.halt("manual test")
    result = engine.tick()
    assert result.reject.code is RejectCode.HALTED
    assert result.receipt is None
    assert broker.fetch_positions() == {}
    assert engine.strategy.rejects


def test_the_second_position_on_the_same_symbol_is_refused(tmp_path):
    engine, _, _ = make_engine(tmp_path)
    engine.tick()
    second = engine.tick()
    assert second.reject.code is RejectCode.ALREADY_IN_POSITION


def test_hitting_the_daily_loss_limit_halts_and_persists(tmp_path):
    engine, broker, ticks = make_engine(
        tmp_path, prices=[100_000, 100_000], config_data={"risk": {"max_daily_loss_pct": 1.0}}
    )
    engine.tick()  # opens
    broker.close_position(SYMBOL)
    broker._equity = D("9000")  # a 10% hole, well past the 1% daily stop
    ticks["now"] = START + timedelta(minutes=1)
    result = engine.tick()
    assert result.reject.code in (RejectCode.DAILY_LOSS_LIMIT, RejectCode.MAX_DRAWDOWN)
    assert engine.state.halted is True
    # A fresh engine must still see the halt.
    reloaded = EngineState.load(tmp_path / "state.json")
    assert reloaded.halted is True


def test_resume_clears_a_halt(tmp_path):
    engine, _, _ = make_engine(tmp_path)
    engine.halt("because")
    engine.resume()
    assert EngineState.load(tmp_path / "state.json").halted is False
    assert engine.tick().receipt is not None


# --- idempotency -------------------------------------------------------------


def test_the_same_order_id_is_never_sent_twice(tmp_path):
    engine, broker, _ = make_engine(tmp_path)
    first = engine.tick()
    # Pretend the position vanished but the id was already recorded.
    broker._positions.clear()
    second = engine.tick()
    assert second.order.client_order_id == first.order.client_order_id
    assert second.receipt is None, "an already-sent id must not go out again"


def test_sent_ids_survive_a_restart(tmp_path):
    engine, _, _ = make_engine(tmp_path)
    sent = engine.tick().order.client_order_id
    assert sent in EngineState.load(tmp_path / "state.json").sent_order_ids


# --- failures ----------------------------------------------------------------


def test_a_broken_strategy_does_not_kill_the_bot(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ScriptedStrategy(raises=True))
    result = engine.tick()
    assert "strategy raised" in result.error
    assert result.receipt is None


def test_a_broker_read_failure_is_recorded(tmp_path):
    engine, broker, _ = make_engine(tmp_path)

    def boom(_symbol):
        raise BrokerError("exchange down")

    broker._price = boom
    result = engine.tick()
    assert "broker read failed" in result.error


def test_repeated_failures_halt_the_bot(tmp_path):
    engine, broker, _ = make_engine(tmp_path, strategy=ScriptedStrategy(raises=True))
    engine.max_consecutive_errors = 3
    code = engine.run(max_iterations=5, sleeper=lambda _s: None)
    assert engine.state.halted is True
    assert code == 1


def test_an_order_the_broker_rejects_is_not_treated_as_filled(tmp_path):
    engine, broker, _ = make_engine(tmp_path)

    def refuse(_order):
        raise BrokerError("margin")

    broker.place_order = refuse  # type: ignore[method-assign]
    result = engine.tick()
    assert result.order is not None
    assert result.receipt is None
    assert "rejected by broker" in result.error


# --- the loop ----------------------------------------------------------------


def test_run_stops_after_the_requested_iterations(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ScriptedStrategy(side=None))
    slept: list[float] = []
    assert engine.run(max_iterations=3, sleeper=slept.append) == 0
    # Sleeps happen between iterations, not after the last one.
    assert slept == [1, 1]


def test_run_survives_a_keyboard_interrupt(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ScriptedStrategy(side=None))

    def interrupt(_seconds):
        raise KeyboardInterrupt

    assert engine.run(max_iterations=5, sleeper=interrupt) == 0


# --- bookkeeping -------------------------------------------------------------


def test_the_audit_trail_records_decisions(tmp_path):
    engine, _, _ = make_engine(tmp_path)
    engine.tick()
    engine.tick()  # refused: already in position
    lines = [json.loads(line) for line in (tmp_path / "audit.jsonl").read_text().splitlines()]
    assert "order" in lines[0] and "receipt" in lines[0]
    assert lines[1]["reject"]["code"] == "already_in_position"
    assert all("at" in row and "price" in row for row in lines)


def test_a_new_utc_day_resets_the_daily_budget(tmp_path):
    engine, broker, ticks = make_engine(tmp_path)
    engine.tick()
    assert engine.state.day == "2026-11-04"
    broker._equity = D("9500")
    ticks["now"] = START + timedelta(days=1)
    engine.tick()
    assert engine.state.day == "2026-11-05"
    assert engine.state.day_start_equity == D("9500")


def test_peak_equity_only_goes_up(tmp_path):
    engine, broker, ticks = make_engine(tmp_path, strategy=ScriptedStrategy(side=None))
    engine.tick()
    broker._equity = D("12000")
    ticks["now"] = START + timedelta(minutes=1)
    engine.tick()
    broker._equity = D("11000")
    ticks["now"] = START + timedelta(minutes=2)
    engine.tick()
    assert engine.state.peak_equity == D("12000")


def test_state_survives_being_written_and_read(tmp_path):
    path = tmp_path / "state.json"
    state = EngineState(
        day="2026-11-04",
        day_start_equity=D("10000.5"),
        peak_equity=D("12345.67"),
        halted=True,
        halt_reason="test",
        sent_order_ids=["a", "b"],
    )
    state.save(path)
    loaded = EngineState.load(path)
    assert loaded == state
    assert isinstance(loaded.peak_equity, Decimal)


def test_a_corrupt_state_file_does_not_stop_the_bot(tmp_path):
    path = tmp_path / "state.json"
    path.write_text("{ not json", encoding="utf-8")
    assert EngineState.load(path) == EngineState()


def test_only_the_last_few_hundred_order_ids_are_kept(tmp_path):
    path = tmp_path / "state.json"
    EngineState(sent_order_ids=[str(i) for i in range(900)]).save(path)
    assert len(EngineState.load(path).sent_order_ids) == 500


# --- paper broker ------------------------------------------------------------


def test_the_paper_broker_tracks_pnl_on_close(tmp_path):
    engine, broker, _ = make_engine(tmp_path, prices=[100_000, 110_000], config_data=None)
    engine.tick()
    qty = broker.fetch_positions()[SYMBOL].qty
    entry = broker.fetch_positions()[SYMBOL].entry_price
    broker.close_position(SYMBOL)
    assert broker.realized_pnl == (D(110_000) - entry) * qty
    assert broker.fetch_positions() == {}


def test_the_paper_broker_refuses_a_second_position():
    config = load_config(env={})
    broker = PaperBroker(fixed_price(100), config.instrument)
    from hyperbot.types import Side, SizedOrder

    order = SizedOrder("id", config.trading.symbol, Side.BUY, D(1), D(100), D(99), None, D(1), "x")
    broker.place_order(order)
    with pytest.raises(BrokerError, match="already holds"):
        broker.place_order(order)


def test_the_paper_broker_only_knows_its_own_symbol():
    config = load_config(env={})
    broker = PaperBroker(fixed_price(100), config.instrument)
    with pytest.raises(BrokerError):
        broker.fetch_instrument("DOGE_USDT_Perp")


def test_a_price_series_walks_then_holds():
    source = price_series([1, 2, 3], loop=False)
    assert [source("x") for _ in range(5)] == [D(1), D(2), D(3), D(3), D(3)]


def test_a_looping_price_series_repeats():
    source = price_series([1, 2], loop=True)
    assert [source("x") for _ in range(4)] == [D(1), D(2), D(1), D(2)]


def test_an_empty_price_series_is_a_programming_error():
    with pytest.raises(ValueError):
        price_series([])


def test_closing_nothing_on_paper_is_not_an_error():
    config = load_config(env={})
    broker = PaperBroker(fixed_price(100), config.instrument)
    assert broker.close_position(config.trading.symbol) is None


def test_tick_results_expose_order_or_reject(tmp_path):
    engine, _, _ = make_engine(tmp_path)
    opened = engine.tick()
    refused = engine.tick()
    assert isinstance(opened, TickResult)
    assert opened.reject is None and opened.order is not None
    assert refused.order is None and refused.reject is not None


# --- warm-up -----------------------------------------------------------------


def test_the_window_is_primed_from_candles_before_the_first_decision(tmp_path):
    """A strategy needing history should not have to wait for iterations."""
    engine, _, _ = make_engine(
        tmp_path,
        strategy=ScriptedStrategy(side=None),
        config_data={"trading": {"warmup_candles": 30}},
        history=[100_000 + i for i in range(30)],
    )
    assert len(engine.prices) == 30
    assert engine.prices[-1] == D(100_029)


def test_the_window_holds_everything_the_warm_up_asks_for(tmp_path):
    engine, _, _ = make_engine(
        tmp_path,
        strategy=ScriptedStrategy(side=None),
        config_data={"trading": {"warmup_candles": 400}},
        history=list(range(1, 401)),
    )
    assert engine.prices.maxlen >= 400
    assert len(engine.prices) == 400


def test_warm_up_can_be_switched_off(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ScriptedStrategy(side=None))
    assert len(engine.prices) == 0


def test_a_broker_without_candles_starts_with_an_empty_window(tmp_path):
    engine, broker, _ = make_engine(
        tmp_path,
        strategy=ScriptedStrategy(side=None),
        config_data={"trading": {"warmup_candles": 10}},
    )
    # A broker that simply has no candle feed, as the protocol allows.
    broker.fetch_candles = None  # type: ignore[method-assign]
    engine.prices.clear()
    engine._warm_up()
    assert len(engine.prices) == 0


def test_a_failing_candle_feed_does_not_stop_the_bot(tmp_path):
    engine, broker, _ = make_engine(
        tmp_path,
        strategy=ScriptedStrategy(side=None),
        config_data={"trading": {"warmup_candles": 10}},
    )

    def boom(*_args):
        raise BrokerError("candle feed down")

    broker.fetch_candles = boom  # type: ignore[method-assign]
    engine.prices.clear()
    engine._warm_up()
    assert len(engine.prices) == 0
    assert engine.tick().error is None


def test_the_strategy_sees_the_warmed_up_window(tmp_path):
    seen = {}

    class Watcher(ScriptedStrategy):
        def get_signal(self, market):
            seen["count"] = len(market.recent_prices)
            return None

    engine, _, _ = make_engine(
        tmp_path,
        strategy=Watcher(side=None),
        config_data={"trading": {"warmup_candles": 20}},
        history=list(range(1, 21)),
    )
    engine.tick()
    # Twenty warm-up closes plus the mark read on this tick.
    assert seen["count"] == 21
