---
name: code-review
description: Systematic code review when the user asks to review code; analyze git diffs for bugs, regressions, risks, missing tests, and spec mismatches; report findings by severity with file references and suggested fixes.
---

# Code Review Skill

Use this skill to perform a consistent, high-signal code review on current git changes.

## Workflow

### 1. Identify the Change Scope (default: deep review)

- Run `git status` to see overall change status (never use -uall flag)
- Run `git diff` for unstaged changes
- Run `git diff --staged` for staged changes
- Focus on new or changed logic and behavior, but also scan nearby code for integration risks

### 2. Review for Correctness and Risk

- Look for bugs, edge cases, error handling gaps, and state or concurrency issues
- Check for behavior regressions against prior behavior or stated requirements
- Verify input validation, security concerns, and data integrity
- Security focus: SQL injection, XSS, command injection, path traversal, sensitive data exposure, CSRF

### 3. Review for Quality and Maintainability

- Spot unclear logic, unnecessary complexity, or missing comments for non-obvious behavior
- Confirm naming and structure are consistent with codebase patterns
- Check for code duplication and DRY violations

### 4. Review Tests

- Identify missing tests for new logic or bug fixes
- If tests exist, check coverage for edge cases and failure modes

### 5. Provide a Review Report

## Reporting Format

**Findings first, ordered by severity (critical, high, medium, low).**

Each finding should include:
- **Issue**: What is wrong
- **Location**: `path/to/file.py:line` (or nearest symbol if line unknown)
- **Impact**: Why it matters
- **Suggested fix**: How to resolve

Example format:

```
## Findings

### Critical
- **[Issue description]** in `src/api/auth.py:42`
  - Impact: Security vulnerability allowing unauthorized access
  - Fix: Add authentication check before processing request

### High
[findings...]

### Medium
[findings...]

### Low
[findings...]

## Open Questions / Assumptions
[List any unclear requirements or assumptions made during review]

## Change Summary
[Brief summary of what changed - keep this short, findings are priority]
```

If no issues found, state that explicitly and list any residual risks or test gaps.

## Notes

- Apply the review immediately after identifying changes
- If requirements are unclear, ask brief clarification before finalizing the review
- Do not implement changes unless user requests fixes; prioritize review quality
- Keep summaries brief; focus on findings first

$ARGUMENTS
