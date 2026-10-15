"""Genuine SDK loops. Provider clients and credentials are private to one child process."""
import json
import inspect
from pathlib import Path
from .knowledge import tools_for, atomic_json
from .capabilities import GovernedTools, ClaudeCallIds
from .usage import openai_usage, claude_usage

async def openai_run(launch, state, emit):
    from agents import Agent, FunctionTool, Runner, RunConfig, SQLiteSession, OpenAIResponsesModel, ModelSettings
    from openai import AsyncOpenAI
    available = tools_for(launch)
    governed = GovernedTools(launch, emit)
    for name, descriptor in governed.tools.items():
        available[name] = (descriptor["description"], descriptor["input_schema"], None)
    tools = []
    for name, (description, schema, function) in available.items():
        async def invoke(context, raw, function=function, name=name):
            if name in governed.tools:
                return json.dumps(await governed.invoke(name, json.loads(raw), getattr(context, "tool_call_id", None)), allow_nan=False)
            value=function(json.loads(raw))
            if inspect.isawaitable(value): value=await value
            return json.dumps(value, allow_nan=False)
        tools.append(FunctionTool(name=name, description=description, params_json_schema=schema,
                                  on_invoke_tool=invoke, strict_json_schema=name not in governed.tools))
    async with AsyncOpenAI(api_key=launch.api_key, max_retries=0) as client:
        agent = Agent(name="Platform Agent", instructions=launch.instructions,
                      model=OpenAIResponsesModel(model=launch.model, openai_client=client),
                      model_settings=ModelSettings(preserve_raw_usage=True), tools=tools)
        session = SQLiteSession(launch.scope, str(state / "openai.sqlite"))
        result = Runner.run_streamed(agent, launch.input, session=session, max_turns=launch.max_turns,
                                     run_config=RunConfig(tracing_disabled=True))
        streamed = []
        try:
            async for event in result.stream_events():
                if event.type == "raw_response_event" and getattr(event.data, "type", None) == "response.output_text.delta":
                    streamed.append(event.data.delta)
                    await emit({"event": "delta", "text": event.data.delta})
            if not result.is_complete or result.interruptions or governed.poisoned:
                raise RuntimeError("sdk_run_incomplete")
            await emit({"event": "completed", "text": str(result.final_output), "streamed_text": "".join(streamed) or str(result.final_output), "usage": openai_usage(result.raw_responses)})
        finally:
            if not result.is_complete:
                result.cancel()

async def claude_run(launch, state, emit):
    from claude_agent_sdk import ClaudeAgentOptions, ClaudeSDKClient, ResultMessage, SdkMcpTool, create_sdk_mcp_server, PermissionResultDeny
    from claude_agent_sdk.types import StreamEvent
    available = tools_for(launch)
    governed = GovernedTools(launch, emit)
    for name, descriptor in governed.tools.items():
        available[name] = (descriptor["description"], descriptor["input_schema"], None)
    tools = []
    call_ids = ClaudeCallIds(governed)
    for name, (description, schema, function) in available.items():
        async def invoke(arguments, function=function, name=name):
            if name in governed.tools:
                try:
                    call_id = call_ids.take(name, arguments)
                except Exception:
                    governed.poisoned = True
                    raise
                value = await governed.invoke(name, arguments, call_id)
                return {"content": [{"type": "text", "text": json.dumps(value, allow_nan=False)}]}
            value=function(arguments)
            if inspect.isawaitable(value): value=await value
            return {"content": [{"type": "text", "text": json.dumps(value, allow_nan=False)}]}
        tools.append(SdkMcpTool(name=name, description=description, input_schema=schema, handler=invoke))
    async def deny(name, args, context):
        return PermissionResultDeny(message="Only platform-owned pure tools are available")
    saved = state / "claude-session.json"
    resume = json.loads(saved.read_text())["session_id"] if saved.exists() else None
    governed_options = {}
    if governed.tools:
        from claude_agent_sdk import HookMatcher
        governed_options["hooks"] = {"PreToolUse": [HookMatcher(matcher="mcp__platform__platform_capability_.*", hooks=[call_ids.before], timeout=10)]}
    options = ClaudeAgentOptions(model=launch.model, system_prompt=launch.instructions,
        max_turns=launch.max_turns, tools=[], setting_sources=[], strict_mcp_config=True,
        mcp_servers={"platform": create_sdk_mcp_server("platform", tools=tools)},
        allowed_tools=["mcp__platform__" + name for name in available],
        permission_mode="default", can_use_tool=deny, include_partial_messages=True,
        cwd=str(state), resume=resume,
        env={"ANTHROPIC_API_KEY": launch.api_key, "CLAUDE_CODE_MAX_RETRIES": "0", "DISABLE_TELEMETRY": "1", "DISABLE_ERROR_REPORTING": "1", "MCP_TOOL_TIMEOUT": "700000", "CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS": "0"},
        stderr=lambda message: None, **governed_options)
    complete = None
    streamed = []
    async with ClaudeSDKClient(options=options) as client:
        await client.query(launch.input)
        async for event in client.receive_response():
            if isinstance(event, StreamEvent):
                data = event.event
                if data.get("type") == "content_block_delta" and data.get("delta", {}).get("type") == "text_delta":
                    streamed.append(data["delta"]["text"])
                    await emit({"event": "delta", "text": data["delta"]["text"]})
            elif isinstance(event, ResultMessage):
                if governed.poisoned or event.is_error or event.subtype != "success" or getattr(event, "terminal_reason", None) not in (None, "completed"):
                    raise RuntimeError("sdk_provider_execution_failed")
                complete = event
        if complete is None:
            raise RuntimeError("sdk_run_incomplete")
    # Commit only a successful session. Atomic replacement, scoped server-owned path.
    atomic_json(saved,{"session_id":complete.session_id})
    await emit({"event": "completed", "text": complete.result or "", "streamed_text": "".join(streamed) or complete.result or "",
        "usage": claude_usage(complete.usage, complete.total_cost_usd)})
