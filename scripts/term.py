"""Tolerant pyte screen: agents emit private-mode queries pyte does not model."""
import pyte

class Screen(pyte.Screen):
    def report_device_status(self, *args, **kwargs):
        pass
    def write_process_input(self, *args, **kwargs):
        pass

def replay(path, cols, rows):
    data = open(path, "rb").read().decode("utf-8", "replace")
    screen = Screen(cols, rows)
    pyte.Stream(screen).feed(data)
    return screen
