from harness import FreshOSTestCase

# Services that stay running once booted. pulse and fault come and go by design.
LONG_RUNNING = ("kbd", "comp", "shell", "dash", "ping", "pong", "mcp")


class BootTest(FreshOSTestCase):
    def test_every_long_running_service_is_running(self) -> None:
        def all_running() -> bool:
            services = self.services()
            return all(services.get(name, {}).get("running") for name in LONG_RUNNING)

        self.wait_until(all_running, timeout=20, message=f"all of {LONG_RUNNING} running")

    def test_tasks_are_named_by_the_kernel(self) -> None:
        names = {task["name"] for task in self.boot.mcp().view("tasks")}
        self.assertLessEqual(set(LONG_RUNNING), names)
