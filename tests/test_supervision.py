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

        expected = (("fault", "fault"), ("pulse", "clean"))

        def logged() -> dict[str, tuple[int, int]]:
            return {
                name: (
                    len(self.boot.find_logs(rf"^\[init\] {name} exited \({reason}\)$")),
                    len(self.boot.find_logs(rf"^\[init\] restarting {name}$")),
                )
                for name, reason in expected
            }

        # Both services keep exiting while this runs, so bracket the kernel's
        # snapshot with two log counts. The pause lets every line init wrote
        # before the snapshot reach the harness before the second count.
        before = logged()
        services = self.services()
        time.sleep(0.5)
        after = logged()
        for name, _ in expected:
            exits, restarts = services[name]["exits"], services[name]["restarts"]
            with self.subTest(service=name):
                # The kernel counts an exit before init logs it, and init
                # starts no new instance until it has, so at most one exit
                # is ever unlogged.
                self.assertLessEqual(before[name][0], exits, (before, services[name], after))
                self.assertLessEqual(exits, after[name][0] + 1, (before, services[name], after))
                # init logs "restarting" just before the spawn the kernel
                # counts, so at most one logged restart is uncounted.
                self.assertLessEqual(before[name][1] - 1, restarts, (before, services[name], after))
                self.assertLessEqual(restarts, after[name][1], (before, services[name], after))

    def test_every_instance_gets_a_new_generation(self) -> None:
        # fault crash-loops into a freed slot, usually the one it just left,
        # so a reused slot always turns up; wait for one. Instances are told
        # apart by the kernel's restart count, not by (task, generation),
        # so a generation that failed to change would still be caught.
        instances: dict[int, tuple[int, int]] = {}  # restarts -> (task, generation)

        def slot_reused() -> bool:
            fault = self.services().get("fault", {})
            if fault.get("running"):
                instances.setdefault(fault["restarts"], (fault["task"], fault["generation"]))
            tasks = [task for task, _ in instances.values()]
            return len(tasks) > len(set(tasks))

        self.wait_until(slot_reused, timeout=40, message="fault to restart into a slot it used before")
        seen = [instances[restarts] for restarts in sorted(instances)]
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


class BufferingTest(FreshOSTestCase):
    # Only this boot has PROBEBUF.ELF, and it leaves off the other probes, so
    # the two buffering probes always find free task slots.
    boot_options = {"omit": ("probe-bad", "probe-chan"), "stage_as": {"PROBEBUF.ELF": "probe-chan"}}

    def line(self, pattern: str) -> int:
        """The index of the only serial line matching `pattern`."""
        regex = re.compile(pattern)
        found = [i for i, text in enumerate(self.boot.lines) if regex.search(text)]
        self.assertEqual(len(found), 1, f"/{pattern}/ matched lines {found}")
        return found[0]

    def test_queued_messages_survive_a_restart(self) -> None:
        # probe-buf-rx exits after seq 1, so seq 2 and 3 can only reach the
        # instance init restarts. The channel is FIFO, so this holds however
        # the sender's sends interleave with the receiver's exit.
        self.boot.wait_for_log(r"^\[probe-buf-rx\] \[buf\] got seq=3$", timeout=30)
        for seq in (1, 2, 3):
            self.line(rf"^\[probe-buf-tx\] \[buf\] sent seq={seq}$")
        first = self.line(r"^\[probe-buf-rx\] \[buf\] got seq=1$")
        restarted = self.line(r"^\[init\] restarting probe-buf-rx$")
        second = self.line(r"^\[probe-buf-rx\] \[buf\] got seq=2$")
        third = self.line(r"^\[probe-buf-rx\] \[buf\] got seq=3$")
        self.assertLess(first, restarted, "the first instance took seq 1, then exited")
        self.assertLess(restarted, second, "seq 2 waited for the restarted instance")
        self.assertLess(second, third, "the queue kept its order")
        self.assertEqual(self.services()["probe-buf-rx"]["restarts"], 1)


class MalformedElfTest(FreshOSTestCase):
    boot_options = {"extra_files": {"BADELF.ELF": elf_with_wx_segment()}}

    def test_malformed_elf_is_refused(self) -> None:
        self.boot.wait_for_log(r"refused BADELF\.ELF: segment is writable and executable")
        self.boot.wait_for_log(r"\[init\] cannot start probe-badelf: Invalid")
        self.assertTrue(self.services()["pong"]["running"])
