"""The installer refuses a host its binary cannot run on, before downloading."""

import os
from pathlib import Path
import subprocess
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(sys.platform.startswith("linux"), "the glibc floor is a Linux check")
class Installer(unittest.TestCase):
    def test_a_host_below_the_glibc_floor_is_refused_with_the_reason(self):
        environment = os.environ | {"E_INSTALL_GLIBC": "99.0"}
        result = subprocess.run(
            ["sh", str(ROOT / "install.sh")],
            capture_output=True,
            text=True,
            env=environment,
        )
        self.assertEqual(result.returncode, 1)
        self.assertIn("need glibc 99.0 or newer", result.stderr)
        # Refused before the download: no archive, no checksum, no install dir.
        self.assertNotIn("checksum", result.stderr)
        self.assertNotIn("e --version", result.stdout)


if __name__ == "__main__":
    unittest.main()
