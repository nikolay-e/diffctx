import logging

PACKAGE_LOGGER_NAME = "treemapper"


def setup_logging(verbosity: int) -> None:
    level_map = {
        0: logging.ERROR,
        1: logging.WARNING,
        2: logging.INFO,
        3: logging.DEBUG,
    }
    level = level_map.get(verbosity, logging.INFO)

    pkg_logger = logging.getLogger(PACKAGE_LOGGER_NAME)
    pkg_logger.setLevel(level)

    if pkg_logger.handlers:
        for handler in pkg_logger.handlers:
            handler.setLevel(level)
            if not handler.formatter:
                handler.setFormatter(logging.Formatter("%(levelname)s: %(message)s"))
    else:
        handler = logging.StreamHandler()
        handler.setLevel(level)
        handler.setFormatter(logging.Formatter("%(levelname)s: %(message)s"))
        pkg_logger.addHandler(handler)
