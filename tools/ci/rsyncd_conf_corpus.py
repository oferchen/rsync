#!/usr/bin/env python3
"""Maintain the rsyncd.conf corpus that pins oc's config parser to upstream.

Each corpus entry is a directory under crates/daemon/tests/rsyncd_conf_corpus
holding an `rsyncd.conf` (plus any `&include`/`&merge` targets it names, by a
path relative to that directory) and `upstream.dump`, the parse upstream rsync
3.5.1's own loadparm.c/params.c produces for it.

Subcommands:

  collect RAW_DIR CORPUS_DIR
      Turn configs captured from an upstream testsuite run into corpus
      entries. RAW_DIR holds one directory per daemon start, as written by a
      wrapper passed to `runtests.py --rsync-bin`: `rsyncd.conf`,
      `source-path` (where the testsuite wrote it) and `incl.*` copies of its
      `&include`/`&merge` targets. Scratch paths are rewritten under
      /corpus/<test> and duplicates are dropped.

  regen CORPUS_DIR LPDUMP
      Rewrite every `upstream.dump`, and `upstream-startup.dump` (the
      globals a listening daemon reads at startup, `lpdump -g`), by running
      LPDUMP, a program linked
      against an upstream rsync build that calls lp_load() and prints every
      global and module parameter through its lp_*() accessor. Build it with
      tools/ci/lpdump_gen.py (see that file). It runs in the entry directory
      so relative `&include` paths resolve the way the daemon's cwd does.
"""
import hashlib
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

DIRECTIVE = re.compile(r'^(\s*&(?:include|merge)[\s=]+)(\S.*?)\s*$', re.I)


def normalize(text, scratch, test):
    return text.replace(scratch, f'/corpus/{test}')


def collect(raw_dir, corpus_dir):
    seen = {}
    for entry in sorted(Path(raw_dir).iterdir()):
        conf = entry / 'rsyncd.conf'
        if not conf.is_file():
            continue
        source = Path((entry / 'source-path').read_text().strip())
        scratch = str(source.parent)
        test = source.parent.name
        text = normalize(conf.read_text(errors='surrogateescape'), scratch, test)
        lines = []
        for line in text.splitlines():
            m = DIRECTIVE.match(line)
            if m:
                line = m.group(1) + 'incl.' + Path(m.group(2)).name
            lines.append(line)
        text = '\n'.join(lines) + '\n'
        incl = {p.name: p for p in entry.glob('incl.*')}
        key = hashlib.sha256(text.encode('utf-8', 'surrogateescape')
                             + ''.join(sorted(incl)).encode()).hexdigest()
        if key in seen:
            continue
        seen[key] = test
        n = sum(1 for v in seen.values() if v == test)
        out = Path(corpus_dir) / f'testsuite-{test}-{n}'
        out.mkdir(parents=True, exist_ok=True)
        (out / 'rsyncd.conf').write_text(text, errors='surrogateescape')
        for name, path in incl.items():
            dest = out / name
            if path.is_dir():
                shutil.copytree(path, dest, dirs_exist_ok=True)
                files = dest.rglob('*')
            else:
                shutil.copy(path, dest)
                files = [dest]
            for f in files:
                if f.is_file():
                    f.write_text(normalize(f.read_text(errors='surrogateescape'),
                                           scratch, test), errors='surrogateescape')
    print(f'{len(seen)} unique configs')


def regen(corpus_dir, lpdump):
    lpdump = str(Path(lpdump).resolve())
    env = {'PATH': os.environ.get('PATH', '/usr/bin:/bin'), 'LC_ALL': 'C'}
    for entry in sorted(Path(corpus_dir).iterdir()):
        if not (entry / 'rsyncd.conf').is_file():
            continue
        run = subprocess.run([lpdump, 'rsyncd.conf'], cwd=entry, env=env,
                             capture_output=True, text=True, errors='surrogateescape')
        (entry / 'upstream.dump').write_text(run.stdout, errors='surrogateescape')
        if run.returncode != 0:
            print(f'{entry.name}: lp_load failed', file=sys.stderr)
        run = subprocess.run([lpdump, '-g', 'rsyncd.conf'], cwd=entry, env=env,
                             capture_output=True, text=True, errors='surrogateescape')
        (entry / 'upstream-startup.dump').write_text(run.stdout, errors='surrogateescape')


def main(argv):
    if len(argv) == 4 and argv[1] == 'collect':
        collect(argv[2], argv[3])
    elif len(argv) == 4 and argv[1] == 'regen':
        regen(argv[2], argv[3])
    else:
        print(__doc__, file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv))
