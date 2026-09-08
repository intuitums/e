# e-sdk

Programmatic access to e's coding-agent core from Rust: create a session,
send prompts, subscribe to one ordered event stream, and run tools — the
same harness the terminal frontend drives, without a terminal.

Status: minimal facade implemented; unstable. See [docs/sdk.md](../docs/sdk.md)
for the planned surface and
[docs/decisions/0002-rust-sdk-package.md](../docs/decisions/0002-rust-sdk-package.md)
for why this is a package of its own.
