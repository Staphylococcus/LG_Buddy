#!/usr/bin/env python3
import copy
import json
import shutil
import struct
import tempfile
import unittest
from pathlib import Path

import kwin_matrix as matrix


class KWinMatrixTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.output = Path(self.temp.name)
        self.manifest = matrix.load_manifest()
        self.rows = matrix.targets(self.manifest)
        self.source_id = matrix.source_identity()

    def artifact(self, row):
        binary = bytearray(64)
        binary[:6] = b"\x7fELF\x02\x01"
        struct.pack_into("<HH", binary, 16, 3, 62)
        binary += json.dumps(row, sort_keys=True).encode()
        checksum = matrix.digest(binary)
        fields = [row["kwin_version"], row["qt_version"], row["architecture"], self.source_id, checksum]
        path = self.output / f"{fields[0]}-{fields[1]}-{fields[2]}-{checksum}"
        path.mkdir()
        (path / "plugin.so").write_bytes(binary)
        (path / "metadata.tsv").write_text("\t".join(fields) + "\n")
        (path / "build.json").write_text(json.dumps({
            "target": row, "inputs_sha256": matrix.input_identity([row]),
            "plugin_sha256": checksum, "loader_test": "passed",
        }))
        return path

    def record(self, path, **changes):
        record_path = path / "build.json"
        record = json.loads(record_path.read_text())
        record.update(changes)
        record_path.write_text(json.dumps(record))

    def test_committed_floor_and_qt_coverage(self):
        expected_versions = {f"6.{minor}.{patch}" for minor, last in ((4, 6), (5, 6), (6, 6), (7, 5)) for patch in range(last + 1)}
        self.assertEqual({r["kwin_version"] for r in self.rows}, expected_versions)
        self.assertEqual(len(self.rows), 33)
        self.assertEqual(len(matrix.targets(self.manifest, "6.4", "6.8")), 7)
        for row in self.rows:
            minimum = {"6.4": 8, "6.5": 9, "6.6": 10, "6.7": 10}[row["kwin_version"].rsplit(".", 1)[0]]
            self.assertGreaterEqual(int(row["qt_minor"].split(".")[1]), minimum)

    def test_complete_matrix_and_missing_variant(self):
        paths = [self.artifact(row) for row in self.rows]
        self.assertEqual(matrix.verify(self.output, self.rows), 33)
        shutil.rmtree(paths[-1])
        with self.assertRaisesRegex(ValueError, "Missing KWin artifacts"):
            matrix.verify(self.output, self.rows)

    def test_corruption_stale_source_and_missing_validation(self):
        row = self.rows[0]
        path = self.artifact(row)
        plugin = path / "plugin.so"
        original = plugin.read_bytes()
        plugin.write_bytes(original + b"corrupt")
        with self.assertRaisesRegex(ValueError, "corrupt plugin"):
            matrix.verify(self.output, [row])
        plugin.write_bytes(original)
        with self.assertRaisesRegex(ValueError, "Stale source"):
            matrix.verify(self.output, [row], bridge_id="0" * 64)
        self.record(path, loader_test="not run")
        with self.assertRaisesRegex(ValueError, "Missing loader validation"):
            matrix.verify(self.output, [row])

    def test_wrong_qt_toolchain_or_source_revision(self):
        row = self.rows[0]
        path = self.artifact(row)
        wrong = copy.deepcopy(row)
        wrong["environment"]["qt_nixpkgs"]["rev"] = "0" * 40
        self.record(path, target=wrong)
        with self.assertRaisesRegex(ValueError, "different matrix inputs"):
            matrix.verify(self.output, [row])
        self.record(path, target=row, inputs_sha256="0" * 64)
        with self.assertRaisesRegex(ValueError, "different matrix inputs"):
            matrix.verify(self.output, [row])

    def test_unexpected_version_and_symlinks(self):
        row = self.rows[0]
        path = self.artifact(row)
        with self.assertRaisesRegex(ValueError, "Unexpected or duplicate artifact"):
            matrix.verify(self.output, [self.rows[-1]])
        plugin = path / "plugin.so"
        copy_path = self.output.parent / (self.output.name + "-external")
        copy_path.write_bytes(plugin.read_bytes())
        self.addCleanup(copy_path.unlink)
        plugin.unlink()
        plugin.symlink_to(copy_path)
        with self.assertRaisesRegex(ValueError, "Missing or linked artifact"):
            matrix.verify(self.output, [row])

    def test_manifest_rejects_unpinned_sources_and_duplicate_targets(self):
        path = self.output / "targets.json"
        for change in (lambda m: m["kwin"][0].update(rev="master"),
                       lambda m: m["kwin"].append(m["kwin"][0]),
                       lambda m: m["environments"]["6.8"]["qt_nixpkgs"].update(rev="nixos-unstable")):
            altered = copy.deepcopy(self.manifest)
            change(altered)
            path.write_text(json.dumps(altered))
            with self.assertRaises(ValueError):
                matrix.load_manifest(path)


if __name__ == "__main__":
    unittest.main()
