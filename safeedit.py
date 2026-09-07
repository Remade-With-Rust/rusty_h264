"""Atomic in-place source edit: temp file in the same dir, then os.replace.

open(P,'w') TRUNCATES FIRST; when the write then failed with a permission error
it destroyed a 4,086-line source. Never edit a source file any other way here.
"""
import io, os, tempfile, sys

def edit(path, subs, require_growth=True):
    raw = io.open(path, encoding='utf-8', newline='').read()
    cr = raw.count('\r\n') > raw.count('\n') / 2
    s = raw.replace('\r\n', '\n') if cr else raw
    before = len(s)
    total = 0
    for old, new, lbl, want in subs:
        n = s.count(old)
        print("  %-40s sites=%d%s" % (lbl, n, "" if (want is None or n == want) else "  <-- EXPECTED %s" % want))
        if want is not None and n != want:
            raise SystemExit("ABORT: %s expected %s sites, found %d" % (lbl, want, n))
        s = s.replace(old, new); total += n
    if require_growth and len(s) <= before:
        raise SystemExit("ABORT: output did not grow (%d -> %d)" % (before, len(s)))
    if cr: s = s.replace('\n', '\r\n')
    d = os.path.dirname(os.path.abspath(path)) or '.'
    fd, tmp = tempfile.mkstemp(dir=d, suffix='.tmp'); os.close(fd)
    io.open(tmp, 'w', encoding='utf-8', newline='').write(s)
    os.replace(tmp, path)
    print("  atomically written, %d substitutions" % total)
    return total


def restore(src, dst):
    """Restore `dst` from a backup, atomically.

    `cp backup src.rs` has the SAME hazard as `open(P,'w')`: it truncates the
    destination and then writes. On 2026-09-07 that truncation succeeded and the
    write failed ("No medium found", on a drive with 833 GB free), leaving a
    4,649-line source at ZERO BYTES -- the second time this exact class of
    incident has destroyed a file in this repo. Backups and restores go through
    here for the same reason edits do.
    """
    data = io.open(src, encoding='utf-8', newline='').read()
    if not data:
        raise SystemExit("ABORT: backup %s is empty" % src)
    d = os.path.dirname(os.path.abspath(dst)) or '.'
    fd, tmp = tempfile.mkstemp(dir=d, suffix='.tmp'); os.close(fd)
    io.open(tmp, 'w', encoding='utf-8', newline='').write(data)
    os.replace(tmp, dst)
    print("  restored %s from %s (%d chars)" % (dst, src, len(data)))
    return len(data)


def backup(src, dst):
    """Snapshot `src` to `dst`, atomically. See `restore` for why not `cp`."""
    return restore(src, dst)
