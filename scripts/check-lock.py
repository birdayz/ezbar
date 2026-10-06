#!/usr/bin/env python3
"""Pre-build denylist check using only Python 3.11+ stdlib (does not run Cargo).

This blocks known incident versions, not unknown malware. Review dependency
changes and run advisory checks as well. Keep deny.toml in sync.
"""
import argparse
from pathlib import Path
import sys
import tomllib

BLOCKED = {
    "arrayref": {"0.3.10"},
    "proc-macro1": None,  # malicious lookalike; NOT the legitimate proc-macro2
    "internment": {"0.8.7"},
    "append-only-vec": {"0.1.9"},
}


def violations(lock):
    for package in lock["package"]:
        name, version = package["name"], package["version"]
        if name in BLOCKED and (BLOCKED[name] is None or version in BLOCKED[name]):
            yield f"{name} {version}: blocked incident package/version"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("lockfile", nargs="?", type=Path,
                        default=Path(__file__).resolve().parent.parent / "Cargo.lock")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        for name, versions in BLOCKED.items():
            for version in versions or {"1.0.107", "0.0.1"}:
                assert list(violations({"package": [{"name": name, "version": version}]}))
        assert not list(violations({"package": [
            {"name": "arrayref", "version": "0.3.9"},
            {"name": "proc-macro2", "version": "1.0.107"},
        ]}))
        print("Lock guard self-tests passed")
        return 0
    try:
        with args.lockfile.open("rb") as source:
            errors = list(violations(tomllib.load(source)))
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f"Lock guard failed: {error}", file=sys.stderr)
        return 1
    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    print(f"{args.lockfile}: no denylisted incident versions (not a safety guarantee)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
