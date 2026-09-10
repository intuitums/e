"""Tolerant pyte screen: agents emit private-mode queries pyte does not model."""
import codecs, json, pathlib, re
import pyte

class Screen(pyte.Screen):
    def report_device_status(self, *args, **kwargs):
        pass
    def write_process_input(self, *args, **kwargs):
        pass

def replay(path, cols, rows, on_frame=None):
    """Replay bytes and resizes; optionally inspect each completed sync frame.

    The callback receives the live screen. Copy any cells retained beyond it.
    """
    data = pathlib.Path(path).read_bytes()
    screen = Screen(cols, rows)
    stream = pyte.Stream(screen)
    decoder = codecs.getincrementaldecoder("utf-8")("replace")
    sidecar = pathlib.Path(str(path) + ".sizes.json")
    sizes = json.loads(sidecar.read_text()) if sidecar.exists() else []
    pending = ""
    def feed(chunk, final=False):
        # pyte predates the Kitty keyboard push/pop protocol. Its parser
        # otherwise leaks the unsupported parameter into visible text.
        nonlocal pending
        text = pending + decoder.decode(chunk, final=final)
        text = re.sub(r"\x1b\[[<>][0-9;]*u", "", text)
        # A Kitty sequence split across a chunk boundary (a resize offset can
        # fall mid-escape) matches neither half. Hold back a trailing partial
        # one so the next chunk can complete it; a full escape is never held.
        pending = ""
        if not final:
            m = re.search(r"\x1b(?:\[[<>][0-9;]*)?\Z", text)
            if m:
                pending = text[m.start():]
                text = text[: m.start()]
        stream.feed(text)

    events = []
    if on_frame is not None:
        events.extend((match.end(), "frame", None)
                      for match in re.finditer(re.escape(b"\x1b[?2026l"), data))
    events.extend((size["offset"], "resize", size) for size in sizes)
    offset = 0
    for end, kind, size in sorted(events, key=lambda event: event[0]):
        feed(data[offset:end])
        if kind == "resize":
            screen.resize(lines=size["rows"], columns=size["cols"])
        else:
            on_frame(screen)
        offset = end
    feed(data[offset:], final=True)
    return screen


if __name__ == "__main__":
    import sys
    print("\n".join(replay(sys.argv[1], int(sys.argv[2]), int(sys.argv[3])).display))
