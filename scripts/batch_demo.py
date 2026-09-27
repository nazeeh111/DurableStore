#!/usr/bin/env python3
"""Exercise an atomic outbox-state transition using disposable local data."""
import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith("DURABLESTORE_FAIL_")}
    with tempfile.TemporaryDirectory(prefix="durablestore-batch-") as directory:
        def run(*command):
            result = subprocess.run([str(binary), directory, *command], env=env,
                                    capture_output=True, text=True, check=True)
            return json.loads(result.stdout)

        def batch(operations):
            path = Path(directory) / "changes.tsv"
            lines = []
            for command, key, *value in operations:
                fields = [command, key.encode().hex()]
                if command == "put":
                    fields.append(value[0].encode().hex())
                lines.append("\t".join(fields))
            path.write_text("\n".join(lines) + "\n")
            return run("batch", str(path))

        def state():
            return {bytes.fromhex(e["key_hex"]).decode(): bytes.fromhex(e["value_hex"]).decode()
                    for e in run("list")["entries"]}

        run("init")
        batch([("put", "job:42", "queued"), ("put", "pending:42", "artifact.txt")])
        before = state()
        if before != {"job:42": "queued", "pending:42": "artifact.txt"}:
            raise AssertionError(before)
        receipt = batch([("put", "job:42", "complete"), ("delete", "pending:42"),
                         ("put", "result:42", "artifact.txt")])
        expected = {"job:42": "complete", "result:42": "artifact.txt"}
        after = state()
        if after != expected or receipt.get("operations") != 3:
            raise AssertionError((receipt, after))
        run("compact")
        if state() != expected:
            raise AssertionError("Compaction changed the state")
        print(json.dumps({"before": before, "after": after, "operations_in_transition": 3,
                          "reopened_and_compacted": True, "external_message_sent": False}, indent=2))


if __name__ == "__main__":
    main()
