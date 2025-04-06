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

    logging.basicConfig(level=level, format="%(levelname)s: %(message)s")
