#!/usr/bin/env python3
"""Unix PTY smoke benchmark of the actual run loop; no Python packages.

From the workspace root (with dependencies already cached for --offline):
Build: CARGO_TARGET_DIR=target cargo build --offline --locked -j2 -p nexo-tui --example pty_latency
Run: python3 crates/nexo-tui/tests/pty_latency.py target/debug/examples/pty_latency
Measures paste injection -> Python recognition of the synchronized frame showing
its marker, including decoding, cell reconstruction and marker-search overhead;
not pure TUI, frame-receipt or physical display latency. Reconstructs terminal
cells because ratatui emits only differences. Each clear and paste is acknowledged
before the next input.
The generous 500 ms p95 / 2000 ms max gates catch severe regressions only;
passing is not a guarantee of interactive fluency or a baseline comparison.
"""
import codecs
import fcntl
import math
import os
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import time
import unicodedata

BEGIN, END = b'\x1b[?2026h', b'\x1b[?2026l'
N = 30
P95_LIMIT_MS, MAX_LIMIT_MS = 500, 2000


class Screen:
    """Small cell model for this fixed-size, non-scrolling ratatui fixture.

    Implements cursor movement, erasure and UTF-8 cell widths used by its
    backend. Unsupported escapes fail closed rather than acknowledge raw bytes.
    Not a general-purpose terminal emulator (no resize, grapheme shaping, etc.).
    """
    def __init__(self):
        self.cells = [[' '] * 100 for _ in range(36)]
        self.row = self.col = 0
        self.decoder = codecs.getincrementaldecoder('utf-8')()
        self.pending = ''

    def feed(self, data):
        self.pending += self.decoder.decode(data)
        text, i = self.pending, 0
        while i < len(text):
            ch = text[i]
            if ch == '\x1b':
                if i + 1 == len(text):
                    break
                if text[i + 1] == '[':
                    match = re.match(r'\x1b\[([0-?]*)([ -/]*)([@-~])', text[i:])
                    if not match:
                        break
                    self.csi(*match.groups())
                    i += len(match[0])
                    continue
                if text[i + 1] == ']':
                    match = re.match(r'\x1b\].*?(?:\x07|\x1b\\)', text[i:], re.S)
                    if not match:
                        break
                    i += len(match[0])
                    continue
                raise AssertionError(f'unsupported terminal escape: {text[i:i+20]!r}')
            if ch == '\r':
                self.col = 0
            elif ch == '\n':
                self.row = min(35, self.row + 1)
            elif ch == '\b':
                self.col = max(0, self.col - 1)
            elif ch >= ' ' and ch != '\x7f':
                width = 0 if unicodedata.combining(ch) else (2 if unicodedata.east_asian_width(ch) in 'WF' else 1)
                if width:
                    if self.col + width > 100:
                        raise AssertionError('unexpected terminal autowrap')
                    for col in range(self.col, self.col + width):
                        self.clear_cell(self.row, col)
                    self.cells[self.row][self.col] = ch
                    if width == 2:
                        self.cells[self.row][self.col + 1] = ''
                    self.col += width
            i += 1
        self.pending = text[i:]

    def clear_cell(self, row, col):
        # A wide glyph occupies a lead cell plus an empty-string continuation.
        # Overwriting/erasing either half invalidates the entire old glyph.
        cells = self.cells[row]
        if cells[col] == '' and col > 0:
            cells[col - 1] = ' '
        if col + 1 < 100 and cells[col + 1] == '':
            cells[col + 1] = ' '
        cells[col] = ' '

    def csi(self, params, intermediate, command):
        # SGR and cursor appearance do not modify existing cells.
        if command == 'm' or (command == 'q' and intermediate == ' '):
            return
        if command in 'hl' and params.startswith('?'):
            # Only fixture/backend modes: cursor visibility, mouse reporting,
            # bracketed paste and synchronized output do not affect cell layout.
            harmless = {'?25', '?1000', '?1002', '?1003', '?1006', '?1015', '?2004', '?2026'}
            if not intermediate and params in harmless:
                return
            if not intermediate and params == '?1049' and command == 'h':
                self.cells = [[' '] * 100 for _ in range(36)]
                self.row = self.col = 0
                return
            # In particular, origin/autowrap modes and restoring the main screen
            # require semantics this fixed alternate-screen model cannot supply.
            raise AssertionError(f'unsupported DEC mode {params!r} {intermediate!r} {command!r}')
        if intermediate or any(c not in '0123456789;' for c in params):
            raise AssertionError(f'unsupported CSI {params!r} {intermediate!r} {command!r}')
        values = [int(p or 0) for p in params.split(';')]
        n = values[0] or 1
        if command in 'Hf':
            self.row = min(35, max(0, n - 1))
            self.col = min(99, max(0, (values[1] or 1) - 1)) if len(values) > 1 else 0
        elif command in 'ABCD':
            if command == 'A': self.row = max(0, self.row - n)
            if command == 'B': self.row = min(35, self.row + n)
            if command == 'C': self.col = min(99, self.col + n)
            if command == 'D': self.col = max(0, self.col - n)
        elif command == 'G':
            self.col = min(99, n - 1)
        elif command == 'd':
            self.row = min(35, n - 1)
        elif command in 'JK':
            mode = values[0]
            if mode not in (0, 1, 2, 3) or (command == 'K' and mode == 3):
                raise AssertionError(f'unsupported erase mode {mode}')
            if command == 'J' and mode == 3:
                # ED 3 clears scrollback only; this model has no scrollback.
                return
            cursor = self.row * 100 + self.col
            for row in range(36):
                for col in range(100):
                    pos = row * 100 + col
                    affected = mode == 2 or (mode == 0 and pos >= cursor) or (mode == 1 and pos <= cursor)
                    if affected and (command == 'J' or row == self.row):
                        self.clear_cell(row, col)
        else:
            raise AssertionError(f'unsupported CSI command {command!r}')

    def contains(self, marker):
        return any(marker in ''.join(row) for row in self.cells)


def check_screen():
    # Regression: second frame only replaces the digit, not the whole marker.
    screen = Screen()
    screen.feed(b'\x1b[2;3HP000Z')
    screen.feed(b'\x1b[2;6H1')
    assert screen.contains('P001Z') and not screen.contains('P000Z')
    screen.feed(b'\x1b[2;3H\x1b[K')
    assert not screen.contains('P001Z')
    screen.feed(b'\x1b[3;1H' + '界'.encode()[:1])
    screen.feed('界'.encode()[1:] + b'P002Z')
    assert screen.contains('P002Z')

    # ED 3 removes scrollback, not visible cells or the cursor; ED 2 does clear.
    before = [row[:] for row in screen.cells]
    cursor = screen.row, screen.col
    screen.feed(b'\x1b[3J')
    assert screen.cells == before and (screen.row, screen.col) == cursor
    screen.feed(b'\x1b[2J')
    assert all(cell == ' ' for row in screen.cells for cell in row)

    # Replacing either half of a wide glyph must remove the other half too.
    for column, expected in ((1, ['P', ' ', 'Z']), (2, [' ', 'P', 'Z'])):
        screen = Screen()
        screen.feed('界Z'.encode())
        screen.feed(f'\x1b[1;{column}HP'.encode())
        assert screen.cells[0][:3] == expected
        assert not screen.contains('界')
    # A wide replacement can overlap two old wide glyphs at once.
    screen = Screen()
    screen.feed('界界Z'.encode())
    screen.feed('\x1b[1;2H語'.encode())
    assert screen.cells[0][:5] == [' ', '語', '', ' ', 'Z']
    # Partial erasure also invalidates the other half, without touching neighbors.
    for command, column, expected in ((b'K', 2, [' ', ' ', ' ']),
                                      (b'1K', 1, [' ', ' ', 'Z'])):
        screen = Screen()
        screen.feed('界Z'.encode())
        screen.feed(f'\x1b[1;{column}H'.encode() + b'\x1b[' + command)
        assert screen.cells[0][:3] == expected

    # Permit only explicitly modeled fixture modes; reject origin/autowrap,
    # unknown and mixed modes, intermediate bytes and unsupported main-screen restore.
    screen = Screen()
    screen.feed(b'P003Z')
    for mode in (25, 1000, 1002, 1003, 1006, 1015, 2004, 2026):
        for command in 'hl':
            screen.feed(f'\x1b[?{mode}{command}'.encode())
            assert screen.contains('P003Z')
    for sequence in (b'\x1b[?6h', b'\x1b[?7l', b'\x1b[?9999h',
                     b'\x1b[?25;7h', b'\x1b[?25 h', b'\x1b[?1049l'):
        try:
            Screen().feed(sequence)
        except AssertionError:
            pass
        else:
            raise AssertionError(f'unsupported mode accepted: {sequence!r}')
    # Entering the alternate screen clears cells/cursor without losing decoder state.
    screen.feed(b'\x1b[?1049h' + '界'.encode()[:1])
    screen.feed('界'.encode()[1:])
    assert screen.cells[0][:3] == ['界', '', ' ']
    assert (screen.row, screen.col) == (0, 2)


def run(binary, blocks, animations):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 100, 0, 0))
    with tempfile.TemporaryDirectory(prefix='nexo-pty-') as home:
        env = dict(os.environ, HOME=home, XDG_DATA_HOME=home, XDG_CONFIG_HOME=home,
                   XDG_CACHE_HOME=home, TERM='xterm-256color', PTY_BLOCKS=str(blocks),
                   PTY_ANIMATIONS=str(int(animations)), NEXO_INLINE='0', NEXO_NO_ALT_SCREEN='0')
        proc = subprocess.Popen([binary], stdin=slave, stdout=slave, stderr=slave, env=env, cwd=home, start_new_session=True)
        os.close(slave)
        pending = b''
        screen = Screen()

        def frame_until(marker, present=True, timeout=30):
            nonlocal pending
            deadline = time.monotonic() + timeout
            while True:
                start = pending.find(BEGIN)
                if start >= 0:
                    end = pending.find(END, start + len(BEGIN))
                    if end >= 0:
                        # Include startup/control bytes preceding BEGIN as well.
                        frame, pending = pending[:end + len(END)], pending[end + len(END):]
                        screen.feed(frame)
                        if screen.contains(marker) == present:
                            return time.monotonic_ns()
                        continue
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise AssertionError(f'timed out waiting for {marker!r} present={present}; process={proc.poll()}')
                if select.select([master], [], [], min(0.5, remaining))[0]:
                    chunk = os.read(master, 65536)
                    if not chunk:
                        raise AssertionError(f'PTY closed; process={proc.poll()}')
                    pending += chunk
                    if len(pending) > 8 * 1024 * 1024:
                        raise AssertionError('terminal frame exceeds 8 MiB')
        try:
            frame_until('PTY_FIXTURE_READY')
            times = []
            previous = None
            for i in range(N):
                if previous is not None:
                    os.write(master, b'\x15')
                    frame_until(previous, present=False)
                marker = f'P{i:03}Z'
                started = time.monotonic_ns()
                os.write(master, b'\x1b[200~' + marker.encode() + b'\x1b[201~')
                ended = frame_until(marker)
                times.append((ended - started) / 1e6)
                previous = marker
            times.sort()
            p = lambda n: times[math.ceil(len(times)*n/100)-1]
            print(f'PTY paste-to-Python-recognition (includes observer processing) blocks={blocks} animations={animations} n={len(times)} p50={p(50):.3f} p95={p(95):.3f} p99={p(99):.3f} max={p(100):.3f} ms', flush=True)
            assert p(95) <= P95_LIMIT_MS, f'p95 exceeds smoke limit {P95_LIMIT_MS} ms'
            assert p(100) <= MAX_LIMIT_MS, f'max exceeds smoke limit {MAX_LIMIT_MS} ms'
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            os.close(master)


if __name__ == '__main__':
    check_screen()
    binary = os.path.abspath(sys.argv[1])
    for blocks in (1000, 5000):
        for animations in (False, True):
            run(binary, blocks, animations)
