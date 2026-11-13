# The bot in a box.
#
#   docker build -t tradebot .
#   docker run --rm tradebot
#
# What it runs by default is the demo: paper money, real prices, no
# credentials of any kind. That is deliberate - an image whose default
# command could place a real order is an image somebody will run by
# accident.
#
# To run your own config instead, mount it over the one inside:
#
#   docker run --rm -v "$PWD/config:/app/config:ro" tradebot \
#     tradebot --dry-run --config config/config.yaml
#
# On Railway: point it at this repository, it reads this file, and the
# default command above starts the demo. Credentials, if you ever add any,
# go in Railway's variables (GRVT_API_KEY and friends) and are read by
# ${...} in the config - never baked into the image.

FROM python:3.11-slim

ENV PYTHONUNBUFFERED=1 \
    PIP_NO_CACHE_DIR=1 \
    PIP_DISABLE_PIP_VERSION_CHECK=1

WORKDIR /app

# The package first, so a change to a config or a doc does not reinstall it.
COPY pyproject.toml README.md ./
COPY tradebot ./tradebot
RUN pip install .

COPY config ./config

# The audit trail and the state file land here. On Railway this disk is
# wiped on every deploy, which is fine for a demo and wrong for a bot you
# care about - mount a volume at /app/logs for that.
RUN mkdir -p /app/logs

CMD ["tradebot", "--dry-run", "--paper-live", "--config", "config/demo.yaml"]
