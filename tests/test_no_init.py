import time

from harness import FreshOSTestCase


class NoInitTest(FreshOSTestCase):
    """Decision 0006: the kernel requires only init, and has no fallbacks."""

    boot_options = {"omit": ("init",)}
    ready_pattern = r"INIT\.ELF missing from"

    def test_nothing_else_runs(self) -> None:
        time.sleep(3)
        for tag in (r"\[init\]", r"\[kbd\]", r"\[comp\]", r"\[shell\]", r"\[mcp\]"):
            self.assertEqual(self.boot.find_logs(tag), [], f"{tag} ran without init")
