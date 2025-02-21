import sys

import json
import pathlib
import shutil

_, path, worker = sys.argv

d = []
for seed_dir in pathlib.Path(path).iterdir():
    with (seed_dir / 'metadata.json').open() as f:
        j = json.load(f)
    if j['worker'] == worker:
        d.append(seed_dir)
for sd in d:
    shutil.rmtree(sd)
