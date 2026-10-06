import os
import tempfile

os.environ["PAPERBOY_DATA_DIR"] = tempfile.mkdtemp(prefix="paperboy-tests-")
