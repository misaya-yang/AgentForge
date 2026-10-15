"""Request-local observations for blocking local LangChain model invocations."""
from __future__ import annotations

from threading import Lock
from typing import Any

from ai_gateway_core.exceptions import ValidationFailedError

_FIELDS = ("input_tokens", "output_tokens", "total_tokens")


def prepare_local_invoke_usage(config: dict[str, Any]) -> tuple[dict[str, Any], Any]:
    try:
        from langchain_core.callbacks.base import AsyncCallbackHandler, BaseCallbackManager
        from langchain_core.messages import AIMessage
        from langchain_core.outputs import ChatGeneration
    except ModuleNotFoundError as exc:
        if exc.name != "langchain_core":
            raise
        return config, None

    class UsageObserver(AsyncCallbackHandler):
        run_inline = True
        raise_error = True

        def __init__(self):
            self.runs: dict[str, dict[str, Any]] = {}
            self.lock = Lock()

        def start(self, run_id, *, chat):
            with self.lock:
                self.runs.setdefault(str(run_id), {"chat": chat, "ended": False, "failed": False})

        async def on_chat_model_start(self, serialized, messages, *, run_id, **kwargs):
            del serialized, messages, kwargs
            self.start(run_id, chat=True)

        async def on_llm_start(self, serialized, prompts, *, run_id, **kwargs):
            del serialized, prompts, kwargs
            self.start(run_id, chat=False)

        async def on_llm_end(self, response, *, run_id, **kwargs):
            del kwargs
            counts = None
            zero_cost = False
            generations = getattr(response, "generations", None)
            # Match the SDK callback's first-generation usage convention.
            generation = generations[0][0] if generations and generations[0] else None
            if isinstance(generation, ChatGeneration) and isinstance(generation.message, AIMessage):
                usage = generation.message.usage_metadata
                zero_cost = isinstance(usage, dict) and type(usage.get("total_cost")) in (int, float) and usage["total_cost"] == 0
                if isinstance(usage, dict) and all(
                    type(usage.get(key)) is int and usage[key] >= 0 for key in _FIELDS
                ) and usage["total_tokens"] == usage["input_tokens"] + usage["output_tokens"]:
                    counts = {key: usage[key] for key in _FIELDS}
            with self.lock:
                run = self.runs.setdefault(str(run_id), {"chat": False, "failed": False})
                run["ended"] = True
                run["counts"] = counts
                run["zero_cost"] = zero_cost

        async def on_llm_error(self, error, *, run_id, **kwargs):
            del error, kwargs
            with self.lock:
                run = self.runs.setdefault(str(run_id), {"chat": False, "ended": False})
                run["failed"] = True

        def result(self):
            with self.lock:
                runs = list(self.runs.values())
                reported = [run for run in runs if run.get("counts") is not None]
                complete = bool(runs) and all(
                    run.get("chat") and run.get("ended") and not run.get("failed")
                    and run.get("counts") is not None for run in runs
                )
                coverage = {
                    "coverage_scope": "observed_chat_model_first_generations",
                    "coverage": "complete" if complete else "partial" if reported else "unknown",
                    "observed_model_runs": len(runs),
                    "reported_model_runs": len(reported),
                    "completed_model_runs": sum(bool(run.get("ended")) for run in runs),
                    "sdk_zero_cost_runs": sum(bool(run.get("zero_cost")) for run in runs),
                    "accounting_eligible": False,
                    "accounting_reason": "model_binding_unverified",
                }
                # Names deliberately stay outside the recursive token-usage extractor.
                # Callback reports (including cache replays) do not prove consumption.
                if reported:
                    coverage.update({
                        "observed_input": sum(run["counts"]["input_tokens"] for run in reported),
                        "observed_output": sum(run["counts"]["output_tokens"] for run in reported),
                        "observed_total": sum(run["counts"]["total_tokens"] for run in reported),
                    })
                return coverage

    observer = UsageObserver()
    prepared = dict(config)
    callbacks = config.get("callbacks")
    if callbacks is None:
        prepared["callbacks"] = [observer]
    elif isinstance(callbacks, list):
        prepared["callbacks"] = [*callbacks, observer]
    elif isinstance(callbacks, BaseCallbackManager):
        copied = callbacks.copy()
        copied.add_handler(observer, inherit=True)
        prepared["callbacks"] = copied
    else:
        raise ValidationFailedError("langgraph_callbacks_invalid")
    return prepared, observer
