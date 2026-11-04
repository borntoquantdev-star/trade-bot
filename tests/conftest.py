"""Test configuration.

The default ``pytest`` run is entirely offline: no network, no credentials, no
exchange SDK.  Tests that place real orders live in ``tests/integration`` and
need three separate things to be true before they run:

1. ``--run-integration`` on the command line,
2. ``grvt.env`` set to ``testnet`` (never ``prod``),
3. credentials present in the environment.

Version 1.0.0 shipped integration tests with none of those guards, so a bare
``pytest`` could place four real orders on whatever account the config pointed
at.
"""

from __future__ import annotations

import pytest


def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--run-integration",
        action="store_true",
        default=False,
        help="run tests that place real orders on the GRVT testnet",
    )


def pytest_collection_modifyitems(config: pytest.Config, items: list[pytest.Item]) -> None:
    if config.getoption("--run-integration"):
        return
    skip = pytest.mark.skip(reason="needs --run-integration (places real orders on the testnet)")
    for item in items:
        if "integration" in item.keywords:
            item.add_marker(skip)
