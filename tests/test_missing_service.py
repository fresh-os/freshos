from harness import FreshOSTestCase


class MissingPongTest(FreshOSTestCase):
    boot_options = {"omit": ("pong",)}

    def test_everything_else_runs_and_pong_is_reported(self) -> None:
        self.boot.wait_for_log(r"\[init\] cannot start pong: NotFound")
        services = self.services()
        self.assertFalse(services.get("pong", {}).get("running", False))
        for name in ("kbd", "comp", "shell", "dash", "ping", "mcp"):
            self.assertTrue(services[name]["running"], name)
