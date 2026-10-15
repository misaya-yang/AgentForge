"""Shared quiz grading and persistence primitives.

Generation is a capability-worker operation. Gateway-facing read, submit, and
delete operations live here.
"""

from .quiz_access_service import QuizAccessService
from .quiz_grader import QuizGrader

__all__ = ["QuizAccessService", "QuizGrader"]
