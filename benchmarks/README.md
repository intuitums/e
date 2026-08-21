# Benchmarks

The numbers e's identity depends on: a small binary, a fast start.

```sh
python3 benchmarks/run.py          # uses target/release/e, builds it if absent
```

Each run writes a timestamped report into [results/](results/) — commit the
ones worth keeping (a release, a big refactor) so regressions have a paper
trail. Numbers are only comparable within one machine.

Measured today: binary size, cold start (`e --version`, median of 20), and
spawn-to-first-frame on a bare home (median of 5). Candidates for later, when
something makes them matter: renderer throughput on a large transcript, and
session-replay time on a long log.
