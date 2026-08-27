# Benchmarks

The numbers e's identity depends on: a small binary, a fast start.

```sh
./x bench                          # enforce CI's generous regression budgets
python3 benchmarks/run.py          # record a local result for comparison
```

Each run writes a timestamped report into [results/](results/) — commit the
ones worth keeping (a release, a big refactor) so regressions have a paper
trail. Numbers are only comparable within one machine.

`benchmarks/budgets.json` contains portable ceilings, not aspirational
targets. They are intentionally well above healthy measurements so shared CI
noise does not fail a change; crossing one means a regression deserves an
explicit investigation and budget change in the same review.

Measured today: binary size, cold start (`e --version`, median of 20), and
spawn-to-first-frame on a bare home (median of 5). Candidates for later, when
something makes them matter: renderer throughput on a large transcript, and
session-replay time on a long log.
