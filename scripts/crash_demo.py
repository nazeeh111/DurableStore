#!/usr/bin/env python3
"""Exercise the compiled fault-injection binary without editing existing stores."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

POINTS = (
    "append_header", "append_payload", "append_sync", "compact_header",
    "compact_record", "compact_sync", "compact_rename", "compact_dirsync",
)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    results = []
    for point in POINTS:
        with tempfile.TemporaryDirectory(prefix="durablestore-demo-") as directory:
            clean_env = {key: value for key, value in os.environ.items()
                         if not key.startswith("DURABLESTORE_FAIL_")}

            def run(*command, fail_at=None, expected=0):
                env = dict(clean_env)
                if fail_at:
                    env["DURABLESTORE_FAIL_AT"] = fail_at
                result = subprocess.run([str(binary), directory, *command],
                                        capture_output=True, text=True, env=env, check=False)
                if result.returncode != expected:
                    raise RuntimeError(f"{point}/{command}: expected exit {expected}, "
                                       f"got {result.returncode}: {result.stderr}")
                return json.loads(result.stdout) if result.stdout else None

            run("init")
            run("put", "61636b", "64757261626c65")  # ack = durable
            run("put", "676f6e65", "76616c7565")
            run("delete", "676f6e65")
            operation = ("put", "696e666c69676874", "6d61796265") if point.startswith("append") else ("compact",)
            run(*operation, fail_at=point, expected=86)
            recovered = run("get", "61636b")
            deleted = run("get", "676f6e65", expected=3)
            if recovered != {"found": True, "value_hex": "64757261626c65"} or deleted != {"found": False}:
                raise AssertionError(f"Acknowledged state was lost at {point}")
            run("put", "706f7374", "72657374617274")
            results.append({"boundary": point, "acknowledged_value_survived": True,
                            "acknowledged_delete_survived": True, "restart_writable": True})
    print(json.dumps({"process_crash_only": True, "cases": results}, indent=2))


if __name__ == "__main__":
    main()
