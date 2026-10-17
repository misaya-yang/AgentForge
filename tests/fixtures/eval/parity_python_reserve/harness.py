"""Trusted user-code sandbox harness. CPython owns parsing/execution, Rust owns scoring.
This is not a platform Python service and never runs on the eval host.
"""
import ast
import json
import sys

request = json.load(sys.stdin)
source = request["source"]
try:
    tree = ast.parse(source, filename="<candidate-reserve>")
except SyntaxError:
    print(json.dumps({"validation_error": "patched_source_syntax"}))
    raise SystemExit(0)
if len(tree.body) != 1 or not isinstance(tree.body[0], (ast.FunctionDef, ast.AsyncFunctionDef)):
    print(json.dumps({"validation_error": "patched_source_must_define_one_function"}))
    raise SystemExit(0)
function = tree.body[0]
if not isinstance(function, ast.FunctionDef) or function.name != "reserve":
    print(json.dumps({"validation_error": "patched_source_function_name"}))
    raise SystemExit(0)
error = None
for node in ast.walk(tree):
    if isinstance(node, (ast.Import, ast.ImportFrom, ast.Global, ast.Nonlocal, ast.With, ast.AsyncWith, ast.ClassDef, ast.Attribute, ast.Lambda)):
        error = "patched_source_unsafe_ast"
        break
    if isinstance(node, ast.Call) and not (isinstance(node.func, ast.Name) and node.func.id in ("ValueError", "TypeError", "isinstance")):
        error = "patched_source_unsafe_call"
        break
if error:
    print(json.dumps({"validation_error": error}))
    raise SystemExit(0)
if function.decorator_list:
    print(json.dumps({"validation_error": "patched_source_runtime:TypeError"}))
    raise SystemExit(0)

class ExecutionLimitError(Exception):
    pass

def execute(stock, requested):
    # A fresh function namespace per case preserves the old independent-call contract.
    namespace = {"__builtins__": {"ValueError": ValueError, "TypeError": TypeError, "isinstance": isinstance, "int": int, "bool": bool}}
    steps = 0
    def trace(frame, event, arg):
        nonlocal steps
        if frame.f_code.co_filename == "<candidate-reserve>":
            frame.f_trace_opcodes = True
            if event == "opcode":
                steps += 1
                if steps > 10000:
                    raise ExecutionLimitError()
        return trace
    try:
        sys.settrace(trace)
        exec(compile(tree, "<candidate-reserve>", "exec"), namespace)
        result = namespace["reserve"](stock, requested)
        # Serialization happens after tracing is removed; no candidate-defined objects
        # can be introduced because classes/attribute calls/imports are rejected.
        return {"value": result}
    except BaseException as exc:
        return {"error": type(exc).__name__}
    finally:
        sys.settrace(None)

results = [execute(case[0], case[1]) for case in request["required_cases"]]
negative = execute(5, -1)
try:
    encoded = json.dumps({"cases": results, "negative": negative}, allow_nan=False, separators=(",", ":"))
except (TypeError, ValueError, OverflowError):
    encoded = json.dumps({"validation_error": "patched_source_runtime:TypeError"})
if len(encoded.encode("utf-8")) > 65536:
    encoded = json.dumps({"validation_error": "patched_source_runtime:ExecutionLimitError"})
print(encoded)
