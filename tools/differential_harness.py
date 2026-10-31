#!/usr/bin/env python3
"""Compare pinned-reference oracle wrappers using deterministic JSON lines."""

import json
import os
from pathlib import Path
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tools" / "differential_vectors.json"
ORACLES = {
    "SBB": "SBB_RASTA_ORACLE",
    "Railway-CCS": "RCCS_RASTA_ORACLE",
    "EULYNX": "EULYNX_RASTA_ORACLE",
}


def run_oracle(name, executable, cases):
    request = "".join(json.dumps(case, separators=(",", ":")) + "\n" for case in cases)
    completed = subprocess.run(
        [executable],
        input=request,
        text=True,
        capture_output=True,
        timeout=30,
        check=False,
    )
    if completed.returncode != 0:
        raise RuntimeError(f"{name} exited {completed.returncode}: {completed.stderr.strip()}")
    responses = [json.loads(line) for line in completed.stdout.splitlines() if line.strip()]
    if len(responses) != len(cases):
        raise RuntimeError(f"{name} returned {len(responses)} of {len(cases)} responses")
    for case, response in zip(cases, responses):
        if response.get("case") != case["case"]:
            raise RuntimeError(f"{name} returned an out-of-order case")
        expected = case["expected"]
        if response.get("accepted") != expected["accepted"]:
            raise RuntimeError(f"{name} acceptance differs for {case['case']}")
        if response.get("accepted") and response.get("wire") != expected["wire"]:
            raise RuntimeError(f"{name} wire output differs for {case['case']}")


def main():
    cases = json.loads(FIXTURES.read_text(encoding="ascii"))
    if not isinstance(cases, list) or not cases:
        raise RuntimeError("differential fixture must be a non-empty list")
    used = 0
    for name, variable in ORACLES.items():
        executable = os.environ.get(variable)
        if executable:
            run_oracle(name, executable, cases)
            print(f"{name}: {len(cases)} cases matched")
            used += 1
        else:
            print(f"{name}: skipped ({variable} is not set)")
    print(f"fixtures valid; {used} external oracle(s) executed")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f"differential harness failed: {error}", file=sys.stderr)
        sys.exit(1)
