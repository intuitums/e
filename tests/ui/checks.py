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


def tui_mode(frames):
    """Default startup is compact; the settings switch pins and unpins the dock."""
    first = frames[0]
    row = first.find('┃ ')
    assert row is not None and row < len(first.rows) - 3, 'startup is not inline'
    assert any(frame.find('TUI Mode') is not None
               and any('Inline  Fullscreen' in row for row in frame.rows)
               for frame in frames), 'missing TUI Mode choices'
    bare = [frame for frame in frames if frame.find('TUI Mode') is None]
    assert any(frame.find('┃ ') == len(frame.rows) - 3 for frame in bare), 'Fullscreen option did not pin'
    final = frames[-1]
    row = final.find('┃ ')
    assert row is not None and row < len(final.rows) - 3, 'inline option did not unpin'


def single_tool(frames):
    """A single wrapped call retains its rails and footer after completion."""
    for action in ('Running', 'Ran'):
        observed = [frame for frame in frames if frame.find(f'├ {action} printf') is not None]
        assert observed, f'no wrapped {action} call captured'
        for frame in observed:
            start = frame.find(f'├ {action} printf')
            end = frame.find('└ ctrl+o to view')
            assert end is not None and end > start, 'missing closing hint below call'
            for row in range(start + 1, end):
                assert frame.rows[row].startswith('│ '), 'wrapped call has a broken rail'
                assert frame.colors[row][0] == frame.colors[start][0], 'rail color changed'
    completed = [frame for frame in frames if frame.find('SINGLE_TOOL_FINISHED') is not None]
    assert {len(frame.rows[0]) for frame in completed} >= {44, 140}, 'completed label never resized'
    for frame in completed:
        start = frame.find('├ Ran printf')
        end = frame.find('└ ctrl+o to view')
        assert start is not None and end is not None
        if len(frame.rows[0]) == 44:
            assert end == start + 2, 'narrow label exceeded its two-row budget'
            assert frame.rows[end - 1].rstrip().endswith('…'), 'missing truncation marker'
        else:
            assert end == start + 1, 'wide label did not reflow'
            assert 'long enough to wrap' in frame.rows[start], 'resize did not reveal the source'
    assert frames[-1].find('SINGLE_TOOL_FINISHED') is not None, 'single tool did not finish'


def heredoc_tool(frames):
    """The main tree hides heredoc bodies; Ctrl+O still shows the full command."""
    final = frames[-1]
    assert final.find('HEREDOC_FINISHED') is not None, 'heredoc turn did not finish'
    row = final.find("├ Ran cat <<'E_LABEL_SCRIPT' >/dev/null …")
    assert row is not None, 'missing abbreviated heredoc header'
    assert final.rows[row + 1].startswith('└ ctrl+o to view'), 'heredoc body occupied preview rows'
    assert final.find('HEREDOC_BODY_ONLY') is None, 'body leaked into the main tree'
    assert any(frame.find('HEREDOC_BODY_ONLY') is not None for frame in frames), 'review lost the body'
    for mode, ending in [('Review', '└ 1 more rows · → to expand'), ('Full detail', '└ REVIEW_LINE_FOUR')]:
        observed = [frame for frame in frames if frame.find(f'┃ {mode} ·') is not None]
        assert observed, f'{mode} was not opened'
        for frame in observed:
            start = frame.find("├ Ran cat <<'E_LABEL_SCRIPT'")
            end = frame.find(ending)
            assert start is not None and end is not None and end > start, 'review closed before its output'
            assert all(frame.rows[row].startswith('│ ') for row in range(start + 1, end)), 'broken review rail'



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
        assert sum('ctrl+o to view' in row for row in frame.rows) == 1, 'duplicate review hints'
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
    'heredoc-tool': heredoc_tool,
    'single-tool': single_tool,
    'tui-mode': tui_mode,
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
