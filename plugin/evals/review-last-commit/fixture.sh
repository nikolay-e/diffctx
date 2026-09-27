#!/bin/bash
set -euo pipefail
git init -q
git config user.email eval@example.com
git config user.name eval
mkdir -p src/shop src/reports tests
cat >src/shop/timing.py <<'PY'
import time


def elapsed(start: float) -> float:
    return time.monotonic() - start
PY
cat >src/shop/checkout.py <<'PY'
from shop.timing import elapsed as took

SLOW_CHECKOUT_SECONDS = 2.0


def is_slow(start: float) -> bool:
    return took(start) > SLOW_CHECKOUT_SECONDS
PY
cat >src/reports/latency.py <<'PY'
from shop import timing


def render(start: float) -> str:
    return f"{timing.elapsed(start):.1f}s"
PY
cat >src/shop/cart.py <<'PY'
def total(items: list[tuple[str, float]]) -> float:
    return sum(price for _, price in items)
PY
cat >tests/test_checkout.py <<'PY'
from shop.checkout import is_slow


def test_fast_checkout_is_not_slow(monkeypatch):
    monkeypatch.setattr("shop.checkout.took", lambda start: 1.5)
    assert not is_slow(0.0)
PY
git add -A
git commit -qm "initial shop"
cat >src/shop/timing.py <<'PY'
import time


def elapsed(start: float) -> float:
    return (time.monotonic() - start) * 1000
PY
git commit -qam "timing: report elapsed in milliseconds"
