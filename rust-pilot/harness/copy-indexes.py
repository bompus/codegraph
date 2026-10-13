"""Take consistent snapshots without opening an original index writable."""
import json
import shutil
import sqlite3
import sys
from pathlib import Path

source_root, destination = map(Path, sys.argv[1:3])
for name in sys.argv[3:]:
    source = source_root / name
    target = destination / name
    target.mkdir(parents=True, exist_ok=True)
    index = target / '.codegraph'
    index.mkdir(exist_ok=True)
    database = index / 'codegraph.db'
    if not database.exists():
        for entry in (source / '.codegraph').iterdir():
            if entry.name.startswith('codegraph.db'):
                continue
            if entry.is_file():
                shutil.copy2(entry, index / entry.name)
        with sqlite3.connect((source / '.codegraph/codegraph.db').as_uri() + '?mode=ro', uri=True) as original:
            original.execute('PRAGMA busy_timeout=5000')
            with sqlite3.connect(database) as copied:
                copied.execute('PRAGMA busy_timeout=5000')
                original.backup(copied)
                assert copied.execute('PRAGMA integrity_check').fetchall() == [('ok',)]
    for entry in source.iterdir():
        if entry.name in ('.git', '.codegraph'):
            continue
        link = target / entry.name
        if not link.exists() and not link.is_symlink():
            link.symlink_to(entry, target_is_directory=entry.is_dir())
    print(json.dumps({'corpus': name, 'copy': str(database), 'bytes': database.stat().st_size}))
