---
name: test
description: Automatically identify code changes, analyze affected functionality, generate or run relevant tests, and verify test coverage. Use when you need to test current modifications or ensure test coverage for changed code.
---

# Test Automation Skill

Analyze current code changes and ensure proper test coverage through generation or execution.

## Workflow

### 1. Identify Changed Code

- Run `git status` to see modified files
- Run `git diff` to understand what changed
- Categorize changes: new features, bug fixes, refactoring, or API changes
- Identify the functional areas affected

### 2. Locate Existing Tests

- Find related test files using patterns:
  - `tests/**/test_*.py` (Python pytest)
  - `**/*.test.ts`, `**/*.spec.ts` (TypeScript/Jest)
  - `**/*.test.tsx`, `**/*.spec.tsx` (React)
- Check if tests already exist for the modified code
- Analyze current test coverage for affected modules

### 3. Determine Test Strategy

Based on the change type:

| Change Type | Test Approach |
|-------------|---------------|
| New feature | Generate new unit tests + integration tests |
| Bug fix | Write regression test that reproduces the bug |
| Refactoring | Ensure existing tests still pass, add if missing |
| API change | Update API tests, add contract tests |

### 4. Generate Tests (if needed)

Follow these patterns:

**Python (pytest)**
```python
# AAA Pattern: Arrange, Act, Assert
async def test_function_name_expected_behavior():
    # Arrange - set up test data
    # Act - call the function
    # Assert - verify results
```

**TypeScript/Jest**
```typescript
describe('ComponentName', () => {
  it('should behavior when condition', () => {
    // Arrange, Act, Assert
  });
});
```

**Test Generation Guidelines:**
- One test per behavior/scenario
- Use descriptive test names: `test_<function>_<scenario>_<expected_result>`
- Mock external dependencies (database, APIs, file system)
- Use factory functions for test data: `create_test_user(overrides)`
- Include edge cases: null/empty inputs, boundary values, error conditions
- Test both happy path and error paths

### 5. Run Tests

Execute relevant tests based on project setup:

**Python:**
```bash
# Run specific test file
pytest tests/path/to/test_file.py -v

# Run tests matching a pattern
pytest -k "test_function_name" -v

# Run with coverage
pytest --cov=src --cov-report=term-missing
```

**JavaScript/TypeScript:**
```bash
# Run specific test
npm test -- --testPathPattern="filename"

# Run with coverage
npm test -- --coverage
```

### 6. Analyze Results

- If tests pass: Report success with coverage summary
- If tests fail:
  - Analyze failure reason
  - Determine if it's a test issue or code issue
  - Suggest fixes

## Reporting Format

```
## Test Report

### Changes Analyzed
- Files modified: [list]
- Functional areas: [list]

### Test Execution

#### Existing Tests
- Tests found: X
- Tests run: X
- Passed: X
- Failed: X
- Skipped: X

#### Generated Tests
- New tests created: X
- Location: [file paths]

### Coverage Analysis
| Module | Before | After | Delta |
|--------|--------|-------|-------|
| ... | ... | ... | ... |

### Issues Found
[List any failing tests with analysis]

### Recommendations
- [Missing test coverage areas]
- [Suggested additional tests]
```

## Test Quality Checklist

- [ ] Tests are independent (no shared state)
- [ ] Tests are deterministic (same result every run)
- [ ] No real network calls (use mocks/stubs)
- [ ] Fast execution (< 1s per unit test)
- [ ] Clear failure messages
- [ ] Edge cases covered
- [ ] Error handling tested

## Notes

- Run only affected test subset when possible for speed
- For TDD workflow: write failing test first, then implement
- If test framework is unclear, check `package.json` or `pyproject.toml`
- Prioritize unit tests over integration tests for speed
- Use fixtures for complex test data setup

$ARGUMENTS
