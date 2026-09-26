from harness import FreshOSTestCase


class IsolationTest(FreshOSTestCase):
    def test_fault_runs_at_el0_in_its_own_space(self) -> None:
        self.boot.wait_for_log(r"task \d+ el0 asid=\d+ entry=0x4000", timeout=20)

    def test_fault_is_contained_and_restarted(self) -> None:
        self.boot.wait_for_log(r"\[init\] fault exited \(fault\)", timeout=20)
        self.wait_until(
            lambda: len(self.boot.find_logs(r"\[init\] fault exited \(fault\)")) >= 2,
            timeout=20,
            message="fault to crash twice (restarted in between)",
        )
        self.assertTrue(self.services()["pong"]["running"])

    def test_forbidden_memory_access_is_a_contained_fault(self) -> None:
        probes = ("probe-bad-kernel", "probe-bad-unmap", "probe-bad-code", "probe-bad-stack")

        def all_faulted() -> bool:
            services = self.services()
            return all(services.get(p, {}).get("last_exit") == "fault" for p in probes)

        self.wait_until(all_faulted, timeout=20, message=f"{probes} to fault")
        self.assertEqual(self.boot.find_logs(r"SURVIVED"), [])
        self.assertTrue(self.services()["pong"]["running"])

    def test_logs_are_prefixed_with_the_kernel_registered_name(self) -> None:
        self.boot.wait_for_log(r"^\[pulse\] start$", timeout=20)

    def test_fp_simd_state_survives_task_switches(self) -> None:
        self.boot.wait_for_log(r"^\[probe-abi\] fp=", timeout=20)
        self.assertTrue(self.boot.find_logs(r"^\[probe-abi\] fp=intact$"))

    def test_syscall_boundary_rejects_bad_input_without_faulting(self) -> None:
        self.boot.wait_for_log(r"^\[probe-abi\] done$", timeout=20)
        expected = (
            # Newline and ESC become '?', on one line under the kernel's prefix.
            r"^\[probe-abi\] sanitise a\?b\?\[31mc$",
            # Capped at MAX_LOG (256) bytes, then an ellipsis.
            r"^\[probe-abi\] cap:x{252}…$",
            r"^\[probe-abi\] log outside window = -4$",  # BadPointer
            r"^\[probe-abi\] log straddling guard page = -4$",
            r"^\[probe-abi\] unknown syscall = -1$",  # NoSuchSyscall
        )
        for pattern in expected:
            self.assertTrue(self.boot.find_logs(pattern), pattern)
        self.wait_until(
            lambda: self.services()["probe-abi"]["last_exit"] == "clean",
            timeout=20,
            message="probe-abi to exit cleanly",
        )
