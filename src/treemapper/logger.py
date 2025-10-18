import logging


def setup_logging(verbosity: int) -> None:
    """Configure the logging level based on verbosity."""
    level_map = {
        0: logging.ERROR,
        1: logging.WARNING,
        2: logging.INFO,
        3: logging.DEBUG,
    }
    level = level_map.get(verbosity, logging.INFO)

    # Get the root logger
    root_logger = logging.getLogger()

    # Set the level on the root logger
    root_logger.setLevel(level)

    # Configure handlers - update existing ones or add new if needed
    if root_logger.handlers:
        # Update existing handlers
        for handler in root_logger.handlers:
            handler.setLevel(level)
            if not handler.formatter:
                formatter = logging.Formatter("%(levelname)s: %(message)s")
                handler.setFormatter(formatter)
    else:
        # Add new handler
        handler = logging.StreamHandler()
        handler.setLevel(level)
        formatter = logging.Formatter("%(levelname)s: %(message)s")
        handler.setFormatter(formatter)
        root_logger.addHandler(handler)
