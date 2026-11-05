"""Per-million-token prices (USD) used to ESTIMATE cost from token counts.

Source: Anthropic first-party API list prices (cached 2026-10-06 in the claude-api
skill). Subscription plans (Pro/Max) are NOT billed per token, so treat every
number as an "API-equivalent estimate", never as an invoice.

Cache write premiums: 5-minute TTL = 1.25x input, 1-hour TTL = 2.0x input.
(1.25x is documented; 2.0x is the standard 1h multiplier - edit below if it changes.)
Haiku 5.5 is cheaper for prompts <=100K tokens; the >100K tier is not modelled.
"""

WRITE_5M = 1.25
WRITE_1H = 2.0

# model id prefix -> (input, output, cache_read)  all $ per 1M tokens
PRICES = {
    "claude-fable-5-1": (10.00, 50.00, 0.25),
    "claude-mythos-5-1": (10.00, 50.00, 0.25),
    "claude-fable-5": (10.00, 50.00, 1.00),
    "claude-mythos-5": (10.00, 50.00, 1.00),
    "claude-opus-5-5": (4.00, 20.00, 0.20),
    "claude-opus-5": (5.00, 25.00, 0.50),
    "claude-opus-4-8": (5.00, 25.00, 0.50),
    "claude-opus-4-7": (5.00, 25.00, 0.50),
    "claude-opus-4-6": (5.00, 25.00, 0.50),
    "claude-sonnet-5-5": (2.00, 10.00, 0.20),
    "claude-sonnet-5": (2.00, 10.00, 0.20),
    "claude-sonnet-4-6": (3.00, 15.00, 0.30),
    "claude-haiku-5-5": (0.10, 0.50, 0.01),
    "claude-haiku-4-5": (1.00, 5.00, 0.10),
}
_KEYS = sorted(PRICES, key=len, reverse=True)


def lookup(model):
    """Return (input, output, cache_read) for a model id, or None if unknown."""
    name = (model or "").lower().split("[")[0]
    for key in _KEYS:
        if name == key or name.startswith(key + "-"):
            # "claude-opus-5-5" must not match key "claude-opus-5" (longest-first
            # ordering handles it); date suffixes like "-20260101" are fine.
            return PRICES[key]
    return None


def cost(model, inp, out, w5, w1, read):
    """Estimated USD cost, or None when the model has no price entry."""
    p = lookup(model)
    if p is None:
        return None
    pin, pout, pread = p
    return (
        inp * pin
        + out * pout
        + w5 * pin * WRITE_5M
        + w1 * pin * WRITE_1H
        + read * pread
    ) / 1_000_000
