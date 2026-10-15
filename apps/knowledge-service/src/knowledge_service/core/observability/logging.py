"""Shim: structured logging compatible with gateway's get_logger."""
import logging
import structlog


def get_logger(name: str = __name__) -> structlog.stdlib.BoundLogger:
    """Return a structlog-wrapped logger."""
    return structlog.wrap_logger(
        logging.getLogger(name),
        processors=[
            structlog.stdlib.add_log_level,
            structlog.stdlib.add_logger_name,
            structlog.dev.ConsoleRenderer(),
        ],
    )
