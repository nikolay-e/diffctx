# src/treemapper/logger.py
import logging
import sys

# Убедимся, что имя логгера совпадает с именем пакета (хорошая практика)
# Но для простоты пока конфигурируем корневой логгер
# logger = logging.getLogger('treemapper')


def setup_logging(verbosity: int) -> None:
    """Configure the root logger based on verbosity."""
    level_map = {
        0: logging.ERROR,
        1: logging.WARNING,
        2: logging.INFO,
        3: logging.DEBUG,
    }

    level = level_map.get(verbosity, logging.INFO)

    root_logger = logging.getLogger()

    root_logger.setLevel(level)

    for handler in root_logger.handlers[:]:
        root_logger.removeHandler(handler)

    handler = logging.StreamHandler(sys.stderr)

    handler.setLevel(level)

    formatter = logging.Formatter("%(levelname)s: %(message)s")

    handler.setFormatter(formatter)

    root_logger.addHandler(handler)

    logging.debug(f"Logging setup complete for root logger with level {level} ({logging.getLevelName(level)})")
