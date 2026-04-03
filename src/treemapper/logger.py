import logging

PACKAGE_LOGGER_NAME = "treemapper"

_LOG_LEVEL_MAP = {
    "error": logging.ERROR,
    "warning": logging.WARNING,
    "info": logging.INFO,
    "debug": logging.DEBUG,
}


def setup_logging(verbosity: int | str) -> None:
    if isinstance(verbosity, str):
        level = _LOG_LEVEL_MAP.get(verbosity, logging.INFO)
    else:
        int_to_level = {0: logging.ERROR, 1: logging.WARNING, 2: logging.INFO, 3: logging.DEBUG}
        level = int_to_level.get(verbosity, logging.INFO)

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
