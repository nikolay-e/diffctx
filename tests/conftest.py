# tests/conftest.py
import pytest
import tempfile
import shutil
from pathlib import Path

@pytest.fixture
def temp_dir():
    dirpath = tempfile.mkdtemp()
    yield Path(dirpath)
    shutil.rmtree(dirpath)

@pytest.fixture
def treemapper_script():
    return Path(__file__).parent.parent / 'src' / 'treemapper' / 'treemapper.py'
