#!/bin/sh
# The security-surface audit. Run locally before pushing; CI runs it on every
# PR. Each check pins a promise e makes to its users — a PR that moves one of
# these boundaries must change this script in the same diff, where the review
# can see it.
set -eu
cd "$(dirname "$0")/.."
fail=0

say() { printf '%s\n' "$*"; }
bad() { fail=1; say "FAIL: $*"; }

# Production-only view of a Rust source tree: truncate each file at its
# first #[cfg(test)] marker before scanning. A test module is always the
# last item in a file — CI denies clippy::items_after_test_module — so
# nothing shipped can follow it. Without this, test fixtures (a placeholder
# host like example.invalid, a scratch File::write into a temp dir) trip
# boundaries these checks mean for the shipped binary, not its tests.
prod_rs() {
  for f in "$@"; do
    awk -v file="$f" '
      /^#\[cfg\(test\)\]/ { exit }
      { print file":"FNR":"$0 }
    ' "$f"
  done
}

# 1. Network surface. e talks to its sign-in and model providers and nothing
#    else — in the shipped binary (src/) or its dev tooling (scripts/). A new
#    host means a new place user data can go — add it here deliberately or
#    the build fails.
allowed_hosts="localhost models.dev auth.openai.com api.openai.com chatgpt.com opencode.ai auth.x.ai api.x.ai api.anthropic.com api.github.com github.com ai-gateway.vercel.sh generativelanguage.googleapis.com api.groq.com api.mistral.ai api.deepseek.com api.cerebras.ai openrouter.ai api.together.xyz api.fireworks.ai"
found_hosts=$(
  { prod_rs $(find src -name '*.rs' 2>/dev/null); find scripts -type f 2>/dev/null | xargs cat 2>/dev/null; } |
    grep -ohE 'https?://[A-Za-z0-9.-]+' | sed -E 's#https?://##' | sort -u
)
for host in $found_hosts; do
  case " $allowed_hosts " in
    *" $host "*) ;;
    *) bad "unlisted network host in src/ or scripts/: $host (grep it, then extend guard.sh deliberately)" ;;
  esac
done

# 2. Sovereign home. e reads only ~/.e — never another tool's store.
if out=$(grep -rnE '[~/"]\.(claude|codex|cursor|gemini|opencode|aws|ssh)\b' src/ --include='*.rs' 2>/dev/null); then
  bad "reference to another tool's home directory:"
  say "$out"
fi

# 3. Home resolution happens in one place. HOME/E_HOME lookups outside these
#    files are a second door into the filesystem.
if out=$(grep -rn 'env::var("HOME")\|env::var("E_HOME")' src/ --include='*.rs' |
    grep -v '^src/core/config/home.rs:' | grep -v '^src/tui/app/mod.rs:'); then
  bad "HOME/E_HOME read outside core/config/home.rs (or tui/app's title display):"
  say "$out"
fi

# 4. Config and credential writes go through core/store.rs — the merge-write
#    path that never wipes unknown keys and chmods auth to 0600. Direct write
#    APIs in core are limited to the files that own a format.
if out=$(prod_rs $(find src/core -name '*.rs' 2>/dev/null) | grep -E 'fs::write|File::create|OpenOptions' |
    grep -v '^src/core/config/store.rs:' | grep -v '^src/core/session.rs:' |
    grep -v '^src/core/config/home.rs:' | grep -v '^src/core/tools/' |
    grep -v '^src/core/update.rs:' | grep -v '^src/core/providers/diagnostics.rs:'); then
  bad "direct file write in src/core outside audited store/session/tool/update/diagnostics paths:"
  say "$out"
fi

# 5. Unsafe code stays where it is audited: the libc terminal poll and the
#    bash tool's process-group kill (setsid + SIGKILL at the timeout).
if out=$(grep -rnE 'unsafe (fn|impl|\{)' src/ --include='*.rs' | grep -v '^src/tui/paint/background.rs:' | grep -v '^src/core/tools/bash.rs:'); then
  bad "unsafe code outside tui/paint/background.rs, core/tools/bash.rs:"
  say "$out"
fi

# 6. Workflow actions are pinned by commit SHA — a moved tag must not be able
#    to rewrite our CI.
for wf in .github/workflows/*.yml; do
  [ -f "$wf" ] || continue
  if out=$(grep -n 'uses:' "$wf" | grep -vE '@[0-9a-f]{40}'); then
    bad "workflow action not pinned to a full commit SHA in $wf:"
    say "$out"
  fi
done

# 7. Exact CODEOWNERS paths must exist. This prevents ownership silently
#    disappearing after a directory rename; glob patterns remain valid and
#    are deliberately skipped here.
for pattern in $(awk '!/^#/ && NF { print $1 }' .github/CODEOWNERS); do
  case "$pattern" in
    '*'|*'*'*|*'?'*|*'['*) continue ;;
  esac
  target=${pattern#/}
  if [ ! -e "$target" ]; then
    bad "CODEOWNERS path does not exist: $pattern"
  fi
done

if [ "$fail" -eq 0 ]; then
  say "guard: all checks passed"
else
  exit 1
fi
