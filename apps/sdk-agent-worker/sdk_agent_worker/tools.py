"""Small no-I/O tools; these grant no access to platform resources or credentials."""
import ast
import math
import operator
from datetime import datetime, timezone

SCHEMA = {"type": "object", "properties": {"expression": {"type": "string", "maxLength": 256}}, "required": ["expression"], "additionalProperties": False}
OPERATORS = {ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul, ast.Div: operator.truediv, ast.Mod: operator.mod}

def calculate(arguments):
    if not isinstance(arguments, dict) or set(arguments) != {"expression"}:
        raise ValueError("tool_arguments_invalid")
    expression = arguments["expression"]
    if not isinstance(expression, str) or len(expression) > 256:
        raise ValueError("tool_arguments_invalid")
    tree = ast.parse(expression, mode="eval")
    if sum(1 for _ in ast.walk(tree)) > 80:
        raise ValueError("tool_expression_too_complex")
    def visit(node):
        if isinstance(node, ast.Constant) and type(node.value) in (int, float):
            value = node.value
        elif isinstance(node, ast.BinOp) and type(node.op) in OPERATORS:
            value = OPERATORS[type(node.op)](visit(node.left), visit(node.right))
        elif isinstance(node, ast.UnaryOp) and type(node.op) in (ast.UAdd, ast.USub):
            value = visit(node.operand) * (-1 if isinstance(node.op, ast.USub) else 1)
        else:
            raise ValueError("tool_expression_unsupported")
        if not math.isfinite(value) or abs(value) > 1e15:
            raise ValueError("tool_result_out_of_range")
        return value
    return {"value": visit(tree.body)}

def utc_time(arguments):
    if arguments != {}:
        raise ValueError("tool_arguments_invalid")
    return {"utc": datetime.now(timezone.utc).isoformat()}

TOOLS = {"calculate": ("Evaluate bounded arithmetic without code execution", SCHEMA, calculate),
         "utc_time": ("Read current UTC time", {"type": "object", "properties": {}, "additionalProperties": False}, utc_time)}

async def governed_tool(authority, binding, name, arguments, call_id):
    """An authority is mandatory for platform capabilities; never fall back to local I/O.

    Authority implementations must bind SDK run identity, descriptor/schema revision,
    canonical argument hash and call ID to the existing Runtime approval/lease journal.
    No such host binding exists for standalone framework services yet.
    """
    if authority is None or binding is None:
        raise PermissionError("sdk_runtime_tool_authority_unavailable")
    return await authority.execute_bound(binding, name, arguments, call_id)
