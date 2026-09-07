#!/usr/bin/env python3
"""Name the SOURCE LINE of every bounds check left in a function.

WHY this and not a debug line-table: an inlined bounds check is attributed to
core/src/slice/index.rs, which names the check and not the caller, so every
guard in the codec reads as the same useless line. But every panic call passes a
`&core::panic::Location`, and rustc emits that as an `anon.*` rodata object:

    anon.X.3:
        .quad  anon.X.2          <- the filename string
        .asciz "<len:u64><line:u32><col:u32>"

So for each block that reaches a panic symbol, the `leaq anon.N(%rip)` in that
block names the exact line of OUR code that failed to prove its index. Needs no
source edit and no debug info, so it cannot perturb what it measures, and it
turns "this function has 18 guards" into a ranked worklist in one pass.

Usage: python3 bench/panic_sites.py <file.s> <symbol-substring>
"""
import re, io, sys, struct, collections

BS = chr(92)  # written this way so a heredoc cannot eat the escape

path, want = sys.argv[1], sys.argv[2]
lines = io.open(path, encoding='utf-8', errors='replace').read().split('\n')

LBL = re.compile(r'^([A-Za-z_$][\w$.@]*):\s*$')
ANON = re.compile(r'anon\.[0-9a-f]+\.\d+')

# ---- pass 1: every anon object's .quad target and .asciz payload -----------
objs = {}
cur = None
for ln in lines:
    m = LBL.match(ln)
    if m:
        cur = m.group(1)
        if cur.startswith('anon.'):
            objs[cur] = {'quad': None, 'bytes': None}
        continue
    if cur in objs:
        s = ln.strip()
        if s.startswith('.quad') and objs[cur]['quad'] is None:
            objs[cur]['quad'] = s.split(None, 1)[1].strip()
        elif s.startswith('.asciz') and objs[cur]['bytes'] is None:
            raw = s[s.index('"') + 1:s.rindex('"')]
            out, i = bytearray(), 0
            ESC = {'n': 10, 't': 9, '"': 34, BS: 92, '0': 0}
            while i < len(raw):
                if raw[i] == BS:
                    if raw[i + 1].isdigit():
                        out.append(int(raw[i + 1:i + 4], 8)); i += 4
                    else:
                        out.append(ESC.get(raw[i + 1], 63)); i += 2
                else:
                    out.append(ord(raw[i])); i += 1
            objs[cur]['bytes'] = bytes(out)
        elif s.startswith('.ascii') and objs[cur]['bytes'] is None:
            objs[cur]['bytes'] = s[s.index('"') + 1:s.rindex('"')].encode()

_len_cache = {}

def _file_lines(name):
    """Line count of a named source file, for the plausibility check below."""
    if name not in _len_cache:
        try:
            _len_cache[name] = io.open(name, encoding='utf-8', errors='replace').read().count(chr(10)) + 1
        except OSError:
            _len_cache[name] = None
    return _len_cache[name]


def location(a):
    """Resolve a Location anon to 'file:line', or None if it is not one.

    VERIFIED, not merely decoded. A block that reaches a panic can reference
    several rodata objects -- the Location, a message string, a formatting
    table -- and decoding the wrong one yields a confident, wrong line. The
    first version of this script reported line 16288 of a 4,515-line file and
    was believed for exactly as long as it took to open the file. So a
    candidate only counts if it names a real .rs path AND its line is inside
    that file; everything else is reported as unresolved, which is honest and
    still leaves a ranked worklist.
    """
    o = objs.get(a)
    if not o or not o['quad'] or not o['bytes'] or len(o['bytes']) < 12:
        return None
    f = objs.get(o['quad'])
    if not f or not f['bytes']:
        return None
    name = f['bytes'].decode('utf-8', 'replace')
    if not name.endswith('.rs'):
        return None
    line = struct.unpack('<I', o['bytes'][8:12])[0]
    n = _file_lines(name.replace(chr(92), '/'))
    if line == 0 or (n is not None and line > n):
        return None
    return '%s:%d' % (name, line)

# ---- pass 2: inside the wanted symbol, blocks that reach a panic ----------
sym, body = None, []
for ln in lines:
    m = LBL.match(ln)
    if m:
        if sym and want in sym and body:
            break
        sym = m.group(1)
        if want in sym:
            body = []
        continue
    if sym and want in sym:
        body.append(ln)

hits = collections.Counter()
block = []
for ln in body:
    if re.match(r'^\.LBB', ln.strip()) or LBL.match(ln):
        block = []
    block.append(ln)
    if 'panic' in ln or 'slice_index_fail' in ln or 'unwrap_failed' in ln:
        # Try EVERY anon the block references, nearest first, and take the
        # first that verifies. Guessing one and trusting it is what produced
        # the impossible line numbers described above.
        loc = None
        for a in reversed([x for b in block for x in ANON.findall(b)]):
            loc = location(a)
            if loc:
                break
        hits[loc or '(unresolved)'] += 1
        block = []

print('guard sites in %s (%d total)' % (want, sum(hits.values())))
for loc, n in hits.most_common():
    print('  %3d  %s' % (n, loc))
if not hits:
    print('  (none resolved)')
