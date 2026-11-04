"""The risk gate is the safety-critical part of this package, so it gets the
most tests.  Everything here is offline: no network, no credentials, no clock.
"""

from datetime import datetime, timedelta, timezone

import pytest

from tradebot.money import D
from tradebot.risk import client_order_id, explain, risk_gate
from tradebot.types import (
    InstrumentSpec,
    PortfolioState,
    Position,
    Reject,
    RejectCode,
    RiskLimits,
    Side,
    Signal,
    SizedOrder,
)

SYMBOL = "BTC_USDT_Perp"
NOW = datetime(2026, 11, 4, 12, 0, tzinfo=timezone.utc)
ENTRY = D("100000")
STOP = D("99000")


@pytest.fixture
def spec() -> InstrumentSpec:
    return InstrumentSpec.build(SYMBOL, tick_size="0.1", lot_step="0.001", min_notional="500")


@pytest.fixture
def limits() -> RiskLimits:
    return RiskLimits(
        risk_per_trade=D("0.01"),
        max_positions=1,
        max_daily_loss=D("0.03"),
        max_drawdown=D("0.10"),
        max_leverage=D(5),
        max_slippage=D("0.001"),
        fee_rate=D("0.0005"),
        allow_short=True,
    )


@pytest.fixture
def state() -> PortfolioState:
    return PortfolioState(equity=D("10000"), peak_equity=D("10000"))


def long_signal(**kwargs) -> Signal:
    params = dict(
        symbol=SYMBOL,
        side="buy",
        ref_price=ENTRY,
        stop_price=STOP,
        reason="test",
        created_at=NOW,
        valid_for_seconds=60,
    )
    params.update(kwargs)
    return Signal.build(**params)


def short_signal(**kwargs) -> Signal:
    params = dict(
        symbol=SYMBOL,
        side="sell",
        ref_price=ENTRY,
        stop_price=D("101000"),
        reason="test",
        created_at=NOW,
        valid_for_seconds=60,
    )
    params.update(kwargs)
    return Signal.build(**params)


def gate(signal, state, limits, spec, mark_price=None, now=NOW):
    return risk_gate(
        signal,
        state,
        limits,
        spec,
        D(mark_price) if mark_price is not None else signal.ref_price,
        now,
    )


# --- the happy path ----------------------------------------------------------


def test_approves_a_clean_long(state, limits, spec):
    order = gate(long_signal(), state, limits, spec)
    assert isinstance(order, SizedOrder)
    assert order.side is Side.BUY
    assert order.qty > 0
    # 1% of 10,000 = 100 risked over a 1,000 stop distance plus both fee legs.
    assert order.risk_amount <= state.equity * limits.risk_per_trade
    assert order.stop_price == STOP


def test_approves_a_clean_short(state, limits, spec):
    order = gate(short_signal(), state, limits, spec)
    assert isinstance(order, SizedOrder)
    assert order.side is Side.SELL


def test_quantity_is_always_a_multiple_of_the_lot_step(state, limits, spec):
    order = gate(long_signal(), state, limits, spec)
    assert order.qty % spec.lot_step == 0


def test_limit_price_caps_slippage_on_both_sides(state, limits, spec):
    buy = gate(long_signal(), state, limits, spec)
    sell = gate(short_signal(), state, limits, spec)
    # A long may not pay more than entry + max_slippage...
    assert buy.limit_price <= ENTRY * (1 + limits.max_slippage)
    assert buy.limit_price > ENTRY
    # ...and a short may not accept less than entry - max_slippage.
    assert sell.limit_price >= ENTRY * (1 - limits.max_slippage)
    assert sell.limit_price < ENTRY


def test_limit_prices_land_on_the_tick_grid(state, limits, spec):
    orders = (gate(long_signal(), state, limits, spec), gate(short_signal(), state, limits, spec))
    for order in orders:
        assert order.limit_price % spec.tick_size == 0


# --- the invariant that matters most ----------------------------------------


@pytest.mark.parametrize("stop_distance", ["50", "137.5", "500", "1000", "2500", "9999.9"])
def test_risk_amount_never_exceeds_the_budget(state, limits, spec, stop_distance):
    """Whatever the stop distance, rounding must never risk more than budgeted."""
    signal = long_signal(stop_price=ENTRY - D(stop_distance))
    decision = gate(signal, state, limits, spec)
    if isinstance(decision, Reject):
        pytest.skip(f"not sizeable at this distance: {decision.code.value}")
    budget = state.equity * limits.risk_per_trade
    assert decision.risk_amount <= budget, f"risked {decision.risk_amount} of {budget}"


def test_sizing_ignores_the_leverage_setting(state, limits, spec):
    """Leverage caps exposure; it must never change position size."""
    from dataclasses import replace

    small = gate(long_signal(), state, replace(limits, max_leverage=D(2)), spec)
    large = gate(long_signal(), state, replace(limits, max_leverage=D(20)), spec)
    assert small.qty == large.qty


# --- refusals, most serious first -------------------------------------------


def test_refuses_while_halted(state, limits, spec):
    halted = state.with_halt("manual kill switch")
    decision = gate(long_signal(), halted, limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.HALTED
    assert "manual kill switch" in decision.detail


def test_refuses_at_the_drawdown_kill_switch(limits, spec):
    from dataclasses import replace

    state = PortfolioState(equity=D("9000"), peak_equity=D("10000"))
    decision = gate(long_signal(), state, replace(limits, max_drawdown=D("0.10")), spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.MAX_DRAWDOWN


def test_refuses_at_the_daily_loss_ceiling(limits, spec):
    state = PortfolioState(equity=D("10000"), peak_equity=D("10000"), realized_pnl_today=D("-300"))
    decision = gate(long_signal(), state, limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.DAILY_LOSS_LIMIT


def test_refuses_an_expired_signal(state, limits, spec):
    decision = gate(long_signal(), state, limits, spec, now=NOW + timedelta(seconds=61))
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.EXPIRED


def test_accepts_a_signal_right_up_to_its_deadline(state, limits, spec):
    decision = gate(long_signal(), state, limits, spec, now=NOW + timedelta(seconds=60))
    assert isinstance(decision, SizedOrder)


def test_refuses_short_when_long_only(state, limits, spec):
    from dataclasses import replace

    decision = gate(short_signal(), state, replace(limits, allow_short=False), spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.SHORT_NOT_ALLOWED


@pytest.mark.parametrize(
    ("builder", "bad_stop"),
    [
        (long_signal, ENTRY),
        (long_signal, ENTRY + 1),
        (short_signal, ENTRY),
        (short_signal, ENTRY - 1),
    ],
)
def test_refuses_a_stop_on_the_wrong_side_of_entry(state, limits, spec, builder, bad_stop):
    decision = gate(builder(stop_price=bad_stop), state, limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.INVALID_STOP


def test_refuses_a_stop_tighter_than_one_tick(state, limits, spec):
    decision = gate(long_signal(stop_price=ENTRY - D("0.05")), state, limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.INVALID_STOP
    assert "noise" in decision.detail


def test_refuses_take_profit_on_the_losing_side(state, limits, spec):
    decision = gate(long_signal(take_profit=ENTRY - 500), state, limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.INVALID_STOP


def test_accepts_a_sane_take_profit(state, limits, spec):
    order = gate(long_signal(take_profit=ENTRY + 2000), state, limits, spec)
    assert isinstance(order, SizedOrder)
    assert order.take_profit == ENTRY + 2000


def test_refuses_when_price_has_run_away_from_the_signal(state, limits, spec):
    # 0.5% against a long, with 0.1% allowed.
    decision = gate(long_signal(), state, limits, spec, mark_price=ENTRY * D("1.005"))
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.SLIPPAGE_EXCEEDED


def test_allows_drift_in_our_favour(state, limits, spec):
    decision = gate(long_signal(), state, limits, spec, mark_price=ENTRY * D("0.99"))
    assert isinstance(decision, SizedOrder)


def test_refuses_stacking_onto_an_open_position(state, limits, spec):
    from dataclasses import replace

    held = {SYMBOL: Position(SYMBOL, Side.BUY, D("0.05"), ENTRY)}
    decision = gate(long_signal(), replace(state, positions=held), limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.ALREADY_IN_POSITION


def test_refuses_when_the_concurrent_slot_limit_is_full(state, limits, spec):
    from dataclasses import replace

    held = {"ETH_USDT_Perp": Position("ETH_USDT_Perp", Side.BUY, D("1"), D("3000"))}
    decision = gate(long_signal(), replace(state, positions=held), limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.TOO_MANY_POSITIONS


def test_refuses_when_the_budget_cannot_buy_one_lot_step(limits, spec):
    tiny = PortfolioState(equity=D("1"), peak_equity=D("1"))
    decision = gate(long_signal(), tiny, limits, spec)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.QTY_ROUNDS_TO_ZERO


def test_refuses_below_the_exchange_minimum(limits):
    big_min = InstrumentSpec.build(SYMBOL, "0.1", "0.001", min_notional="100000")
    state = PortfolioState(equity=D("10000"), peak_equity=D("10000"))
    decision = gate(long_signal(), state, limits, big_min)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.BELOW_MIN_NOTIONAL


def test_refuses_when_exposure_would_exceed_the_leverage_cap(state, limits, spec):
    from dataclasses import replace

    held = {"ETH_USDT_Perp": Position("ETH_USDT_Perp", Side.BUY, D("16"), D("3000"))}
    crowded = replace(state, positions=held)
    decision = gate(
        long_signal(),
        crowded,
        replace(limits, max_positions=5),
        spec,
    )
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.EXCEEDS_EXPOSURE


def test_refuses_a_spec_for_the_wrong_symbol(state, limits):
    other = InstrumentSpec.build("ETH_USDT_Perp", "0.01", "0.01", "10")
    decision = gate(long_signal(), state, limits, other)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.BAD_INSTRUMENT


def test_refuses_a_non_positive_mark_price(state, limits, spec):
    decision = gate(long_signal(), state, limits, spec, mark_price=0)
    assert isinstance(decision, Reject)
    assert decision.code is RejectCode.BAD_INSTRUMENT


def test_requires_an_aware_clock(state, limits, spec):
    with pytest.raises(ValueError):
        gate(long_signal(), state, limits, spec, now=datetime(2026, 11, 4, 12, 0))


# --- ordering of reasons -----------------------------------------------------


def test_reports_the_most_serious_reason_first(limits, spec):
    """Halted outranks everything, even an expired signal on a blown account."""
    broken = PortfolioState(
        equity=D("1"),
        peak_equity=D("10000"),
        realized_pnl_today=D("-9999"),
        halted=True,
        halt_reason="kill switch",
    )
    decision = gate(long_signal(), broken, limits, spec, now=NOW + timedelta(hours=5))
    assert decision.code is RejectCode.HALTED


def test_reject_codes_are_ordered_by_severity():
    assert RejectCode.HALTED.severity < RejectCode.MAX_DRAWDOWN.severity
    assert RejectCode.MAX_DRAWDOWN.severity < RejectCode.DAILY_LOSS_LIMIT.severity
    assert RejectCode.DAILY_LOSS_LIMIT.severity < RejectCode.BELOW_MIN_NOTIONAL.severity


# --- idempotency -------------------------------------------------------------


def test_the_same_signal_always_yields_the_same_order_id():
    signal = long_signal()
    assert client_order_id(signal) == client_order_id(signal)


def test_different_signals_get_different_order_ids():
    ids = {
        client_order_id(long_signal()),
        client_order_id(long_signal(stop_price=D("98000"))),
        client_order_id(short_signal()),
        client_order_id(long_signal(created_at=NOW + timedelta(seconds=1))),
    }
    assert len(ids) == 4


def test_the_order_carries_the_idempotency_key(state, limits, spec):
    signal = long_signal()
    order = gate(signal, state, limits, spec)
    assert order.client_order_id == client_order_id(signal)


# --- human-readable output ---------------------------------------------------


def test_explain_describes_both_outcomes(state, limits, spec):
    assert explain(gate(long_signal(), state, limits, spec)).startswith("APPROVED")
    assert explain(gate(long_signal(), state.with_halt("x"), limits, spec)).startswith("REFUSED")
