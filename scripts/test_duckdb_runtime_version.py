"""Exercise the real Makefile's SQLLogicTest runtime selection."""

import subprocess
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


class RuntimeVersionTests(unittest.TestCase):
    def selection(self, *settings):
        result = subprocess.run(
            [
                "make", "--no-print-directory", "-s", "-f", "Makefile", "-f", "-", *settings,
                "runtime_version_probe",
            ],
            cwd=ROOT, text=True, capture_output=True,
            input=".PHONY: runtime_version_probe\nruntime_version_probe:\n\t@echo $(DUCKDB_PIP_INSTALL)\n",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def test_target_release_pins_runtime(self):
        self.assertEqual(self.selection("TARGET_DUCKDB_VERSION=v1.5.5"), "duckdb==1.5.5")

    def test_other_target_release_pins_runtime(self):
        self.assertEqual(self.selection("TARGET_DUCKDB_VERSION=v1.5.6"), "duckdb==1.5.6")

    def test_explicit_runtime_override(self):
        self.assertEqual(
            self.selection("TARGET_DUCKDB_VERSION=v1.5.5", "DUCKDB_TEST_VERSION=1.5.4"),
            "duckdb==1.5.4",
        )

    def test_development_source_uses_existing_default(self):
        self.assertEqual(self.selection("TARGET_DUCKDB_VERSION=abcdef0123"), "duckdb")

    def test_prerelease_uses_existing_default(self):
        self.assertEqual(self.selection("TARGET_DUCKDB_VERSION=v1.6.0-dev"), "duckdb")


if __name__ == "__main__":
    unittest.main()
