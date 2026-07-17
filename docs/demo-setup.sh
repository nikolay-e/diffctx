# shellcheck shell=bash
demo_dir="$(mktemp -d)/demoshop"
mkdir -p "$demo_dir/shop"
cd "$demo_dir" || return
git init -q
git config user.email demo@example.com
git config user.name demo

cat >shop/models.py <<'EOF'
from dataclasses import dataclass


@dataclass
class Order:
    order_id: str
    amount_cents: int
    currency: str = "EUR"
EOF

cat >shop/payment.py <<'EOF'
from shop.models import Order


def charge(order: Order, gateway_url: str) -> dict:
    payload = {
        "id": order.order_id,
        "amount": order.amount_cents,
        "currency": order.currency,
    }
    return {"status": "ok", "sent_to": gateway_url, "payload": payload}
EOF

cat >shop/checkout.py <<'EOF'
from shop.models import Order
from shop.payment import charge

GATEWAY_URL = "https://pay.example.com/v1/charge"


def checkout(order: Order) -> dict:
    receipt = charge(order, GATEWAY_URL)
    return receipt
EOF

git add -A
git commit -qm "initial shop"

cat >shop/payment.py <<'EOF'
from shop.models import Order


def charge(order: Order, gateway_url: str, retries: int = 3) -> dict:
    payload = {
        "id": order.order_id,
        "amount": order.amount_cents,
        "currency": order.currency,
    }
    for attempt in range(retries):
        response = {"status": "ok", "sent_to": gateway_url, "payload": payload}
        if response["status"] == "ok":
            return response
    raise RuntimeError(f"charge failed after {retries} retries")
EOF

git add -A
git commit -qm "fix(payment): retry failed gateway charges"
