import re
import struct
import time

from harness import Boot, FreshOSTestCase


class SupervisionTest(FreshOSTestCase):
    def test_restarts_are_counted_by_the_kernel(self) -> None:
        self.wait_until(
            lambda: self.services().get("pulse", {}).get("restarts", 0) >= 2,
            timeout=30,
            message="pulse restarted twice",
        )
        self.assertEqual(self.services()["pulse"]["last_exit"], "clean")

    def test_second_receiver_is_refused_by_the_kernel(self) -> None:
        self.boot.wait_for_log(r"\[init\] cannot start probe-dup-recv: ReceiverTaken")
        self.assertTrue(self.services()["pong"]["running"])

    def test_shell_restart_goes_through_init(self) -> None:
        self.boot.wait_for_log(r"\[probe-chan\] \[test\] done", timeout=30)
        self.wait_until(
            lambda: not self.services()["probe-chan"]["running"],
            timeout=10,
            message="probe-chan to finish",
        )
        self.boot.send_keys("restart probe-chan\r")
        self.wait_until(
            lambda: len(self.boot.find_logs(r"\[probe-chan\] \[test\] done")) >= 2,
            timeout=20,
            message="probe-chan to run again",
        )

    def test_crash_loop_does_not_leak_frames(self) -> None:
        def state() -> tuple[bool, int, bool, int]:
            services = self.services()
            fault, pulse = services.get("fault", {}), services.get("pulse", {})
            return (
                fault.get("running", True),
                fault.get("restarts", 0),
                pulse.get("running", False),
                pulse.get("restarts", 0),
            )

        def frames_between_fault_runs() -> int:
            # Sample while fault is down, so no instance's frames are counted,
            # and while pulse is up, so its are always counted. Each is down
            # for only a few hundred ms, so keep only a sample taken with the
            # same state, and the same restart counts, on both sides of it.
            deadline = time.monotonic() + 20
            while time.monotonic() < deadline:
                before = state()
                if before[0] or not before[2]:
                    time.sleep(0.02)
                    continue
                frames = self.boot.mcp().view("system")["frames_free"]
                if state() == before:
                    return frames
            self.fail("no sample with fault down and pulse up")

        def restarts() -> int:
            # mcp starts first, so fault may not be registered yet.
            return self.services().get("fault", {}).get("restarts", 0)

        self.wait_until(lambda: restarts() >= 2, timeout=30, message="fault restarting")
        before, first = frames_between_fault_runs(), restarts()
        self.wait_until(lambda: restarts() >= first + 5, timeout=60, message="five more restarts")
        after = frames_between_fault_runs()
        self.assertLessEqual(abs(before - after), 2, f"frames {before} -> {after}")

    def test_queued_messages_survive_a_restart(self) -> None:
        # probe-buf-rx exits after seq 1; probe-buf-tx sends 2 and 3 while it
        # is down. They must wait in the channel for the restarted instance.
        self.boot.wait_for_log(r"^\[probe-buf-rx\] \[buf\] got seq=3$", timeout=30)

        def line(pattern: str) -> int:
            regex = re.compile(pattern)
            return next(i for i, text in enumerate(self.boot.lines) if regex.search(text))

        sent = line(r"^\[probe-buf-tx\] \[buf\] sent seq=3$")
        restarted = line(r"^\[init\] restarting probe-buf-rx$")
        received = line(r"^\[probe-buf-rx\] \[buf\] got seq=2$")
        self.assertLess(line(r"^\[probe-buf-rx\] \[buf\] got seq=1$"), sent)
        self.assertLess(sent, restarted, "seq 3 was sent while the receiver was down")
        self.assertLess(restarted, received, "seq 2 reached the restarted instance")
        self.assertGreaterEqual(self.services()["probe-buf-rx"]["restarts"], 1)

    def test_exits_are_charged_to_the_exact_task(self) -> None:
        # A task id is reused as soon as its task exits; the generation tells
        # the instances apart, so each service is charged only its own exits.
        self.wait_until(
            lambda: self.services().get("fault", {}).get("restarts", 0) >= 3
            and self.services().get("pulse", {}).get("restarts", 0) >= 1,
            timeout=40,
            message="fault and pulse restarting",
        )
        tasks = self.boot.mcp().view("tasks")
        self.assertTrue(all(task["generation"] >= 1 for task in tasks), tasks)
        self.assertEqual(self.boot.find_logs(r"^\[init\] fault exited \(clean\)"), [])
        self.assertEqual(self.boot.find_logs(r"^\[init\] pulse exited \(fault\)"), [])
        services = self.services()
        for name, reason in (("fault", "fault"), ("pulse", "clean")):
            logged = len(self.boot.find_logs(rf"^\[init\] {name} exited \({reason}\)$"))
            restarts = len(self.boot.find_logs(rf"^\[init\] restarting {name}$"))
            with self.subTest(service=name):
                # The kernel records an exit just before init hears of it,
                # and a restart just after init asks, so allow one in flight.
                self.assertIn(services[name]["exits"] - logged, (0, 1))
                self.assertIn(restarts - services[name]["restarts"], (0, 1))

    def test_every_instance_gets_a_new_generation(self) -> None:
        # fault restarts into a freed slot, usually the one it just left.
        seen: list[tuple[int, int]] = []

        def fault_instances() -> int:
            fault = self.services().get("fault", {})
            instance = (fault.get("task"), fault.get("generation"))
            if fault.get("running") and instance not in seen:
                seen.append(instance)
            return len(seen)

        self.wait_until(lambda: fault_instances() >= 3, timeout=40, message="three fault instances")
        for i, (task, generation) in enumerate(seen):
            for earlier_task, earlier_generation in seen[:i]:
                if task == earlier_task:
                    self.assertGreater(generation, earlier_generation, seen)

    def test_kernel_stack_peaks_are_reported(self) -> None:
        tasks = {task["name"]: task for task in self.boot.mcp().view("tasks")}
        for name in ("init", "mcp"):
            with self.subTest(task=name):
                peak = tasks[name]["stack_peak_bytes"]
                self.assertIsNotNone(peak)
                self.assertGreater(peak, 800)  # at least one saved frame
                self.assertLess(peak, 32 * 1024)


def elf_with_wx_segment() -> bytes:
    """A minimal aarch64 ELF whose only segment is writable and executable."""
    header = struct.pack(
        "<4sBBBBB7xHHIQQQIHHHHHH",
        b"\x7fELF", 2, 1, 1, 0, 0,          # 64-bit, little-endian, version 1
        2, 0xB7, 1,                         # ET_EXEC, EM_AARCH64, version
        0x4_0000_0078, 64, 0,               # entry, phoff, shoff
        0, 64, 56, 1, 64, 0, 0,             # flags, ehsize, phentsize, phnum, shentsize, shnum, shstrndx
    )
    segment = struct.pack(
        "<IIQQQQQQ",
        1, 7,                               # PT_LOAD, flags R|W|X
        0, 0x4_0000_0000, 0x4_0000_0000,    # offset, vaddr, paddr
        0x80, 0x80, 0x1000,                 # filesz, memsz, align
    )
    return (header + segment).ljust(0x80, b"\0")


class MalformedElfTest(FreshOSTestCase):
    boot_options = {"extra_files": {"BADELF.ELF": elf_with_wx_segment()}}

    def test_malformed_elf_is_refused(self) -> None:
        self.boot.wait_for_log(r"refused BADELF\.ELF: segment is writable and executable")
        self.boot.wait_for_log(r"\[init\] cannot start probe-badelf: Invalid")
        self.assertTrue(self.services()["pong"]["running"])
