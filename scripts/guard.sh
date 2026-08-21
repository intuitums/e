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

# 1. Network surface. e talks to its sign-in and model providers and nothing
#    else. A new host in src/ means a new place user data can go — add it
#    here deliberately or the build fails.
allowed_hosts="localhost auth.openai.com api.openai.com chatgpt.com opencode.ai auth.x.ai api.x.ai api.anthropic.com"
found_hosts=$(grep -rhoE 'https?://[A-Za-z0-9.-]+' src/ 2>/dev/null | sed -E 's#https?://##' | sort -u)
for host in $found_hosts; do
  case " $allowed_hosts " in
    *" $host "*) ;;
    *) bad "unlisted network host in src/: $host (grep it, then extend guard.sh deliberately)" ;;
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
    grep -v '^src/core/config/home.rs:' | grep -v '^src/main.rs:'); then
  bad "HOME/E_HOME read outside core/config/home.rs (or main.rs's title display):"
  say "$out"
fi

# 4. Config and credential writes go through core/store.rs — the merge-write
#    path that never wipes unknown keys and chmods auth to 0600. Direct write
#    APIs in core are limited to the files that own a format.
if out=$(grep -rnE 'fs::write|File::create|OpenOptions' src/core/ --include='*.rs' |
    grep -v '^src/core/config/store.rs:' | grep -v '^src/core/session.rs:' |
    grep -v '^src/core/config/home.rs:' | grep -v '^src/core/tools/'); then
  bad "direct file write in src/core outside config/{store,home}.rs, session.rs, tools:"
  say "$out"
fi

# 5. Unsafe code stays where it is audited (the libc terminal poll).
if out=$(grep -rnE 'unsafe (fn|impl|\{)' src/ --include='*.rs' | grep -v '^src/tui/background.rs:'); then
  bad "unsafe code outside tui/background.rs:"
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

if [ "$fail" -eq 0 ]; then
  say "guard: all checks passed"
else
  exit 1
fi
