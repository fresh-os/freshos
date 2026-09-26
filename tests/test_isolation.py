from harness import FreshOSTestCase


class IsolationTest(FreshOSTestCase):
    def test_fault_runs_at_el0_in_its_own_space(self) -> None:
        self.boot.wait_for_log(r"task \d+ el0 asid=\d+ entry=0x4000", timeout=20)

    def test_fault_is_contained_and_restarted(self) -> None:
        self.boot.wait_for_log(r"service fault exited \(task \d+, reason=fault\)", timeout=20)
        self.wait_until(
            lambda: len(self.boot.find_logs(r"service fault exited")) >= 2,
            timeout=20,
            message="fault to crash twice (restarted in between)",
        )
        self.assertTrue(self.services()["pong"]["running"])

    def test_forbidden_memory_access_is_a_contained_fault(self) -> None:
        probes = ("probe-bad-kernel", "probe-bad-unmapped", "probe-bad-code", "probe-bad-stack")

        def all_faulted() -> bool:
            services = self.services()
            return all(services.get(p, {}).get("last_exit") == "fault" for p in probes)

        self.wait_until(all_faulted, timeout=20, message=f"{probes} to fault")
        self.assertEqual(self.boot.find_logs(r"SURVIVED"), [])
        self.assertTrue(self.services()["pong"]["running"])

    def test_logs_are_prefixed_with_the_kernel_registered_name(self) -> None:
        self.boot.wait_for_log(r"^\[pulse\] start$", timeout=20)
