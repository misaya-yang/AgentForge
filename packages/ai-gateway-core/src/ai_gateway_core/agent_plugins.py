"""Agent Plugins 1.0.0 directory loader.

The loader validates portable Skills plus stdio and Streamable HTTP MCP
declarations. It also parses inert agent definitions from named client
extensions without treating them as an Agent Plugins core component or as
runtime authority. The Assistant composition root decides whether an
operator-trusted component may be activated.
"""

from __future__ import annotations

import hashlib
import json
import re
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Final

import yaml

from .agent_plugin_mcp import (
    MCP_SCHEMA_V1,
    AgentPluginMCPServer,
    load_agent_plugin_mcp,
)
from .skills.models import SkillManifest, SkillSource

PLUGIN_SCHEMA_V1: Final = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"
_PLUGIN_FIELDS: Final = frozenset(
    {
        "$schema",
        "name",
        "version",
        "description",
        "author",
        "homepage",
        "repository",
        "license",
        "keywords",
        "extensions",
    }
)
_PLUGIN_NAME_RE: Final = re.compile(r"^(?!.*(?:--|\.\.))[a-z0-9](?:[a-z0-9.-]*[a-z0-9])?$")
_SKILL_NAME_RE: Final = re.compile(r"^(?!.*--)[a-z0-9](?:[a-z0-9-]*[a-z0-9])?$")
_MAX_PLUGIN_MANIFEST_BYTES: Final = 64 * 1024
_MAX_SKILL_BYTES: Final = 50_000
_MAX_AGENT_BYTES: Final = 50_000
_MAX_AGENTS_PER_PLUGIN: Final = 64
_AGENT_EXTENSION_NAMESPACES: Final = (
    "com.misaya.ai-gateway",
    "com.github.awesome-copilot",
)
_AGENT_ID_RE: Final = _SKILL_NAME_RE
_TOOL_NAME_RE: Final = re.compile(r"^[A-Za-z0-9](?:[A-Za-z0-9_.:-]{0,127})$")
_TOOL_CATEGORIES: Final = frozenset(
    {
        "retrieval",
        "generation",
        "analysis",
        "integration",
        "utility",
        "skill",
        "mcp",
    }
)
_AGENT_BASE_TYPES: Final = frozenset({"explore", "task", "plan"})


class AgentPluginLoadError(ValueError):
    """Stable, content-free plugin rejection."""

    def __init__(self, code: str) -> None:
        self.code = code
        super().__init__(code)


class _UniqueKeyLoader(yaml.SafeLoader):
    """Safe YAML loader that rejects duplicate mapping keys at every level."""


def _construct_unique_mapping(
    loader: _UniqueKeyLoader,
    node: yaml.nodes.MappingNode,
    deep: bool = False,
) -> dict[Any, Any]:
    loader.flatten_mapping(node)
    result: dict[Any, Any] = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        try:
            duplicate = key in result
        except TypeError as exc:
            raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_FRONTMATTER_INVALID") from exc
        if duplicate:
            raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_FRONTMATTER_DUPLICATE_KEY")
        result[key] = loader.construct_object(value_node, deep=deep)
    return result


_UniqueKeyLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG,
    _construct_unique_mapping,
)


@dataclass(frozen=True)
class AgentPluginDiagnostic:
    code: str
    component: str
    severity: str = "warning"

    def to_dict(self) -> dict[str, str]:
        return {
            "code": self.code,
            "component": self.component,
            "severity": self.severity,
        }


@dataclass(frozen=True)
class AgentPluginManifest:
    name: str
    version: str = ""
    description: str = ""
    author: dict[str, str] = field(default_factory=dict)
    homepage: str = ""
    repository: str = ""
    license: str = ""
    keywords: tuple[str, ...] = ()
    extensions: dict[str, dict[str, Any]] = field(default_factory=dict)


@dataclass(frozen=True)
class AgentPluginAgentLimits:
    """Initial/recommended budgets requested by a client-extension agent.

    These values are package data, not hard runtime authority. A host may
    extend them after verified progress, but must clamp every extension to its
    own limits and the parent run's remaining budget. The ``max_*`` names are
    retained as compatibility aliases for existing Agent markdown.
    """

    max_turns: int
    max_tool_calls: int
    max_tokens: int
    timeout_seconds: int
    idle_timeout_seconds: int

    @property
    def initial_max_turns(self) -> int:
        return self.max_turns

    @property
    def initial_max_tool_calls(self) -> int:
        return self.max_tool_calls

    @property
    def recommended_max_tokens(self) -> int:
        return self.max_tokens

    @property
    def initial_timeout_seconds(self) -> int:
        return self.timeout_seconds


@dataclass(frozen=True)
class AgentPluginAgent:
    """Inert agent definition loaded from a client extension.

    Agent Plugins 1.0.0 has only portable Skills and MCP components. This
    structure intentionally represents a host-owned extension: declared tools
    and categories are capability requests that never grant permission.
    """

    qualified_id: str
    plugin: str
    id: str
    name: str
    description: str
    instructions: str
    base_type: str
    allowed_tools: tuple[str, ...]
    allowed_tool_categories: tuple[str, ...]
    limits: AgentPluginAgentLimits
    source_namespace: str
    source_path: str
    sha256: str

    @property
    def max_turns(self) -> int:
        return self.limits.max_turns

    @property
    def max_tool_calls(self) -> int:
        return self.limits.max_tool_calls

    @property
    def max_tokens(self) -> int:
        return self.limits.max_tokens

    @property
    def timeout_seconds(self) -> int:
        return self.limits.timeout_seconds

    @property
    def content_sha256(self) -> str:
        return self.sha256


@dataclass(frozen=True)
class LoadedAgentPlugin:
    root: Path
    manifest: AgentPluginManifest
    skills: tuple[SkillManifest, ...]
    mcp_servers: tuple[AgentPluginMCPServer, ...]
    diagnostics: tuple[AgentPluginDiagnostic, ...]
    mcp_present: bool = False
    agents: tuple[AgentPluginAgent, ...] = ()


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise AgentPluginLoadError("AGENT_PLUGIN_JSON_DUPLICATE_KEY")
        result[key] = value
    return result


def _read_contained_file(
    path: Path,
    *,
    root: Path,
    limit: int,
    invalid_code: str,
) -> str:
    try:
        resolved = path.resolve(strict=True)
    except OSError as exc:
        raise AgentPluginLoadError(invalid_code) from exc
    if not resolved.is_relative_to(root) or not resolved.is_file():
        raise AgentPluginLoadError(invalid_code)
    try:
        if resolved.stat().st_size > limit:
            raise AgentPluginLoadError(f"{invalid_code}_TOO_LARGE")
        return resolved.read_text(encoding="utf-8")
    except OSError as exc:
        raise AgentPluginLoadError(invalid_code) from exc
    except UnicodeError as exc:
        raise AgentPluginLoadError(f"{invalid_code}_ENCODING") from exc


def _validate_manifest(raw: Any) -> tuple[AgentPluginManifest, list[AgentPluginDiagnostic]]:
    if not isinstance(raw, dict):
        raise AgentPluginLoadError("AGENT_PLUGIN_MANIFEST_NOT_OBJECT")

    diagnostics = [
        AgentPluginDiagnostic(
            code="AGENT_PLUGIN_MANIFEST_UNKNOWN_FIELD",
            component=str(field_name),
        )
        for field_name in sorted(set(raw) - _PLUGIN_FIELDS)
    ]
    schema = raw.get("$schema")
    if schema != PLUGIN_SCHEMA_V1:
        raise AgentPluginLoadError("AGENT_PLUGIN_SCHEMA_UNSUPPORTED")

    name = raw.get("name")
    if not isinstance(name, str) or not 1 <= len(name) <= 64 or not _PLUGIN_NAME_RE.fullmatch(name):
        raise AgentPluginLoadError("AGENT_PLUGIN_NAME_INVALID")

    string_fields: dict[str, str] = {}
    for field_name in (
        "version",
        "description",
        "homepage",
        "repository",
        "license",
    ):
        value = raw.get(field_name, "")
        if not isinstance(value, str):
            raise AgentPluginLoadError("AGENT_PLUGIN_MANIFEST_FIELD_INVALID")
        string_fields[field_name] = value

    author = raw.get("author", {})
    if not isinstance(author, dict) or set(author) - {"name", "email", "url"}:
        raise AgentPluginLoadError("AGENT_PLUGIN_AUTHOR_INVALID")
    if any(not isinstance(value, str) for value in author.values()):
        raise AgentPluginLoadError("AGENT_PLUGIN_AUTHOR_INVALID")

    keywords = raw.get("keywords", [])
    if not isinstance(keywords, list) or any(not isinstance(value, str) for value in keywords):
        raise AgentPluginLoadError("AGENT_PLUGIN_KEYWORDS_INVALID")

    extensions = raw.get("extensions", {})
    if not isinstance(extensions, dict):
        diagnostics.append(
            AgentPluginDiagnostic(
                code="AGENT_PLUGIN_EXTENSIONS_IGNORED",
                component="extensions",
            )
        )
        extensions = {}
    elif any(not isinstance(value, dict) for value in extensions.values()):
        raise AgentPluginLoadError("AGENT_PLUGIN_EXTENSIONS_INVALID")

    return (
        AgentPluginManifest(
            name=name,
            author={str(key): str(value) for key, value in author.items()},
            keywords=tuple(keywords),
            extensions={str(key): dict(value) for key, value in extensions.items()},
            **string_fields,
        ),
        diagnostics,
    )


def _parse_agent_skill(
    content: str,
    *,
    directory_name: str,
    plugin: AgentPluginManifest,
) -> SkillManifest:
    if "\x00" in content:
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_INVALID")
    normalized = content.replace("\r\n", "\n").replace("\r", "\n")
    match = re.match(r"^---\s*\n(.*?)\n---\s*\n?(.*)", normalized, re.DOTALL)
    if not match:
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_FRONTMATTER_REQUIRED")
    try:
        metadata = yaml.safe_load(match.group(1))
    except yaml.YAMLError as exc:
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_FRONTMATTER_INVALID") from exc
    if not isinstance(metadata, dict):
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_FRONTMATTER_INVALID")

    name = metadata.get("name")
    description = metadata.get("description")
    if (
        not isinstance(name, str)
        or not 1 <= len(name) <= 64
        or not _SKILL_NAME_RE.fullmatch(name)
        or name != directory_name
    ):
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_NAME_INVALID")
    if not isinstance(description, str) or not 1 <= len(description) <= 1024:
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_DESCRIPTION_INVALID")

    for optional_field in ("license", "compatibility", "allowed-tools"):
        value = metadata.get(optional_field)
        if value is not None and not isinstance(value, str):
            raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_FRONTMATTER_INVALID")
    compatibility = metadata.get("compatibility")
    if isinstance(compatibility, str) and not 1 <= len(compatibility) <= 500:
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_COMPATIBILITY_INVALID")
    custom_metadata = metadata.get("metadata", {})
    if not isinstance(custom_metadata, dict) or any(
        not isinstance(key, str) or not isinstance(value, str)
        for key, value in custom_metadata.items()
    ):
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_METADATA_INVALID")
    discovery_delegate_tool = str(
        custom_metadata.get("discovery_delegate_tool") or ""
    ).strip()
    if discovery_delegate_tool and not re.fullmatch(
        r"[A-Za-z][A-Za-z0-9_.:-]{0,127}",
        discovery_delegate_tool,
    ):
        raise AgentPluginLoadError("AGENT_PLUGIN_SKILL_METADATA_INVALID")

    # ``allowed-tools`` is descriptive only. A third-party package cannot
    # grant itself tools or permissions; the existing runtime policy remains
    # authoritative.
    return SkillManifest(
        name=name,
        title=name,
        description=description,
        entrypoint=f"md://agent-plugin/{plugin.name}/{name}",
        summary=description[:180],
        version=plugin.version or custom_metadata.get("version", "1.0.0"),
        tags=list(plugin.keywords),
        permissions=[],
        enabled=True,
        artifact_type="agent_plugin_instruction",
        instructions=match.group(2).strip(),
        config={
            "agent_plugin": plugin.name,
            "declared_allowed_tools": metadata.get("allowed-tools", ""),
            "discovery_delegate_tool": discovery_delegate_tool,
        },
        source=SkillSource.MARKETPLACE,
        max_context_tokens=2000,
        author=plugin.author.get("name", ""),
        generated=False,
        lifecycle_status="active",
    )


def _load_agent_frontmatter(raw: str) -> dict[str, Any]:
    try:
        metadata = yaml.load(raw, Loader=_UniqueKeyLoader)
    except AgentPluginLoadError:
        raise
    except yaml.YAMLError as exc:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_FRONTMATTER_INVALID") from exc
    if not isinstance(metadata, dict) or any(not isinstance(key, str) for key in metadata):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_FRONTMATTER_INVALID")
    return metadata


def _parse_string_list(
    value: Any,
    *,
    allowed_values: frozenset[str] | None = None,
    item_pattern: re.Pattern[str] | None = None,
    max_items: int,
) -> tuple[str, ...]:
    if not isinstance(value, list) or any(not isinstance(item, str) for item in value):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID")
    if len(value) > max_items:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID")
    if len(value) != len(set(value)):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID")
    if allowed_values is not None and any(item not in allowed_values for item in value):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID")
    if item_pattern is not None and any(not item_pattern.fullmatch(item) for item in value):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_CAPABILITIES_INVALID")
    return tuple(value)


def _parse_agent_limit(
    metadata: dict[str, Any],
    field_name: str,
    *,
    default: int,
    minimum: int,
    maximum: int,
) -> int:
    value = metadata.get(field_name, default)
    if isinstance(value, bool) or not isinstance(value, int) or not minimum <= value <= maximum:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_LIMIT_INVALID")
    return value


def _parse_agent_limit_alias(
    metadata: dict[str, Any],
    field_name: str,
    legacy_name: str,
    *,
    default: int,
    minimum: int,
    maximum: int,
) -> int:
    if (
        field_name in metadata
        and legacy_name in metadata
        and metadata[field_name] != metadata[legacy_name]
    ):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_LIMIT_INVALID")
    source_name = field_name if field_name in metadata else legacy_name
    return _parse_agent_limit(
        metadata,
        source_name,
        default=default,
        minimum=minimum,
        maximum=maximum,
    )


def _derived_agent_id(source_path: str) -> str:
    filename = Path(source_path).name
    stem = filename[: -len(".md")]
    if stem.endswith(".agent"):
        stem = stem[: -len(".agent")]
    candidate = re.sub(r"[^a-z0-9-]+", "-", stem.lower()).strip("-")
    if not candidate or not _AGENT_ID_RE.fullmatch(candidate):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_ID_INVALID")
    return candidate


def _parse_agent_definition(
    content: str,
    *,
    plugin: AgentPluginManifest,
    source_namespace: str,
    source_path: str,
) -> AgentPluginAgent:
    if "\x00" in content:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_INVALID")
    normalized = content.replace("\r\n", "\n").replace("\r", "\n")
    match = re.match(r"^---\s*\n(.*?)\n---\s*\n?(.*)", normalized, re.DOTALL)
    if not match:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_FRONTMATTER_REQUIRED")
    metadata = _load_agent_frontmatter(match.group(1))

    agent_id = metadata.get("id")
    if agent_id is None and source_namespace == "com.github.awesome-copilot":
        agent_id = _derived_agent_id(source_path)
    if (
        not isinstance(agent_id, str)
        or not 1 <= len(agent_id) <= 64
        or not _AGENT_ID_RE.fullmatch(agent_id)
    ):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_ID_INVALID")

    name = metadata.get("name")
    description = metadata.get("description")
    if not isinstance(name, str) or not 1 <= len(name) <= 128:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_NAME_INVALID")
    if not isinstance(description, str) or not 1 <= len(description) <= 2048:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_DESCRIPTION_INVALID")

    base_type = metadata.get("base_type")
    if base_type is None and source_namespace == "com.github.awesome-copilot":
        base_type = "explore"
    if not isinstance(base_type, str) or base_type not in _AGENT_BASE_TYPES:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_BASE_TYPE_INVALID")

    declared_tools = metadata.get("allowed_tools")
    if declared_tools is None and source_namespace == "com.github.awesome-copilot":
        declared_tools = metadata.get("tools", [])
    if declared_tools is None:
        declared_tools = []
    allowed_tools = _parse_string_list(
        declared_tools,
        item_pattern=_TOOL_NAME_RE,
        max_items=64,
    )
    allowed_categories = _parse_string_list(
        metadata.get("allowed_tool_categories", []),
        allowed_values=_TOOL_CATEGORIES,
        max_items=len(_TOOL_CATEGORIES),
    )

    limits = AgentPluginAgentLimits(
        max_turns=_parse_agent_limit_alias(
            metadata,
            "initial_max_turns",
            "max_turns",
            default=6,
            minimum=1,
            maximum=30,
        ),
        max_tool_calls=_parse_agent_limit_alias(
            metadata,
            "initial_max_tool_calls",
            "max_tool_calls",
            default=10,
            minimum=1,
            maximum=60,
        ),
        max_tokens=_parse_agent_limit_alias(
            metadata,
            "recommended_max_tokens",
            "max_tokens",
            default=4096,
            minimum=256,
            maximum=8192,
        ),
        timeout_seconds=_parse_agent_limit_alias(
            metadata,
            "initial_timeout_seconds",
            "timeout_seconds",
            default=120,
            minimum=5,
            maximum=600,
        ),
        idle_timeout_seconds=_parse_agent_limit(
            metadata,
            "idle_timeout_seconds",
            default=120,
            minimum=5,
            maximum=600,
        ),
    )
    instructions = match.group(2).strip()
    if not instructions:
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_INSTRUCTIONS_REQUIRED")

    digest = hashlib.sha256(content.encode("utf-8")).hexdigest()
    return AgentPluginAgent(
        qualified_id=f"{plugin.name}:{agent_id}",
        plugin=plugin.name,
        id=agent_id,
        name=name,
        description=description,
        instructions=instructions,
        base_type=base_type,
        allowed_tools=allowed_tools,
        allowed_tool_categories=allowed_categories,
        limits=limits,
        source_namespace=source_namespace,
        source_path=source_path,
        sha256=digest,
    )


def _read_agent_definition(
    raw_reference: Any,
    *,
    root: Path,
    plugin: AgentPluginManifest,
    source_namespace: str,
) -> AgentPluginAgent:
    if (
        not isinstance(raw_reference, str)
        or "\x00" in raw_reference
        or "\\" in raw_reference
        or not raw_reference.startswith("./agents/")
    ):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_PATH_INVALID")
    source_path = raw_reference[2:]
    relative = Path(source_path)
    if (
        relative.is_absolute()
        or len(relative.parts) != 2
        or relative.parts[0] != "agents"
        or relative.suffix != ".md"
        or relative.name in {".md", "..md"}
    ):
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_PATH_INVALID")

    agents_directory = root / "agents"
    candidate = root / relative
    if agents_directory.is_symlink() or candidate.is_symlink():
        raise AgentPluginLoadError("AGENT_PLUGIN_AGENT_PATH_INVALID")
    content = _read_contained_file(
        candidate,
        root=root,
        limit=_MAX_AGENT_BYTES,
        invalid_code="AGENT_PLUGIN_AGENT_INVALID",
    )
    return _parse_agent_definition(
        content,
        plugin=plugin,
        source_namespace=source_namespace,
        source_path=relative.as_posix(),
    )


def _load_agent_definitions(
    *,
    root: Path,
    plugin: AgentPluginManifest,
) -> tuple[list[AgentPluginAgent], list[AgentPluginDiagnostic]]:
    definitions: list[AgentPluginAgent] = []
    diagnostics: list[AgentPluginDiagnostic] = []
    for namespace in _AGENT_EXTENSION_NAMESPACES:
        extension = plugin.extensions.get(namespace)
        if extension is None or "agents" not in extension:
            continue
        references = extension.get("agents")
        component = f"extensions.{namespace}.agents"
        if not isinstance(references, list) or len(references) > _MAX_AGENTS_PER_PLUGIN:
            diagnostics.append(
                AgentPluginDiagnostic(
                    code="AGENT_PLUGIN_AGENTS_COMPONENT_INVALID",
                    component=component,
                )
            )
            continue
        for index, reference in enumerate(references):
            try:
                definitions.append(
                    _read_agent_definition(
                        reference,
                        root=root,
                        plugin=plugin,
                        source_namespace=namespace,
                    )
                )
            except AgentPluginLoadError as exc:
                diagnostics.append(
                    AgentPluginDiagnostic(
                        code=exc.code,
                        component=f"{component}[{index}]",
                    )
                )

    duplicate_ids = {
        agent_id
        for agent_id, count in Counter(item.id for item in definitions).items()
        if count > 1
    }
    for agent_id in sorted(duplicate_ids):
        diagnostics.append(
            AgentPluginDiagnostic(
                code="AGENT_PLUGIN_AGENT_ID_CONFLICT",
                component=agent_id,
            )
        )
    return (
        [item for item in definitions if item.id not in duplicate_ids],
        diagnostics,
    )


def load_agent_plugin(path: str | Path) -> LoadedAgentPlugin:
    """Load one portable Agent Plugin directory without executing package code."""

    try:
        root = Path(path).expanduser().resolve(strict=True)
    except OSError as exc:
        raise AgentPluginLoadError("AGENT_PLUGIN_ROOT_INVALID") from exc
    if not root.is_dir():
        raise AgentPluginLoadError("AGENT_PLUGIN_ROOT_INVALID")

    manifest_text = _read_contained_file(
        root / "plugin.json",
        root=root,
        limit=_MAX_PLUGIN_MANIFEST_BYTES,
        invalid_code="AGENT_PLUGIN_MANIFEST_INVALID",
    )
    try:
        raw_manifest = json.loads(manifest_text, object_pairs_hook=_reject_duplicate_keys)
    except AgentPluginLoadError:
        raise
    except json.JSONDecodeError as exc:
        raise AgentPluginLoadError("AGENT_PLUGIN_MANIFEST_JSON_INVALID") from exc
    manifest, diagnostics = _validate_manifest(raw_manifest)

    skills: list[SkillManifest] = []
    skills_path = root / "skills"
    if skills_path.exists():
        try:
            resolved_skills = skills_path.resolve(strict=True)
        except OSError:
            resolved_skills = skills_path
        if not resolved_skills.is_relative_to(root) or not resolved_skills.is_dir():
            diagnostics.append(
                AgentPluginDiagnostic(
                    code="AGENT_PLUGIN_SKILLS_COMPONENT_INVALID",
                    component="skills",
                )
            )
        else:
            for child in sorted(resolved_skills.iterdir(), key=lambda item: item.name):
                try:
                    resolved_child = child.resolve(strict=True)
                except OSError:
                    continue
                if not resolved_child.is_relative_to(root) or not resolved_child.is_dir():
                    diagnostics.append(
                        AgentPluginDiagnostic(
                            code="AGENT_PLUGIN_SKILL_PATH_INVALID",
                            component=child.name,
                        )
                    )
                    continue
                skill_path = resolved_child / "SKILL.md"
                if not skill_path.exists():
                    continue
                try:
                    skill_text = _read_contained_file(
                        skill_path,
                        root=root,
                        limit=_MAX_SKILL_BYTES,
                        invalid_code="AGENT_PLUGIN_SKILL_INVALID",
                    )
                    skills.append(
                        _parse_agent_skill(
                            skill_text,
                            directory_name=child.name,
                            plugin=manifest,
                        )
                    )
                except AgentPluginLoadError as exc:
                    diagnostics.append(AgentPluginDiagnostic(code=exc.code, component=child.name))

    agents, agent_diagnostics = _load_agent_definitions(root=root, plugin=manifest)
    diagnostics.extend(agent_diagnostics)

    mcp_result = load_agent_plugin_mcp(root)
    diagnostics.extend(
        AgentPluginDiagnostic(code=item.code, component=item.component)
        for item in mcp_result.diagnostics
    )

    return LoadedAgentPlugin(
        root=root,
        manifest=manifest,
        skills=tuple(skills),
        agents=tuple(agents),
        mcp_servers=mcp_result.servers,
        diagnostics=tuple(diagnostics),
        mcp_present=mcp_result.present,
    )


__all__ = [
    "AgentPluginAgent",
    "AgentPluginAgentLimits",
    "AgentPluginDiagnostic",
    "AgentPluginLoadError",
    "AgentPluginManifest",
    "AgentPluginMCPServer",
    "LoadedAgentPlugin",
    "MCP_SCHEMA_V1",
    "PLUGIN_SCHEMA_V1",
    "load_agent_plugin",
]
