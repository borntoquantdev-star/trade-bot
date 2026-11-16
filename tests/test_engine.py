"""Engine tests: the loop, the state file, the audit trail, idempotency.

Everything runs against :class:`PaperBroker` with an injected clock, so the
tests are offline and do not sleep.
"""

import json
from datetime import datetime, timedelta, timezone
from decimal import Decimal

import pytest

from tradebot.brokers.base import BrokerError
from tradebot.brokers.paper import PaperBroker, fixed_price, price_series, random_walk
from tradebot.core.config import load_config
from tradebot.core.engine import Engine, EngineState, TickResult
from tradebot.money import D
from tradebot.strategies.base import MarketView, Strategy
from tradebot.types import RejectCode, Signal

SYMBOL = "BTC_USDT_Perp"
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


class ChartingStrategy(ScriptedStrategy):
    """Draws one line, or raises while drawing it."""

    def __init__(self, breaks: bool = False):
        super().__init__(side=None)
        self.breaks = breaks

    def chart(self, market: MarketView):
        if self.breaks:
            raise RuntimeError("the pen ran out")
        return {"average": market.price - 1}


def test_the_audit_row_carries_what_the_strategy_was_looking_at(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ChartingStrategy())
    engine.tick()
    row = json.loads((tmp_path / "audit.jsonl").read_text().splitlines()[0])
    assert row["view"] == {"average": "99999"}


def test_a_strategy_that_cannot_draw_still_trades_and_writes_its_row(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ChartingStrategy(breaks=True))
    result = engine.tick()
    assert result.error is None
    row = json.loads((tmp_path / "audit.jsonl").read_text().splitlines()[0])
    assert "view" not in row


def test_a_strategy_with_nothing_to_draw_adds_no_view(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ScriptedStrategy(side=None))
    engine.tick()
    row = json.loads((tmp_path / "audit.jsonl").read_text().splitlines()[0])
    assert "view" not in row


def test_a_new_day_resets_the_daily_budget(tmp_path):
    engine, broker, ticks = make_engine(tmp_path)
    engine.tick()
    assert engine.state.day == "2026-11-04"
    broker._equity = D("9500")
    ticks["now"] = START + timedelta(days=1)
    engine.tick()
    assert engine.state.day == "2026-11-05"
    assert engine.state.day_start_equity == D("9500")


def test_the_daily_budget_resets_at_midnight_thai_time_not_utc(tmp_path):
    engine, broker, ticks = make_engine(tmp_path)
    ticks["now"] = datetime(2026, 11, 4, 16, 59, tzinfo=timezone.utc)  # 23:59 in Bangkok
    engine.tick()
    assert engine.state.day == "2026-11-04"
    broker._equity = D("9500")
    ticks["now"] = datetime(2026, 11, 4, 17, 0, tzinfo=timezone.utc)  # 00:00 in Bangkok
    engine.tick()
    assert engine.state.day == "2026-11-05"
    assert engine.state.day_start_equity == D("9500")
    broker._equity = D("9400")
    ticks["now"] = datetime(2026, 11, 5, 0, 30, tzinfo=timezone.utc)  # 07:30, old reset time
    engine.tick()
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
    from tradebot.types import Side, SizedOrder

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


# --- random walk (ทำให้ dry run ขยับจริง) -----------------------------------


def test_a_seeded_walk_is_reproducible():
    a = random_walk(100_000, "0.5", seed=11)
    b = random_walk(100_000, "0.5", seed=11)
    assert [a("BTC") for _ in range(5)] == [b("BTC") for _ in range(5)]


def test_a_walk_moves_but_stays_positive():
    source = random_walk(100_000, "0.5", seed=3)
    prices = [source("BTC") for _ in range(200)]
    assert len(set(prices)) > 1, "ราคาต้องขยับ ไม่ใช่เส้นตรง"
    assert all(p > 0 for p in prices)


def test_a_walk_step_is_bounded():
    source = random_walk(100_000, "1", seed=5)
    previous = D(100_000)
    for _ in range(50):
        price = source("BTC")
        assert abs(price - previous) <= previous * D("0.01") + D("0.000001")
        previous = price


def test_a_walk_refuses_silly_inputs():
    with pytest.raises(ValueError):
        random_walk(0)
    with pytest.raises(ValueError):
        random_walk(100, "0")


# --- the way out -------------------------------------------------------------


class Exiter(ScriptedStrategy):
    """Opens like the scripted strategy, and leaves when told to."""

    def __init__(self, reason: str | None = "test asked", **kwargs):
        super().__init__(**kwargs)
        self.reason = reason
        self.exits_asked = 0

    def get_exit(self, market: MarketView) -> str | None:
        self.exits_asked += 1
        return self.reason


def test_the_loop_can_close_a_position_not_only_open_one(tmp_path):
    engine, broker, _ = make_engine(tmp_path, strategy=Exiter(reason=None))
    engine.tick()
    assert SYMBOL in broker.fetch_positions()

    engine.strategy.reason = "target reached"
    result = engine.tick()
    assert result.exit_reason == "target reached"
    assert result.exit_receipt is not None
    assert broker.fetch_positions() == {}, "ปิดแล้วต้องไม่มีไม้ค้าง"


def test_closing_is_not_asked_about_when_there_is_nothing_to_close(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=Exiter(side=None))
    engine.tick()
    assert engine.strategy.exits_asked == 0


def test_a_halted_bot_can_still_get_out(tmp_path):
    # A halt stops the bot taking new risk. If it also trapped the position
    # the halt was meant to protect, the halt would be the danger.
    engine, broker, _ = make_engine(tmp_path, strategy=Exiter(reason=None))
    engine.tick()
    engine.halt("daily loss limit")
    engine.strategy.reason = "flattening after the halt"

    result = engine.tick()
    assert result.exit_reason == "flattening after the halt"
    assert broker.fetch_positions() == {}


def test_the_exit_is_written_down_with_its_reason(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=Exiter(reason=None))
    engine.tick()
    engine.strategy.reason = "price came back"
    engine.tick()

    rows = [
        json.loads(line)
        for line in (tmp_path / "audit.jsonl").read_text(encoding="utf-8").splitlines()
        if line.strip()
    ]
    closing = rows[-1]
    assert closing["exit"]["reason"] == "price came back"
    assert closing["exit"]["simulated"] is True
    assert "order" not in closing, "ปิดไม้ไม่ใช่การเปิดไม้ใหม่"


def test_opening_waits_until_the_next_tick_after_a_close(tmp_path):
    # One action per iteration keeps the audit trail readable: a line that
    # both closed and opened would hide which price did which.
    engine, broker, ticks = make_engine(tmp_path, strategy=Exiter(reason=None))
    engine.tick()
    opened = broker.fetch_positions()[SYMBOL]

    engine.strategy.reason = "out"
    ticks["now"] += timedelta(seconds=1)
    engine.tick()
    assert broker.fetch_positions() == {}

    # The clock has to move: a signal identical to one already sent is
    # refused by the idempotency guard, which is the behaviour that stops a
    # restart from doubling a position.
    engine.strategy.reason = None
    ticks["now"] += timedelta(seconds=1)
    engine.tick()
    reopened = broker.fetch_positions()[SYMBOL]
    assert reopened.side == opened.side


def test_a_strategy_that_explodes_on_the_way_out_does_not_trap_the_position(tmp_path):
    class Exploding(Exiter):
        def get_exit(self, market):
            raise RuntimeError("exit exploded")

    engine, broker, _ = make_engine(tmp_path, strategy=Exploding())
    engine.tick()
    # The tick survives, the position is still there, and the bot keeps going.
    result = engine.tick()
    assert result.exit_reason is None
    assert SYMBOL in broker.fetch_positions()


# --- the trip to the exchange --------------------------------------------------


def _rows(tmp_path):
    return [json.loads(line) for line in (tmp_path / "audit.jsonl").read_text().splitlines()]


def test_the_trail_says_when_an_order_left_and_how_long_the_answer_took(tmp_path):
    engine, _, _ = make_engine(tmp_path)
    result = engine.tick()
    assert result.sent is not None and result.sent.ms >= 0
    order = _rows(tmp_path)[0]["order"]
    assert datetime.fromisoformat(order["sent_at"]).tzinfo is not None
    assert order["answered_ms"] >= 0


def test_an_order_the_broker_refused_still_says_how_long_the_refusal_took(tmp_path):
    engine, broker, _ = make_engine(tmp_path)

    def refuse(_order):
        raise BrokerError("margin")

    broker.place_order = refuse  # type: ignore[method-assign]
    engine.tick()
    row = _rows(tmp_path)[0]
    assert "receipt" not in row
    assert "answered_ms" in row["order"], "การโดนปฏิเสธก็เป็นการเดินทางไปตลาดหนึ่งรอบ"


def test_an_exit_is_timed_too(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=Exiter(reason=None))
    engine.tick()
    engine.strategy.reason = "target reached"
    engine.tick()
    exit_row = _rows(tmp_path)[1]["exit"]
    assert "sent_at" in exit_row and exit_row["answered_ms"] >= 0


def test_a_tick_that_sends_nothing_carries_no_timing(tmp_path):
    engine, _, _ = make_engine(tmp_path, strategy=ScriptedStrategy(side=None))
    engine.tick()
    row = _rows(tmp_path)[0]
    assert "order" not in row and "exit" not in row
