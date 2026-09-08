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

Measured today: binary size, cold start (`e --version`, median of 20),
spawn-to-first-frame on a bare home (median of 5), and frame assembly and
painting below 10,000 cached Markdown replies (mean of 100 changing-dock
frames). The renderer measurement writes to a sink, so it measures CPU work
and allocations, not terminal throughput. Its 10 ms budget leaves room
within the 33 ms frame interval for event handling and terminal output.

For a renderer-only comparison at 100, 1,000, and 10,000 reply blocks:

```sh
cargo test --release --lib long_session_frame_benchmark -- --ignored --nocapture
```
