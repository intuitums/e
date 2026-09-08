"""Run a command on a real PTY, answer startup probes, and capture output.

CAP_RESIZE_AFTER is a byte marker that triggers one resize to CAP_RESIZE_COLS
and CAP_RESIZE_ROWS. The sidecar records the byte offset for terminal replay.
"""
import json, os, pty, sys, time, select, fcntl, termios, struct, signal

out_path, cols, rows, wait_before, wait_after, *cmd = sys.argv[1:]
cols, rows = int(cols), int(rows)
wait_before, wait_after = float(wait_before), float(wait_after)
prompt = os.environ.get("CAP_PROMPT", "")
wait_for = os.environ.get("CAP_WAIT_FOR", "").encode()
exit_keys = os.environ.get("CAP_EXIT", "")
exit_wait = float(os.environ.get("CAP_EXIT_WAIT", "1"))
resize_after = os.environ.get("CAP_RESIZE_AFTER", "").encode()

pid, fd = pty.fork()
if pid == 0:
    os.execvp(cmd[0], cmd)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

buf = bytearray()
deadline = time.time() + wait_before
typed = False
background_answered = False
cursor_answered = False
resized = False
sizes = []
end = time.time() + wait_before + wait_after
while time.time() < end:
    r, _, _ = select.select([fd], [], [], 0.2)
    if r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk
    # Startup probes read stdin synchronously. Typing before their replies
    # can consume the prompt as a terminal response instead of user input.
    if not background_answered and b"\x1b]11;?" in buf:
        os.write(fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
        background_answered = True
    if not cursor_answered and b"\x1b[6n" in buf:
        os.write(fd, b"\x1b[1;1R")
        cursor_answered = True
    if not typed and time.time() >= deadline and prompt and b"\x1b[?2026l" in buf:
        os.write(fd, prompt.encode() + b"\r")
        typed = True
    if resize_after and not resized and resize_after in buf:
        cols = int(os.environ["CAP_RESIZE_COLS"])
        rows = int(os.environ["CAP_RESIZE_ROWS"])
        sizes.append({"offset": len(buf), "cols": cols, "rows": rows})
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
        resized = True
    if typed and wait_for and wait_for in buf:
        break

# Optional graceful exit lets tests pin terminal cleanup bytes instead of
# ending every capture with SIGTERM.
if exit_keys:
    try:
        os.write(fd, exit_keys.encode())
    except OSError:
        pass
    deadline = time.time() + exit_wait
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.1)
        if not r:
            continue
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            break
        if not chunk:
            break
        buf += chunk

open(out_path, "wb").write(bytes(buf))
with open(out_path + ".sizes.json", "w") as file:
    json.dump(sizes, file)
try:
    os.close(fd)
except OSError:
    pass


def reap():
    try:
        waited, _ = os.waitpid(pid, os.WNOHANG)
    except ChildProcessError:
        return True
    return bool(waited)


def stop_group(sig):
    try:
        os.killpg(pid, sig)
    except ProcessLookupError:
        pass


waited = reap()
for sig in (signal.SIGTERM, signal.SIGKILL):
    if waited:
        break
    stop_group(sig)
    deadline = time.monotonic() + 1
    while time.monotonic() < deadline:
        if reap():
            waited = True
            break
        time.sleep(0.05)
if not waited:
    raise RuntimeError("pty child survived SIGKILL")
print(f"captured {len(buf)} bytes -> {out_path}")
