---
name: test-gen
description: Generate comprehensive tests for specified code or current changes. Creates unit tests, integration tests, and edge case coverage following project conventions.
---

# Test Generation Skill

Generate high-quality tests for specified code or current git changes.

## Target

$ARGUMENTS

If no target specified, analyze `git diff` to identify code needing tests.

## Workflow

### 1. Analyze Target Code

- Read the source file(s) to understand functionality
- Identify:
  - Public functions/methods to test
  - Input parameters and return types
  - Dependencies to mock
  - Error conditions and edge cases
  - Business logic branches

### 2. Detect Project Test Conventions

Check existing test files to match project patterns:

**Python Projects:**
- Look in `tests/`, `test/`, or alongside source files
- Check for pytest fixtures in `conftest.py`
- Identify common mocking patterns (unittest.mock, pytest-mock)
- Check for async test support (pytest-asyncio)

**TypeScript/React Projects:**
- Look in `__tests__/`, `*.test.ts`, `*.spec.ts`
- Check Jest config in `jest.config.js` or `package.json`
- Identify testing library (React Testing Library, Enzyme)
- Check for MSW or other API mocking

### 3. Generate Test Structure

**For each function/method, create tests for:**

| Category | What to Test |
|----------|--------------|
| Happy path | Normal inputs → expected outputs |
| Edge cases | Empty, null, boundary values |
| Error handling | Invalid inputs, exceptions |
| Integration | Interaction with dependencies |

### 4. Generate Test Code

**Python Example:**
```python
import pytest
from unittest.mock import AsyncMock, patch, MagicMock

# Fixtures
@pytest.fixture
def mock_dependency():
    """Factory fixture with sensible defaults."""
    return MagicMock(spec=DependencyClass)

# Happy path
@pytest.mark.asyncio
async def test_function_name_returns_expected_result():
    """Test that function returns X when given Y."""
    # Arrange
    input_data = create_test_input()

    # Act
    result = await function_under_test(input_data)

    # Assert
    assert result.status == "success"
    assert result.data == expected_data

# Edge case
@pytest.mark.asyncio
async def test_function_name_handles_empty_input():
    """Test graceful handling of empty input."""
    result = await function_under_test([])
    assert result == []

# Error case
@pytest.mark.asyncio
async def test_function_name_raises_on_invalid_input():
    """Test that ValueError is raised for invalid input."""
    with pytest.raises(ValueError, match="Invalid input"):
        await function_under_test(None)

# Integration with mock
@pytest.mark.asyncio
async def test_function_name_calls_external_service(mock_dependency):
    """Test interaction with external service."""
    mock_dependency.fetch.return_value = {"data": "value"}

    result = await function_under_test(mock_dependency)

    mock_dependency.fetch.assert_called_once_with(expected_args)
```

**TypeScript/React Example:**
```typescript
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { vi, describe, it, expect, beforeEach } from 'vitest';

describe('ComponentName', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  // Happy path
  it('should render correctly with valid props', () => {
    render(<Component prop="value" />);
    expect(screen.getByText('Expected Text')).toBeInTheDocument();
  });

  // User interaction
  it('should call handler when button clicked', async () => {
    const mockHandler = vi.fn();
    render(<Component onClick={mockHandler} />);

    fireEvent.click(screen.getByRole('button'));

    expect(mockHandler).toHaveBeenCalledTimes(1);
  });

  // Async operation
  it('should display data after loading', async () => {
    render(<Component />);

    await waitFor(() => {
      expect(screen.getByText('Loaded Data')).toBeInTheDocument();
    });
  });

  // Error state
  it('should display error message on failure', async () => {
    vi.mocked(fetchData).mockRejectedValue(new Error('Failed'));

    render(<Component />);

    await waitFor(() => {
      expect(screen.getByText('Error: Failed')).toBeInTheDocument();
    });
  });
});
```

### 5. Test Naming Convention

Follow this pattern for clarity:

```
test_<unit>_<scenario>_<expected_result>

Examples:
- test_create_user_with_valid_data_returns_user_id
- test_create_user_with_duplicate_email_raises_conflict_error
- test_login_with_expired_token_returns_401
```

### 6. Output Location

Place generated tests following project convention:
- `tests/unit/test_<module>.py` for Python
- `src/<module>/__tests__/<component>.test.tsx` for React
- Alongside source if that's the project pattern

## Test Generation Checklist

- [ ] All public functions/methods covered
- [ ] Input validation tested
- [ ] Error paths tested
- [ ] Async operations properly awaited
- [ ] Mocks verify call arguments, not just call count
- [ ] Test data uses factories, not hardcoded values
- [ ] No flaky tests (no sleep, no random without seed)
- [ ] Tests are independent and can run in any order

## Reporting Format

```
## Generated Tests

### Target Code
- File: `src/services/user_service.py`
- Functions: create_user, update_user, delete_user

### Tests Created
- File: `tests/unit/test_user_service.py`
- Total tests: 12

### Coverage

| Function | Tests | Edge Cases | Error Cases |
|----------|-------|------------|-------------|
| create_user | 4 | 2 | 1 |
| update_user | 3 | 1 | 1 |
| delete_user | 2 | 1 | 1 |

### Next Steps
- Run: `pytest tests/unit/test_user_service.py -v`
- Verify mocks match actual dependency behavior
- Add integration tests if needed
```

## Notes

- Match existing project style and patterns
- Use type hints in Python tests for better IDE support
- Prefer specific assertions over generic ones
- Keep tests focused: one concept per test
- If unsure about project conventions, ask before generating
