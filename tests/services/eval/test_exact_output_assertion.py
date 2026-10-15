"""A quoted success phrase must not satisfy a complete-response contract."""

import pytest

from src.services.eval.golden import evaluate_case, validate_cases
from tests.services.eval.test_golden_regression_gate import golden_case, replay_observation

EXPECTED = "The selected knowledge does not specify that."


@pytest.mark.parametrize(
    ("output", "passed"),
    [
        (EXPECTED, True),
        ("\n" + EXPECTED + "\n", True),
        ('Please ask a question; I would say "' + EXPECTED + '".', False),
        ("The mascot is invented. " + EXPECTED, False),
        (EXPECTED.lower(), False),
        ("", False),
    ],
)
def test_exact_output_checks_the_whole_response(output, passed):
    case = golden_case(assertions=[{"type": "output_equals", "value": EXPECTED}])
    case["expected_output"] = {"contains": EXPECTED}
    assert validate_cases([case])["valid"]
    result = evaluate_case(case, replay_observation(output_preview=output))
    assert result["passed"] is passed
    if not passed:
        assert any("output_equals" in failure for failure in result["failures"])


@pytest.mark.parametrize("value", [None, "", " ", 12])
def test_exact_output_requires_a_nonempty_string(value):
    case = golden_case(assertions=[{"type": "output_equals", "value": value}])
    assert not validate_cases([case])["valid"]
