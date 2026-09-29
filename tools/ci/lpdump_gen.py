#!/usr/bin/env python3
"""Generate lpdump.c, a probe that prints upstream rsync's parse of an rsyncd.conf.

The probe calls lp_load() and prints every global and module parameter through
the lp_*() accessors daemon-parm.h generates, so its output is exactly what an
upstream daemon reads for each module. With `-g` it loads the way a listening
daemon does at startup, lp_load(file, 1), and prints only the globals. tools/ci/rsyncd_conf_corpus.py uses it to
regenerate the corpus dumps.

Build it inside a configured and built rsync 3.5.1 tree:

    python3 lpdump_gen.py daemon-parm.h > lpdump.c
    gcc -I./zlib -g -O2 -DHAVE_CONFIG_H -Dmain=rsync_main -c main.c -o main_nomain.o
    gcc -I./zlib -g -O2 -DHAVE_CONFIG_H -c lpdump.c -o lpdump.o
    gcc -o lpdump lpdump.o <the Makefile's $(OBJS) with main.o replaced by
        main_nomain.o> <the Makefile's $(LIBS)>
"""
import re,sys
h=open(sys.argv[1]).read()
glob=[];loc=[]
for m in re.finditer(r'^FN_(GLOBAL|LOCAL)_(STRING|STRING_SHELL|INTEGER|BOOL|CHAR)\((lp_\w+), (\w+)\)',h,re.M):
    scope,typ,fn,var=m.groups()
    (glob if scope=='GLOBAL' else loc).append((typ,fn,var))
out=['#include "rsync.h"','#include <stdio.h>','int rsync_main(int argc, char *argv[]);']
def emit(lst, arg):
    for typ,fn,var in lst:
        if typ.startswith('STRING'):
            out.append(f'  printf("{var}=%s\\n", {fn}({arg}));')
        elif typ=='CHAR':
            out.append(f'  printf("{var}=%d\\n", (int){fn}({arg}));')
        else:
            out.append(f'  printf("{var}=%d\\n", {fn}({arg}));')
out.append('static void dump_globals(void) {'); emit(glob,''); emit(loc,'-1'); out.append('}')
out.append('static void dump_module(int i) {'); emit(loc,'i'); out.append('}')
out.append('''int main(int argc, char *argv[]) {
  int i, globals_only = argc == 3 && strcmp(argv[1], "-g") == 0;
  if (argc != 2 && !globals_only) { fprintf(stderr, "usage: lpdump [-g] CONF\\n"); return 2; }
  if (!lp_load(argv[argc - 1], globals_only)) { printf("load=FAIL\\n"); return 1; }
  printf("load=OK\\n[global]\\n");
  dump_globals();
  if (globals_only) return 0;
  for (i = 0; i < lp_num_modules(); i++) { printf("[module %d]\\n", i); dump_module(i); }
  return 0;
}''')
print('\n'.join(out))
