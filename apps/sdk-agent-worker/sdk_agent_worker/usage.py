"""Bounded SDK usage observations. Absence is never a measured zero."""
import math

MAX_TOKENS = (1 << 63) - 1

def count(value):
    return value if type(value) is int and 0 <= value <= MAX_TOKENS else None

def observation(input_tokens, output_tokens, *, details=None, cost=None):
    incoming, outgoing = count(input_tokens), count(output_tokens)
    result = {"sdk_usage": {"state": "unavailable", "cost_state": "unavailable"}}
    audit = result["sdk_usage"]
    if incoming is not None:
        result["input_tokens"] = incoming
    if outgoing is not None:
        result["output_tokens"] = outgoing
    if incoming is not None and outgoing is not None and incoming + outgoing <= MAX_TOKENS:
        result.update(input_tokens=incoming, output_tokens=outgoing, total_tokens=incoming + outgoing)
        audit["state"] = "reported"
    if details is not None:
        audit["provider_usage"] = details
    if type(cost) in (int, float) and math.isfinite(cost) and cost >= 0:
        audit.update(cost_state="reported", provider_cost_usd=cost)
    return result

def openai_usage(responses):
    # Usage aggregates and request counters can contain initialized SDK zeroes.
    # Only pre-normalization provider evidence from every response qualifies.
    raw = [getattr(response, "raw_usage", None) for response in responses or []]
    if not raw:
        return observation(None, None)
    details = []
    compatible = True
    for value in raw:
        if not isinstance(value, dict):
            details.append({})
            compatible = False
            continue
        item = {key: value[key] for key in ("input_tokens", "output_tokens", "total_tokens") if count(value.get(key)) is not None}
        cache = value.get("input_tokens_details")
        cache = cache if isinstance(cache, dict) else {}
        item["input_tokens_details"] = {key: cache[key] for key in ("cached_tokens", "cache_write_tokens") if count(cache.get(key)) is not None}
        if any(count(cache.get(key)) != 0 for key in ("cached_tokens", "cache_write_tokens")):
            compatible = False
        details.append(item)
    incoming = sum(value["input_tokens"] for value in details) if all("input_tokens" in value for value in details) else None
    outgoing = sum(value["output_tokens"] for value in details) if all("output_tokens" in value for value in details) else None
    result = observation(incoming, outgoing, details=details)
    result["sdk_usage"]["platform_pricing_state"] = "two_rate_compatible" if compatible else "reconciliation_required"
    return result

def claude_usage(usage, cost):
    if not isinstance(usage, dict):
        return observation(None, None, cost=cost)
    keys = ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")
    details = {key: usage[key] for key in keys if key in usage and count(usage[key]) is not None}
    # Cache fields are optional in SDK results; missing components do not authorize
    # a total. Preserve partial evidence and leave accounting for reconciliation.
    values = [count(usage.get(key)) for key in keys]
    if any(value is None for value in values):
        incoming = sum(values[index] for index in (0, 2, 3)) if all(values[index] is not None for index in (0, 2, 3)) else None
        return observation(incoming, values[1], details=details, cost=cost)
    incoming, outgoing, read, creation = values
    result = observation(incoming + read + creation, outgoing, details=details, cost=cost)
    result["sdk_usage"]["platform_pricing_state"] = "two_rate_compatible" if read == 0 and creation == 0 else "reconciliation_required"
    if count(usage.get("total_tokens")) is not None:
        result["sdk_usage"]["provider_usage"]["total_tokens"] = usage["total_tokens"]
    return result
