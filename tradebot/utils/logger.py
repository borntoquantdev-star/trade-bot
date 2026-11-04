"""Logging setup.

Calling :func:`setup_logger` twice with the same name is safe: handlers are
replaced rather than stacked, so log lines never appear three times after a
reload in a notebook or a test.
"""

from __future__ import annotations

import logging
import sys
from logging.handlers import RotatingFileHandler
from pathlib import Path

FORMAT = "%(asctime)s | %(levelname)-8s | %(name)s | %(message)s"
DATE_FORMAT = "%Y-%m-%d %H:%M:%S"


def setup_logger(
    name: str = "tradebot",
    log_file: str | Path | None = "logs/tradebot.log",
    level: int = logging.INFO,
    *,
    console: bool = True,
    max_bytes: int = 5_000_000,
    backups: int = 3,
) -> logging.Logger:
    """Return a configured logger writing to the console and a rotating file."""
    logger = logging.getLogger(name)
    logger.setLevel(level)
    logger.propagate = False

    for handler in list(logger.handlers):
        logger.removeHandler(handler)
        handler.close()

    formatter = logging.Formatter(FORMAT, DATE_FORMAT)

    if console:
        stream = logging.StreamHandler(sys.stdout)
        stream.setFormatter(formatter)
        stream.setLevel(level)
        logger.addHandler(stream)

    if log_file:
        path = Path(log_file)
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            file_handler = RotatingFileHandler(
                path, maxBytes=max_bytes, backupCount=backups, encoding="utf-8"
            )
            file_handler.setFormatter(formatter)
            file_handler.setLevel(level)
            logger.addHandler(file_handler)
        except OSError as exc:
            logger.warning("logging to %s is not possible (%s); console only", path, exc)

    return logger


__all__ = ["setup_logger"]
