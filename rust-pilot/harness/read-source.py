"""Bounded source inventory for the parity pilot (does not write source)."""
import re
import sys
from pathlib import Path
for name in sys.argv[1:]:
    p = Path(name)
    print(f'--- {p} ---')
    text = p.read_text()
    print(re.sub(r'/\*.*?\*/', '', text, flags=re.S))
