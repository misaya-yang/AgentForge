from __future__ import annotations

import copy
import hashlib
import json
import re
from pathlib import Path
from urllib.parse import urlsplit

from scripts import real_agent_scenario_runner as runner

ROOT = Path(__file__).parents[2]
FIXTURE = ROOT / "src/services/eval/fixtures/real_legal_title_vii"
SCENARIOS = FIXTURE / "scenarios.v1.json"
SOURCE_PACKET = FIXTURE / "source_packet.md"


def _golden_answer() -> dict[str, object]:
    return {
        "schema_version": "legal-title-vii-answer/v1",
        "legal_as_of": "2025-03-01",
        "jurisdiction_scope": "US_federal_Title_VII_only",
        "source_resolution": {
            "discrimination_controlling_source": "SRC-SCOTUS-MULDROW-2024",
            "stale_source_treatment": "vacated_not_controlling_for_discrimination_harm",
            "retaliation_controlling_source": "SRC-SCOTUS-BURLINGTON-2006",
            "discrimination_harm_standard": (
                "some_harm_to_terms_or_conditions_no_significance_test"
            ),
            "retaliation_harm_standard": "material_adversity_reasonable_worker_deterrence",
        },
        "issue_outcomes": {
            "coverage": {
                "conclusion": "covered",
                "source_ids": ["SRC-STATUTE-2000E", "SRC-SCOTUS-WALTERS-1997"],
                "fact_ids": ["F-PAYROLL-16", "F-PARTTIME-FRIDAY", "F-WEEKS-22"],
                "analysis": "The continuous employment relationships count on Fridays.",
            },
            "discrimination_rivera": {
                "conclusion": "plausible_claim_not_ultimate_liability",
                "source_ids": ["SRC-STATUTE-2000E-2", "SRC-SCOTUS-MULDROW-2024"],
                "fact_ids": ["F-RIVERA-TRANSFER", "F-MALE-REPLACEMENT", "F-MOTIVE-SEX"],
                "analysis": "The record supplies some disadvantage and evidence of sex motive.",
            },
            "retaliation_rivera": {
                "conclusion": "plausible_claim_material_adversity_required",
                "source_ids": [
                    "SRC-STATUTE-2000E-3",
                    "SRC-SCOTUS-BURLINGTON-2006",
                ],
                "fact_ids": [
                    "F-RIVERA-PROTECTED-COMPLAINT",
                    "F-MOTIVE-COMPLAINT",
                    "F-RIVERA-TRANSFER",
                ],
                "analysis": "Protected opposition and a potentially deterrent transfer are present.",
            },
            "timeliness_rivera": {
                "conclusion": "timely_under_300_day_fepa_window",
                "source_ids": ["SRC-STATUTE-2000E-5", "SRC-EEOC-FILING-CHARGE"],
                "fact_ids": [
                    "F-NOTICE-DATE",
                    "F-FILING-295",
                    "F-FEPA",
                    "F-NO-FEPA-TERMINATION",
                ],
                "analysis": "The joint filing occurred on day 295 in a qualifying jurisdiction.",
            },
            "retaliation_lee": {
                "conclusion": "no_title_vii_protected_activity_on_supplied_facts",
                "source_ids": ["SRC-STATUTE-2000E-3"],
                "fact_ids": ["F-LEE-SAFETY-ONLY"],
                "analysis": "A physical-safety-only complaint is not Title VII opposition here.",
            },
        },
        "trap_resolutions": {
            "T-STALE-CA8": {
                "verdict": "rejected",
                "source_ids": ["SRC-CA8-MULDROW-2022", "SRC-SCOTUS-MULDROW-2024"],
                "fact_ids": ["F-RIVERA-TRANSFER"],
                "reason": "The Supreme Court vacated the heightened discrimination rule.",
            },
            "T-STANDARDS-CONFLATED": {
                "verdict": "rejected",
                "source_ids": [
                    "SRC-SCOTUS-MULDROW-2024",
                    "SRC-SCOTUS-BURLINGTON-2006",
                ],
                "fact_ids": ["F-RIVERA-TRANSFER"],
                "reason": "The two provisions retain different harm standards.",
            },
            "T-WORKED-DAYS": {
                "verdict": "rejected",
                "source_ids": ["SRC-SCOTUS-WALTERS-1997"],
                "fact_ids": ["F-PARTTIME-FRIDAY"],
                "reason": "The employment relationship, not daily compensation, controls.",
            },
            "T-180-DAYS": {
                "verdict": "rejected",
                "source_ids": ["SRC-STATUTE-2000E-5", "SRC-EEOC-FILING-CHARGE"],
                "fact_ids": ["F-FILING-295", "F-FEPA", "F-NO-FEPA-TERMINATION"],
                "reason": "The qualifying agency facts invoke the longer filing window.",
            },
            "T-SAFETY-PROTECTED": {
                "verdict": "rejected",
                "source_ids": ["SRC-STATUTE-2000E-3"],
                "fact_ids": ["F-LEE-SAFETY-ONLY"],
                "reason": "Lee did not oppose a Title VII practice or participate in its process.",
            },
        },
        "sources_used": [
            "SRC-STATUTE-2000E",
            "SRC-STATUTE-2000E-2",
            "SRC-STATUTE-2000E-3",
            "SRC-STATUTE-2000E-5",
            "SRC-SCOTUS-WALTERS-1997",
            "SRC-CA8-MULDROW-2022",
            "SRC-SCOTUS-MULDROW-2024",
            "SRC-SCOTUS-BURLINGTON-2006",
            "SRC-EEOC-FILING-CHARGE",
        ],
        "advice": {
            "liability_position": "preliminary_plausibility_not_ultimate_liability",
            "proof_limitations": [
                "evidence_admissibility_unassessed",
                "credibility_unassessed",
                "discovery_incomplete",
            ],
            "scope_limitations": [
                "state_and_other_federal_law_out_of_scope",
                "remedies_not_quantified",
            ],
            "recommended_next_step": "prompt_qualified_counsel_or_eeoc_review",
        },
    }


def _observation(answer: dict[str, object]) -> dict[str, object]:
    return {
        "trial": 1,
        "observation_sha256": "a" * 64,
        "candidate_output": (
            "Preliminary fixed-date memo with issue-separated source and fact citations.\n"
            "<FINAL_JSON>\n" + json.dumps(answer, ensure_ascii=False) + "\n</FINAL_JSON>"
        ),
    }


def test_scenario_contract_is_real_provider_three_of_three_and_parallel() -> None:
    suite = runner.load_scenarios(SCENARIOS)
    scenario = suite["scenarios"][0]

    assert suite["suite_id"] == "assistant.real-legal-title-vii.v1"
    assert scenario["repetitions"] == 3
    assert scenario["require_parallel"] is True
    assert scenario["required_agent_ids"] == [
        "builtin:explore",
        "builtin:plan",
        "community-doublecheck:doublecheck",
    ]
    assert scenario["answer_locator"] == "final_json_tag"
    assert len(scenario["expected_assertions"]) >= 30
    assert '"expected_assertions"' not in scenario["prompt"]
    assert "Do not include an answer score" in scenario["prompt"]


def test_canonical_source_artifact_is_contained_and_runner_verified() -> None:
    suite = runner.load_scenarios(SCENARIOS)
    scenario = suite["scenarios"][0]
    receipts = runner.verify_source_artifacts(suite, scenario_directory=FIXTURE)
    packet = SOURCE_PACKET.read_text(encoding="utf-8")
    allowed_hosts = {
        "www.eeoc.gov",
        "www.govinfo.gov",
        "www.supremecourt.gov",
        "ecf.ca8.uscourts.gov",
    }

    assert scenario["source_artifacts"] == [
        {
            "artifact_id": "legal.title-vii-source-packet",
            "path": "source_packet.md",
            "sha256": hashlib.sha256(SOURCE_PACKET.read_bytes()).hexdigest(),
            "description": (
                "Pinned fact record and official primary-source extracts used by the task"
            ),
        }
    ]
    assert receipts == [
        {
            "scenario_id": "legal.title-vii.muldrow-transfer",
            "artifact_id": "legal.title-vii-source-packet",
            "relative_path": "source_packet.md",
            "content_sha256": hashlib.sha256(SOURCE_PACKET.read_bytes()).hexdigest(),
            "size_bytes": SOURCE_PACKET.stat().st_size,
        }
    ]
    assert "Legal cutoff: `2025-03-01`" in packet
    source_ids = set(re.findall(r"^### `(SRC-[A-Z0-9-]+)`$", packet, re.MULTILINE))
    assert len(source_ids) == 9
    urls = re.findall(r"^- Official URL: (https://\S+)$", packet, re.MULTILINE)
    assert len(urls) == 9
    for url in urls:
        parsed = urlsplit(url)
        assert parsed.scheme == "https"
        assert parsed.hostname in allowed_hosts


def test_hidden_golden_matrix_passes_only_exact_legal_resolution() -> None:
    scenario = runner.load_scenarios(SCENARIOS)["scenarios"][0]
    result = runner._validated_trial(scenario, _observation(_golden_answer()))

    assert result["answer_parse_error"] is None
    assert result["golden_passed"] is True
    assert all(item["passed"] for item in result["assertions"])

    stale_answer = copy.deepcopy(_golden_answer())
    stale_answer["source_resolution"]["stale_source_treatment"] = "still_controlling"
    stale_result = runner._validated_trial(scenario, _observation(stale_answer))
    assert stale_result["golden_passed"] is False
    assert any(
        item["assertion_id"] == "legal.stale-authority-rejected" and not item["passed"]
        for item in stale_result["assertions"]
    )


def test_wrong_hidden_counterexample_and_missing_final_json_fail_closed() -> None:
    scenario = runner.load_scenarios(SCENARIOS)["scenarios"][0]
    wrong_answer = copy.deepcopy(_golden_answer())
    wrong_answer["issue_outcomes"]["retaliation_lee"]["conclusion"] = (
        "protected_any_workplace_complaint"
    )
    wrong_result = runner._validated_trial(scenario, _observation(wrong_answer))
    assert wrong_result["golden_passed"] is False

    invalid = runner._validated_trial(
        scenario,
        {
            "trial": 1,
            "observation_sha256": "b" * 64,
            "candidate_output": json.dumps(_golden_answer()),
        },
    )
    assert invalid["golden_passed"] is False
    assert invalid["answer_parse_error"] == "candidate must contain exactly one FINAL_JSON block"


def test_judge_policy_is_reduce_only_with_material_error_caps() -> None:
    policy = (FIXTURE / "llm_judge_prompt.md").read_text(encoding="utf-8")

    assert "may only reduce" in policy
    assert "all three independent live provider trials" in policy
    assert "92.000" in policy
    assert "45 if the vacated 2022 Eighth Circuit" in policy
    assert "65 if Lee's safety-only complaint" in policy
    assert "85 for missing, sequential, fabricated" in policy
