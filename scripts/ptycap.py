"""Run a command on a real pty of fixed size, type a prompt, capture raw output."""
import os, pty, sys, time, select, fcntl, termios, struct, signal

out_path, cols, rows, wait_before, wait_after, *cmd = sys.argv[1:]
cols, rows = int(cols), int(rows)
wait_before, wait_after = float(wait_before), float(wait_after)
prompt = os.environ.get("CAP_PROMPT", "")
exit_keys = os.environ.get("CAP_EXIT", "")
exit_wait = float(os.environ.get("CAP_EXIT_WAIT", "1"))

pid, fd = pty.fork()
if pid == 0:
    os.execvp(cmd[0], cmd)

fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))

buf = bytearray()
deadline = time.time() + wait_before
typed = False
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
    if not typed and time.time() >= deadline and prompt:
        os.write(fd, prompt.encode() + b"\r")
        typed = True

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
waited, _ = os.waitpid(pid, os.WNOHANG)
if not waited:
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    deadline = time.time() + 1
    while time.time() < deadline:
        waited, _ = os.waitpid(pid, os.WNOHANG)
        if waited:
            break
        time.sleep(0.05)
if not waited:
    try:
        os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    os.waitpid(pid, 0)
print(f"captured {len(buf)} bytes -> {out_path}")
