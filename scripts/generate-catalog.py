#!/usr/bin/env python3
"""Sync provider data from models.dev (the catalog the reference generates
its provider files from).

    python3 scripts/generate-catalog.py            # show what would change
    python3 scripts/generate-catalog.py --write    # apply it

For every provider file in src/core/provider/providers/, models found on
models.dev get their context_window corrected. Models e lists that
models.dev lacks are left alone; models.dev models e doesn't list are
reported, never auto-added — the seed list stays curated, the live
/models overlay covers discovery.
"""
import json, pathlib, sys, urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
PROVIDERS = ROOT / "src" / "core" / "provider" / "providers"
# models.dev ids for e's provider names, where they differ.
DEV_IDS = {"opencode-go": "opencode-go", "opencode": "opencode",
           "xai": "xai", "openai": "openai", "anthropic": "anthropic",
           "openai-codex": "openai"}


def fetch():
    request = urllib.request.Request(
        "https://models.dev/api.json",
        headers={"User-Agent": "e-catalog-sync", "Accept": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as r:
            return json.load(r)
    except Exception as err:
        sys.exit(f"models.dev fetch failed: {err}")


def main():
    write = "--write" in sys.argv
    data = fetch()
    for path in sorted(PROVIDERS.glob("*.json")):
        provider = json.loads(path.read_text())
        dev = data.get(DEV_IDS.get(provider["name"], provider["name"]))
        if not dev:
            print(f"{provider['name']}: not on models.dev, skipped")
            continue
        dev_models = dev.get("models", {})
        changed = False
        for model in provider.get("models", []):
            entry = dev_models.get(model["id"])
            if not entry:
                print(f"{provider['name']}/{model['id']}: not on models.dev")
                continue
            window = entry.get("limit", {}).get("context") or entry.get("contextWindow")
            if window and window != model.get("context_window"):
                print(f"{provider['name']}/{model['id']}: "
                      f"{model.get('context_window')} -> {window}")
                model["context_window"] = window
                changed = True
        unlisted = [m for m in dev_models if not any(
            x["id"] == m for x in provider.get("models", []))]
        if unlisted:
            print(f"{provider['name']}: models.dev also has: "
                  f"{', '.join(sorted(unlisted)[:8])}"
                  f"{' …' if len(unlisted) > 8 else ''}")
        if write and changed:
            path.write_text(json.dumps(provider, indent="\t") + "\n")
    if not write:
        print("(dry run — pass --write to apply)")


if __name__ == "__main__":
    main()
