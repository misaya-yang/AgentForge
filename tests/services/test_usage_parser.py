from src.services.metrics.usage_parser import (
    extract_assistant_id,
    extract_model,
    extract_provider,
    extract_token_usage,
)


def test_extract_token_usage_from_langgraph_updates_payload():
    payload = {
        "model": {
            "messages": [
                {
                    "id": "msg_1",
                    "usage_metadata": {
                        "input_tokens": 42,
                        "output_tokens": 18,
                        "total_tokens": 60,
                    },
                }
            ]
        }
    }

    usage = extract_token_usage(payload)
    assert usage is not None
    assert usage["input_tokens"] == 42
    assert usage["output_tokens"] == 18
    assert usage["total_tokens"] == 60


def test_extract_token_usage_from_openai_shape():
    payload = {
        "id": "chatcmpl_123",
        "model": "gpt-4o-mini",
        "usage": {
            "prompt_tokens": 15,
            "completion_tokens": 20,
            "total_tokens": 35,
        },
    }

    usage = extract_token_usage(payload)
    assert usage is not None
    assert usage["input_tokens"] == 15
    assert usage["output_tokens"] == 20
    assert usage["total_tokens"] == 35


def test_extract_string_fields_from_nested_payload():
    payload = {
        "metadata": {"provider": "openai"},
        "request": {"assistant_id": "asst_agent"},
        "response": {"model_name": "gpt-4.1-mini"},
    }

    assert extract_provider(payload) == "openai"
    assert extract_assistant_id(payload) == "asst_agent"
    assert extract_model(payload) == "gpt-4.1-mini"
