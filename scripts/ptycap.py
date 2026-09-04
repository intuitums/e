"""Run a command on a real pty of fixed size, type a prompt, capture raw output."""
import os, pty, sys, time, select, fcntl, termios, struct, signal

out_path, cols, rows, wait_before, wait_after, *cmd = sys.argv[1:]
cols, rows = int(cols), int(rows)
wait_before, wait_after = float(wait_before), float(wait_after)
prompt = os.environ.get("CAP_PROMPT", "")
wait_for = os.environ.get("CAP_WAIT_FOR", "").encode()
exit_keys = os.environ.get("CAP_EXIT", "")
exit_wait = float(os.environ.get("CAP_EXIT_WAIT", "1"))
exit_wait_for = os.environ.get("CAP_EXIT_WAIT_FOR", "").encode()
final_exit = os.environ.get("CAP_FINAL_EXIT", "")
final_exit_wait = float(os.environ.get("CAP_FINAL_EXIT_WAIT", "1"))

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
    if typed and wait_for and wait_for in buf:
        break

# Optional staged keys let tests inspect an intermediate frame, then exit
# cleanly and pin the terminal cleanup bytes.
if exit_keys:
    action_start = len(buf)
    try:
        os.write(fd, exit_keys.encode())
    except OSError:
        pass
    deadline = time.time() + exit_wait
    while time.time() < deadline:
        r, _, _ = select.select([fd], [], [], 0.1)
        if r:
            try:
                chunk = os.read(fd, 65536)
            except OSError:
                break
            if not chunk:
                break
            buf += chunk
        if exit_wait_for and exit_wait_for in buf[action_start:]:
            break

if final_exit:
    try:
        os.write(fd, final_exit.encode())
    except OSError:
        pass
    deadline = time.time() + final_exit_wait
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
