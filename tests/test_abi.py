import subprocess
import unittest

from harness import REPO


class AbiUnitTest(unittest.TestCase):
    """Runs freshos-abi's Rust unit tests on the host; no boot needed."""

    def test_abi_unit_tests_pass(self) -> None:
        version = subprocess.run(
            ["rustup", "run", "nightly", "rustc", "-vV"], capture_output=True, text=True, check=True
        ).stdout
        host = next(line.split()[1] for line in version.splitlines() if line.startswith("host:"))
        # The workspace builds only core and alloc for its targets, so the
        # host test harness needs std and test built too.
        result = subprocess.run(
            [
                "rustup", "run", "nightly", "cargo", "test", "--package", "freshos-abi",
                "--target", host, "-Zbuild-std=core,alloc,std,test",
            ],
            cwd=REPO,
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("test tests::task_ref_round_trips_through_its_packed_form ... ok", result.stdout)
