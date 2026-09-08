"""Tolerant pyte screen: agents emit private-mode queries pyte does not model."""
import codecs, json, pathlib, re
import pyte

class Screen(pyte.Screen):
    def report_device_status(self, *args, **kwargs):
        pass
    def write_process_input(self, *args, **kwargs):
        pass

def replay(path, cols, rows):
    """Replay PTY bytes and the capture's optional resize sidecar in order."""
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

    offset = 0
    for size in sizes:
        feed(data[offset:size["offset"]])
        screen.resize(lines=size["rows"], columns=size["cols"])
        offset = size["offset"]
    feed(data[offset:], final=True)
    return screen


if __name__ == "__main__":
    import sys
    print("\n".join(replay(sys.argv[1], int(sys.argv[2]), int(sys.argv[3])).display))
