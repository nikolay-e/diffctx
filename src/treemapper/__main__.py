import logging

# Initialize logging early to avoid Python 3.13 issues with argparse
# This ensures logging's internal state is fully set up before argparse runs
try:
    logging.basicConfig(format="%(levelname)s: %(message)s", level=logging.ERROR)
    # Force initialization of logging internals
    logging.getLogger()
except Exception:
    # If logging initialization fails, we still want to proceed
    pass

from treemapper.treemapper import main

if __name__ == "__main__":
    main()
