from src.services.llm.provider_templates import (
    find_provider_template_for_config,
    get_provider_template,
    known_catalog_model_provider_ids,
    list_provider_templates,
)


def test_provider_template_catalog_contains_mainstream_providers() -> None:
    template_ids = {template.template_id for template in list_provider_templates()}

    assert {
        "dashscope-cn",
        "dashscope-intl",
        "google-ai-studio",
        "google-vertex",
        "custom-openai-compatible",
    }.issubset(template_ids)


def test_dashscope_china_template_owns_raw_provider_metadata() -> None:
    template = get_provider_template("dashscope-cn")

    assert template is not None
    assert template.default_provider_id == "dashscope-cn"
    assert template.api_type == "openai"
    assert template.default_base_url == "https://dashscope.aliyuncs.com/compatible-mode"
    assert {field.name for field in template.credential_fields} == {"api_key"}
    assert "qwen3.8-flash" in {model.model_id for model in template.default_models}
    assert "qwen3.7-plus" in {model.model_id for model in template.default_models}
    assert "qwen3.6-plus" in {model.model_id for model in template.default_models}
    assert "qwen3.7-max" in {model.model_id for model in template.default_models}


def test_find_template_for_existing_regional_dashscope_provider() -> None:
    template = find_provider_template_for_config(
        provider_id="dashscope",
        api_type="openai",
        base_url="https://dashscope-intl.aliyuncs.com/compatible-mode",
    )

    assert template is not None
    assert template.template_id == "dashscope-intl"


def test_gemini35_flash_is_scoped_to_google_templates_not_anthropic() -> None:
    provider_ids = known_catalog_model_provider_ids("Gemini-3.5-Flash")

    assert "google" in provider_ids
    assert "anthropic" not in provider_ids


def test_vertex_template_collects_official_auth_fields() -> None:
    template = get_provider_template("google-vertex")

    assert template is not None
    assert template.api_type == "google-vertex"
    assert template.description == "Google Vertex AI Gemini via official Google auth."
    assert {field.name for field in template.credential_fields} == {
        "project",
        "location",
        "api_key",
    }
