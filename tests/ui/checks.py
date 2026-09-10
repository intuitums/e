"""Assertions over completed PTY frames, including display-cell foreground colors."""
from dataclasses import dataclass
import json
from pathlib import Path

from term import replay


@dataclass(frozen=True)
class Frame:
    """An owned terminal snapshot; the replay screen itself is reused."""
    rows: tuple
    colors: tuple

    @classmethod
    def capture(cls, screen):
        """Copy only the cells the checks need, not terminal internals."""
        return cls(tuple(screen.display), tuple(
            tuple(screen.buffer[row][col].fg for col in range(screen.columns))
            for row in range(screen.lines)))

    def find(self, text):
        """Return the first visible row containing text, or None."""
        return next((i for i, row in enumerate(self.rows) if text in row), None)


def tool_tree(frames):
    """Check the dock through live output, wrapping, resize, and completion."""
    observed = [frame for frame in frames if frame.find('┃ draft while tools run') is not None]
    assert len(observed) >= 2, 'no running/completed draft frames captured'
    assert {len(frame.rows) for frame in observed} >= {18, 30}, 'resize was not exercised'
    for frame in observed:
        row = frame.find('┃ draft while tools run')
        assert row == len(frame.rows) - 3, f'composer moved to row {row + 1} of {len(frame.rows)}'
    assert frames[-1].find('CONNECTED_TOOLS_FINISHED') is not None, 'tool turn did not finish'
    live = [frame for frame in observed if frame.find('├ Running') is not None]
    assert live, 'no connected running command was captured'
    assert any(frame.find('ctrl+o to view') is not None for frame in live), 'no in-tree output hint'
    for frame in live:
        row = frame.find('├ Running')
        assert frame.rows[row + 1].startswith('│'), 'wrapped command repeated or lost its branch'
        rail_color = frame.colors[row][0]
        for i, text in enumerate(frame.rows):
            if text.startswith('│'):
                assert frame.colors[i][0] == rail_color, 'output rail detached by a different color'


def shell_composer(frames):
    """Only the shell marker is green; removing it restores the neutral gutter."""
    wrapped = [frame for frame in frames if frame.find('! printf') is not None
               and any(row.startswith('┃ across') for row in frame.rows)]
    assert wrapped, 'no wrapped shell draft captured'
    for frame in wrapped:
        row = frame.find('! printf')
        assert frame.colors[row][0] == '5faf5f', 'shell marker is not the default bash-mode green'
        assert frame.colors[row][2] == 'default', 'command text inherited the green marker color'
        assert frame.colors[row + 1][0] != frame.colors[row][0], 'continuation rail turned green'
        assert frame.rows[row].count('!') == 1, 'shell prefix rendered twice'
    assert frames[-1].find('┃ printf') is not None, 'deleting ! did not restore the rail'


def body_error(frames):
    """The short error and partial reply must coexist in the final frame."""
    final = frames[-1]
    assert final.find('● Error: Provider response interrupted.') is not None, 'missing short error'
    assert final.find('Partial answer before disconnect.') is not None, 'partial response vanished'
    assert all('decoding response body' not in row for row in final.rows), 'backend detail leaked into the UI'


def diff_counts(frames):
    """Verify green additions and red deletions from a real completed edit."""
    final = frames[-1]
    assert final.find('DIFF_FINISHED') is not None, 'edit turn did not finish'
    row = final.find('Edited sample.txt')
    assert row is not None, 'no completed edit row'
    text = final.rows[row]
    for count, expected in [('+2', '5faf5f'), ('-1', 'd75f5f')]:
        assert count in text, f'missing {count} in {text!r}'
        start = text.index(count)
        assert all(color == expected for color in final.colors[row][start:start + len(count)]), \
            f'{count} is not its diff color'
    assert final.colors[row][text.index('/')] not in ('5faf5f', 'd75f5f'), 'separator inherited a diff color'
    assert final.colors[row][text.index('Edited')] not in ('5faf5f', 'd75f5f'), 'tool label inherited a diff color'


CHECKS = {
    'tool-tree': tool_tree,
    'shell-composer': shell_composer,
    'body-error': body_error,
    'diff-counts': diff_counts,
}


def verify(name, directory):
    """Fail on missing coverage or incorrect frames; leave all artifacts intact."""
    directory = Path(directory)
    result = json.loads((directory / 'result.json').read_text())
    assert result['alive_after_steps'], 'application exited during the scenario'
    paths = sorted(directory.glob('*.raw'))
    assert paths, 'no PTY capture'
    data = paths[-1].read_bytes()
    assert b'\x1b[3J' not in data, 'native scrollback was erased'
    frames = []
    replay(paths[-1], 100, 30, on_frame=lambda screen: frames.append(Frame.capture(screen)))
    assert frames, 'no completed synchronized frames'
    CHECKS[name](frames)
