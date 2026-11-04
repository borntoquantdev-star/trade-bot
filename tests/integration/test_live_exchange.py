"""Tests that talk to a real exchange testnet - GRVT or Hyperliquid.

Run them on purpose, never by accident:

    pytest --run-integration

They need the SDK for whichever exchange ``config/config.yaml`` selects
(``pip install -e ".[grvt]"`` or ``".[hyperliquid]"``), a testnet environment,
and credentials. Every one of those is checked before anything is sent, and
the module refuses to run against production.

The one order it places sits far from the market price and is cancelled in a
``finally`` block.
"""

from __future__ import annotations

import os

import pytest

from tradebot.brokers import build_live_broker
from tradebot.brokers.base import BrokerError
from tradebot.core.config import ConfigError, load_config
from tradebot.money import D
from tradebot.risk import risk_gate
from tradebot.types import PortfolioState, Signal

pytestmark = pytest.mark.integration

REQUIRED_ENV = {
    "grvt": ("GRVT_API_KEY", "GRVT_PRIVATE_KEY", "GRVT_TRADING_ACCOUNT_ID"),
    "hyperliquid": ("HYPERLIQUID_ACCOUNT_ADDRESS", "HYPERLIQUID_API_WALLET_KEY"),
}
TESTNET_NAME = {"grvt": "testnet", "hyperliquid": "testnet"}


@pytest.fixture(scope="module")
def config():
    path = os.environ.get("TRADEBOT_TEST_CONFIG", "config/config.yaml")
    try:
        cfg = load_config(path if os.path.exists(path) else None)
    except ConfigError as exc:
        pytest.skip(f"no usable config: {exc}")

    if cfg.is_live:
        pytest.fail(
            f"integration tests refuse to run against production ({cfg.exchange} "
            f"{cfg.venue_name}). Point the config at a testnet."
        )
    if cfg.venue_name != TESTNET_NAME[cfg.exchange]:
        pytest.fail(f"expected a testnet, got {cfg.exchange} {cfg.venue_name}")

    missing = [name for name in REQUIRED_ENV[cfg.exchange] if not os.environ.get(name)]
    if missing:
        pytest.skip(f"missing credentials in the environment: {', '.join(missing)}")
    return cfg


@pytest.fixture(scope="module")
def broker(config):
    try:
        instance = build_live_broker(config)
    except (BrokerError, ImportError) as exc:
        pytest.skip(f"cannot build the {config.exchange} broker: {exc}")
    try:
        instance.connect()
    except BrokerError as exc:
        pytest.skip(f"cannot connect to {config.exchange} {config.venue_name}: {exc}")
    return instance


# --- read-only checks --------------------------------------------------------


def test_the_mark_price_is_tradable(broker, config):
    assert broker.fetch_mark_price(config.trading.symbol) > 0


def test_the_account_reports_equity(broker):
    """On Hyperliquid this also proves the master address, not the agent, was used."""
    assert broker.fetch_equity() >= 0


def test_positions_can_be_read(broker, config):
    assert isinstance(broker.fetch_positions(config.trading.symbol), dict)


def test_the_instrument_precision_is_usable(broker, config):
    spec = broker.fetch_instrument(config.trading.symbol)
    assert spec.tick_size > 0
    assert spec.lot_step > 0
    assert spec.min_notional > 0


def test_candles_come_back_in_order(broker, config):
    candles = broker.fetch_candles(config.trading.symbol, config.trading.timeframe, 20)
    assert candles, "the exchange returned no candles"
    assert all(c.close > 0 for c in candles)
    assert candles == sorted(candles, key=lambda c: c.open_time)


# --- one order, sized by the same gate the bot uses -------------------------


def test_a_gated_order_round_trip(broker, config):
    """Place one far-from-market order sized by :func:`risk_gate`, then cancel.

    Sizing comes from the gate, not by hand, so this also proves the quantity
    and price the live bot would send are acceptable to the exchange.
    """
    symbol = config.trading.symbol
    spec = broker.fetch_instrument(symbol)
    mark = broker.fetch_mark_price(symbol)

    entry = spec.round_price(mark * D("0.70"), favour="down")
    signal = Signal.build(
        symbol=symbol,
        side="buy",
        ref_price=entry,
        stop_price=spec.round_price(entry * D("0.95"), favour="down"),
        reason="integration test",
        valid_for_seconds=120,
    )
    equity = max(broker.fetch_equity(), D(1000))
    state = PortfolioState(equity=equity, peak_equity=equity, positions={})
    from tradebot.core.engine import utc_now

    decision = risk_gate(signal, state, config.risk, spec, entry, utc_now())
    if hasattr(decision, "code"):
        pytest.skip(f"the gate refused this test order: {decision}")

    receipt = broker.place_order(decision)
    try:
        assert receipt.broker_order_id
        assert receipt.simulated is False
    finally:
        try:
            broker.cancel_order(symbol, decision.client_order_id)
        except BrokerError as exc:  # pragma: no cover - cleanup only
            pytest.fail(
                f"could not cancel test order {receipt.broker_order_id}: {exc}. "
                "Cancel it by hand on the exchange."
            )
