---
name: review-file
description: Deep code review for specified files or directories; analyze for bugs, security risks, code quality issues, and missing tests; report findings by severity with line references and suggested fixes.
---

# File Review Skill

Use this skill to perform a thorough code review on specific files or directories.

## Target

$ARGUMENTS

If no file is specified, ask the user which files to review.

## Workflow

### 1. Identify Review Scope

- Read the specified file(s) using Read tool
- If a directory is specified, use Glob to list relevant files
- Understand the file's role in the project by checking imports and dependencies

### 2. Review for Correctness and Risk

- Look for bugs, edge cases, error handling gaps
- Check for state or concurrency issues
- Identify potential behavior regressions
- Verify input validation and data integrity
- Security concerns:
  - SQL injection, XSS, command injection
  - Path traversal, sensitive data exposure
  - CSRF, insecure deserialization
  - Authentication/authorization gaps

### 3. Review for Quality and Maintainability

- Spot unclear logic or unnecessary complexity
- Check for missing comments on non-obvious behavior
- Confirm naming and structure match codebase patterns
- Identify code duplication (DRY violations)
- Check function/method length (recommend < 50 lines)

### 4. Language-Specific Checks

**Python (.py)**
- Type annotations completeness
- async/await correctness
- Context manager usage (with statements)
- Pydantic model validation
- FastAPI route conventions

**TypeScript/React (.ts/.tsx)**
- Type definitions (avoid `any`)
- React hooks rules
- Unnecessary re-renders
- Component responsibility (single purpose)
- Props type definitions

### 5. Review Tests

- Identify missing tests for the reviewed code
- Check test coverage for edge cases and failure modes

## Reporting Format

**Findings first, ordered by severity (critical, high, medium, low).**

Each finding should include:
- **Issue**: What is wrong
- **Location**: `path/to/file.py:line` (or nearest function/class if line unknown)
- **Impact**: Why it matters
- **Suggested fix**: How to resolve

Example format:

```
## File Review Report

### Target
- File: `src/services/auth.py`
- Type: Python
- Lines: 245

### Findings

#### Critical
- **Potential SQL injection** in `src/services/auth.py:87` (function `get_user`)
  - Impact: Allows attackers to execute arbitrary SQL
  - Fix: Use parameterized queries instead of string formatting

#### High
[findings...]

#### Medium
[findings...]

#### Low
[findings...]

### Strengths
[List positive aspects of the code]

### Open Questions / Assumptions
[List any unclear requirements or assumptions]
```

If no issues found, state that explicitly and note any residual risks or test gaps.

## Notes

- Do not implement changes unless user requests fixes
- Prioritize review quality over speed
- If requirements are unclear, ask brief clarification first
- Focus on actionable findings with clear remediation steps
