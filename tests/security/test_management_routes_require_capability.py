"""Structural guard: every management route handler requires gateway capability."""

from __future__ import annotations

import ast
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[2]

MANAGEMENT_MODULES = (
    ROOT / "src/api/v1/skills.py",
    ROOT / "src/api/v1/mcp.py",
    ROOT / "src/api/v1/providers.py",
    ROOT / "src/api/v1/dashboard.py",
)

ROUTER_DECORATORS = frozenset(
    {
        "router.get",
        "router.post",
        "router.put",
        "router.patch",
        "router.delete",
        "router.websocket",
    }
)


def _handler_functions(module_path: Path) -> tuple[ast.Module, list[ast.AsyncFunctionDef]]:
    tree = ast.parse(module_path.read_text(encoding="utf-8"))
    handlers: list[ast.AsyncFunctionDef] = []
    for node in tree.body:
        if not isinstance(node, (ast.AsyncFunctionDef, ast.FunctionDef)):
            continue
        for deco in node.decorator_list:
            target = deco
            if isinstance(deco, ast.Call):
                target = deco.func
            if isinstance(target, ast.Attribute) and isinstance(target.value, ast.Name):
                key = f"{target.value.id}.{target.attr}"
                if key in ROUTER_DECORATORS:
                    if isinstance(node, ast.AsyncFunctionDef):
                        handlers.append(node)
                    break
    return tree, handlers


def _param_names(fn: ast.AsyncFunctionDef) -> set[str]:
    args = fn.args.args + fn.args.kwonlyargs
    return {arg.arg for arg in args}


def _calls_guard(node: ast.AST) -> bool:
    """True if ``node`` directly calls the gateway-capability guard."""
    if not isinstance(node, ast.Call):
        return False
    func = node.func
    if isinstance(func, ast.Name) and func.id == "require_gateway_capability":
        return True
    if isinstance(func, ast.Attribute) and func.attr == "require_gateway_capability":
        return True
    return False


def _uses_require_gateway_capability(fn: ast.AsyncFunctionDef) -> bool:
    return any(_calls_guard(node) for node in ast.walk(fn))


def _guarding_function_names(tree: ast.Module) -> set[str]:
    """Names of module-level helpers that themselves enforce the guard.

    Handlers frequently delegate authorization to a wrapper (e.g.
    ``_authorize_read`` / ``_authorize_write`` in mcp.py, ``_set_enabled`` in
    skills.py) instead of calling ``require_gateway_capability`` inline. Treat
    a call to any such wrapper as a valid guard. The set is derived from the
    module itself (a wrapper qualifies only if its own body calls the guard),
    so the check stays self-validating and cannot be satisfied by an arbitrary
    unguarded helper.
    """
    guarding: set[str] = set()
    for node in tree.body:
        if not isinstance(node, (ast.AsyncFunctionDef, ast.FunctionDef)):
            continue
        if any(_calls_guard(child) for child in ast.walk(node)):
            guarding.add(node.name)
    return guarding


def _uses_guard_wrapper(fn: ast.AsyncFunctionDef, guarding_names: set[str]) -> bool:
    for node in ast.walk(fn):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Name) and func.id in guarding_names:
            return True
    return False


def _uses_auth_dependency(fn: ast.AsyncFunctionDef) -> bool:
    for node in ast.walk(fn):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if not (isinstance(func, ast.Name) and func.id == "Depends"):
            continue
        if not node.args:
            continue
        arg0 = node.args[0]
        if isinstance(arg0, ast.Name) and arg0.id == "get_auth_context":
            return True
    return "auth" in _param_names(fn)


def _uses_user_capability_helper(fn: ast.AsyncFunctionDef) -> bool:
    for node in ast.walk(fn):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Name) and func.id == "_require_user_gateway_capability":
            return True
    return False


def _uses_websocket_auth(fn: ast.AsyncFunctionDef) -> bool:
    for node in ast.walk(fn):
        if not isinstance(node, ast.Call):
            continue
        func = node.func
        if isinstance(func, ast.Name) and func.id == "authenticate_websocket":
            return True
    return False


@pytest.mark.parametrize("module_path", MANAGEMENT_MODULES, ids=lambda p: p.name)
def test_management_route_handlers_require_capability(module_path: Path) -> None:
    tree, handlers = _handler_functions(module_path)
    guarding_names = _guarding_function_names(tree)
    missing: list[str] = []
    for fn in handlers:
        has_guard = (
            _uses_require_gateway_capability(fn)
            or _uses_user_capability_helper(fn)
            or _uses_guard_wrapper(fn, guarding_names)
        )
        has_auth = (
            _uses_auth_dependency(fn)
            or _uses_user_capability_helper(fn)
            or _uses_websocket_auth(fn)
        )
        if module_path.name == "providers.py" and _uses_user_capability_helper(fn):
            has_auth = True
        if not (has_guard and has_auth):
            missing.append(fn.name)
    assert not missing, (
        f"{module_path.name} handlers missing auth+capability guard: {missing}"
    )
