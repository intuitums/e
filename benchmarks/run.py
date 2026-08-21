#!/usr/bin/env python3
"""e's benchmark suite: the numbers e's identity depends on.

Measures the release binary — build it first (`cargo build --release`) or let
this script do it. Writes a timestamped report into benchmarks/results/ and
prints it. Run on a quiet machine; medians over repeated runs keep the noise
down, but results are only comparable to results from the same machine.
"""
import os, pty, select, statistics, subprocess, sys, time, platform, datetime, fcntl, struct, termios

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BINARY = os.path.join(ROOT, "target", "release", "e")


def build():
    subprocess.run(["cargo", "build", "--release"], cwd=ROOT, check=True,
                   capture_output=True)


def binary_size():
    return os.path.getsize(BINARY)


def cold_start_version(runs=20):
    """Process spawn to exit for `e --version`: the floor of every launch."""
    samples = []
    for _ in range(runs):
        t0 = time.perf_counter()
        subprocess.run([BINARY, "--version"], capture_output=True, check=True)
        samples.append((time.perf_counter() - t0) * 1000)
    return statistics.median(samples)


def boot_to_first_frame(runs=5):
    """Spawn to the banner reaching the terminal: what launch actually feels
    like. Answers the OSC 11 background query the way a real terminal does —
    otherwise e's 400 ms detection timeout dominates the number. Reaps with
    WNOHANG: blocking waitpid on a SIGKILLed pty child can wedge on macOS."""
    samples = []
    for i in range(runs):
        home = f"/tmp/e-bench-home-{os.getpid()}-{i}"
        t0 = time.perf_counter()
        pid, fd = pty.fork()
        if pid == 0:
            os.execve(BINARY, ["e"], dict(os.environ, E_HOME=home, TERM="xterm-256color"))
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 24, 80, 0, 0))
        buf = b""
        answered = False
        deadline = time.time() + 10
        while b"Run /help" not in buf and time.time() < deadline:
            r, _, _ = select.select([fd], [], [], 0.02)
            if r:
                try:
                    buf += os.read(fd, 65536)
                except OSError:
                    break
            if not answered and b"\x1b]11;?" in buf:
                os.write(fd, b"\x1b]11;rgb:0000/0000/0000\x1b\\")
                answered = True
        samples.append((time.perf_counter() - t0) * 1000)
        os.kill(pid, 9)
        for _ in range(100):
            done, _ = os.waitpid(pid, os.WNOHANG)
            if done == pid:
                break
            time.sleep(0.02)
        os.close(fd)
        subprocess.run(["rm", "-rf", home], check=False)
    return statistics.median(samples)


def main():
    if not os.path.exists(BINARY) or "--build" in sys.argv:
        print("building release…", file=sys.stderr)
        build()
    commit = subprocess.run(["git", "rev-parse", "--short", "HEAD"], cwd=ROOT,
                            capture_output=True, text=True).stdout.strip()
    version = subprocess.run([BINARY, "--version"], capture_output=True,
                             text=True).stdout.strip()

    size = binary_size()
    cold = cold_start_version()
    boot = boot_to_first_frame()

    stamp = datetime.datetime.now().strftime("%Y-%m-%d_%H-%M")
    report = "\n".join([
        f"date:            {stamp}",
        f"version:         {version} ({commit})",
        f"machine:         {platform.machine()} · {platform.system()} {platform.release()}",
        f"binary size:     {size} bytes ({size / 1024 / 1024:.2f} MiB)",
        f"cold start:      {cold:.1f} ms   (e --version, median of 20)",
        f"first frame:     {boot:.1f} ms   (spawn → banner on a bare home, median of 5)",
        "",
    ])
    out = os.path.join(ROOT, "benchmarks", "results", f"{stamp}_{commit}.txt")
    with open(out, "w") as f:
        f.write(report)
    print(report)
    print(f"written: {os.path.relpath(out, ROOT)}")


if __name__ == "__main__":
    main()
