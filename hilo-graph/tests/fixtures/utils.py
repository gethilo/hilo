import os
import sys
from collections import defaultdict


def get_env(key: str) -> str:
    return os.environ.get(key, "")