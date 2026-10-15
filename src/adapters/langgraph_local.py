"""Per-request projection of official local LangChain tool events.

No SDK import is required by remote adapters or legacy duck-typed iterators.
"""
from __future__ import annotations

from types import SimpleNamespace
from typing import Any

from ai_gateway_core.exceptions import ValidationFailedError


def _value(item: Any, name: str, default: Any = None) -> Any:
    return item.get(name, default) if isinstance(item, dict) else getattr(item, name, default)


def extract_local_result_text(result: Any) -> str | None:
    """Project a supported local assistant result; None preserves legacy handling."""
    try:
        from langchain_core.messages import AIMessage
    except ModuleNotFoundError as exc:
        if exc.name != "langchain_core":
            raise
        return None

    def is_assistant(message: Any) -> bool:
        return isinstance(message, AIMessage) or (
            isinstance(message, dict)
            and (message.get("role") or message.get("type")) in ("assistant", "ai", "AIMessage")
        )

    message = result if is_assistant(result) else None
    if message is None and isinstance(result, dict):
        messages = result.get("messages")
        if not messages and isinstance(result.get("values"), dict):
            messages = result["values"].get("messages")
        if not messages and isinstance(result.get("output"), dict):
            messages = result["output"].get("messages")
        if isinstance(messages, list):
            message = next((item for item in reversed(messages) if is_assistant(item)), None)
    if isinstance(message, AIMessage):
        return str(message.text)
    if isinstance(message, dict):
        content = message.get("content")
        if content is None:
            return ""
        if isinstance(content, (str, list)):
            return str(AIMessage(content=content).text)
    return None


class LocalToolStream:
    def __init__(self, message_class: Any) -> None:
        self.message_class = message_class
        self.bindings: dict[str, str] = {}
        self.started: dict[str, str] = {}
        self.identities: dict[str, str] = {}
        self.models: dict[str, Any] = {}
        self.emitted: dict[tuple[str, str], tuple[str, str]] = {}
        self.terminal: set[str] = set()

    def project(self, event: Any) -> list[Any]:
        if not isinstance(event, dict):
            return [event]
        kind = event.get("event")
        data = event.get("data") or {}
        run_id = str(event.get("run_id") or "")
        if kind == "on_chat_model_end":
            aggregate = self.models.pop(run_id, None)
            if aggregate is not None and any(
                not item.get("id") for item in aggregate.tool_call_chunks
            ):
                raise ValidationFailedError("langgraph_tool_chunk_identity_missing")
            return []
        if kind == "on_chat_model_stream":
            chunk = data.get("chunk")
            raw = _value(chunk, "tool_call_chunks")
            calls = _value(chunk, "tool_calls")
            content = _value(chunk, "content")
            if isinstance(content, list):
                # Use the SDK's text-only view of this delta, never its aggregate.
                content = self.message_class(content=content).text
                if not raw and not calls:
                    return [{**event, "data": {**data, "chunk": {"content": content}}}]
            if not raw and not calls:
                return [event]
            if not run_id:
                # Custom iterators without model-run identity retain their legacy projection.
                return [event]
            if raw:
                incoming = self.message_class(content="", tool_call_chunks=raw)
            else:
                incoming = self.message_class(content="", tool_calls=calls)
            previous = self.models.get(run_id)
            aggregate = previous + incoming if previous is not None else incoming
            self.models[run_id] = aggregate
            projected = []
            ids = [item.get("id") for item in aggregate.tool_call_chunks if item.get("id")]
            if len(ids) != len(set(ids)):
                raise ValidationFailedError("langgraph_tool_call_identity_conflict")
            for item in aggregate.tool_call_chunks:
                call_id = item.get("id")
                if not call_id or call_id in self.terminal:
                    continue
                name, args = item.get("name") or "", item.get("args") or ""
                key = (run_id, call_id)
                if self.emitted.get(key) == (name, args):
                    continue
                self.emitted[key] = (name, args)
                projected.append({**event, "data": {"chunk": {"tool_calls": [{
                    "id": call_id, "name": name, "args": args,
                }]}}})
            # Model text is still a delta; never emit aggregate.content here.
            if isinstance(content, str) and content:
                projected.append({**event, "data": {"chunk": {"content": content}}})
            return projected
        if kind not in {"on_tool_start", "on_tool_end", "on_tool_error"}:
            return [event]

        explicit = data.get("tool_call_id")
        if kind == "on_tool_end":
            output = data.get("output")
            explicit = _value(output, "tool_call_id")
            if (
                isinstance(output, dict) and output.get("type") == "tool"
                and isinstance(explicit, str) and "content" in output
            ):
                data = {**data, "output": SimpleNamespace(
                    content=output["content"], status=output.get("status"), tool_call_id=explicit,
                )}
        bound = self.bindings.get(run_id)
        if bound and explicit and bound != explicit:
            raise ValidationFailedError("langgraph_tool_call_identity_conflict")
        # Preserve the ID of a legacy start if its iterator never invoked callbacks.
        call_id = bound or self.started.get(run_id) or explicit or run_id
        self.identities[run_id] = call_id
        if kind == "on_tool_start":
            if run_id not in self.started:
                self.terminal.discard(call_id)
            self.started[run_id] = call_id
            tool_input = data.get("input")
            if (
                isinstance(tool_input, dict)
                and tool_input.get("type") == "tool_call"
                and tool_input.get("id") == call_id
                and "args" in tool_input
            ):
                data = {**data, "input": tool_input["args"]}
        else:
            self.terminal.add(call_id)
        # Lifecycle de-duplication still uses the original LangChain run ID.
        return [{**event, "data": data}]

    def finish(self) -> None:
        if any(
            not item.get("id")
            for aggregate in self.models.values()
            for item in aggregate.tool_call_chunks
        ):
            raise ValidationFailedError("langgraph_tool_chunk_identity_missing")

    def tool_identity(self, event: dict[str, Any]) -> str | None:
        if event.get("event") not in {"on_tool_start", "on_tool_end", "on_tool_error"}:
            return None
        return self.identities.get(str(event.get("run_id") or ""))


def prepare_local_tool_stream(config: dict[str, Any]) -> tuple[dict[str, Any], LocalToolStream | None]:
    try:
        from langchain_core.callbacks.base import AsyncCallbackHandler, BaseCallbackManager
        from langchain_core.messages import AIMessageChunk
    except ModuleNotFoundError as exc:
        if exc.name != "langchain_core":
            raise
        return config, None

    state = LocalToolStream(AIMessageChunk)

    class ToolIdentityObserver(AsyncCallbackHandler):
        # The official event streamer is non-inline. Record identity first.
        run_inline = True
        raise_error = True

        async def on_tool_start(self, serialized, input_str, *, run_id, **kwargs):
            del serialized, input_str
            call_id = kwargs.get("tool_call_id")
            if call_id is not None:
                if not isinstance(call_id, str) or not call_id:
                    raise ValidationFailedError("langgraph_tool_call_identity_missing")
                key = str(run_id)
                if key in state.bindings and state.bindings[key] != call_id:
                    raise ValidationFailedError("langgraph_tool_call_identity_conflict")
                state.bindings[key] = call_id

    observer = ToolIdentityObserver()
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
    return prepared, state
