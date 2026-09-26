import unittest

from harness import FreshOSTestCase

# probe-chan logs "[test] <case> result=<refused|ok> code=<n>". Expected codes
# come from freshos_abi::Error.
EXPECTED = {
    "ungranted-send": -2,          # NoSuchHandle
    "recv-on-send-only": -3,       # NoRight
    "send-from-kernel-memory": -4,  # BadPointer
    "send-from-null": -4,
    "send-unaligned": -4,
    "send-straddles-window-end": -4,
    "recv-into-code": -4,
    "log-from-kernel-memory": -4,
    "queue-full": -11,             # Full, on the 17th send
}


class ChannelTest(FreshOSTestCase):
    ready_pattern = r"\[probe-chan\] \[test\] done"

    def result(self, case: str) -> int:
        match = self.boot.wait_for_log(rf"\[test\] {case} result=\w+ code=(-?\d+)", timeout=5)
        return int(match.group(1))

    def test_every_refusal_returns_the_right_error(self) -> None:
        for case, code in EXPECTED.items():
            with self.subTest(case=case):
                self.assertEqual(self.result(case), code)

    @unittest.expectedFailure  # Task 7 adds SPAWN; remove this marker then.
    def test_spawn_is_refused_outside_init(self) -> None:
        self.assertEqual(self.result("spawn-not-init"), -8)  # NotPermitted

    def test_refused_sends_queued_nothing(self) -> None:
        # The refused sends above all targeted the sink. If any had queued a
        # message, the queue would fill before 16 legitimate sends succeeded.
        match = self.boot.wait_for_log(r"\[test\] queue-full result=refused code=-11 after=(\d+)")
        self.assertEqual(int(match.group(1)), 16)

    def test_hostile_log_text_is_contained(self) -> None:
        long_line = self.boot.wait_for_log(r"^\[probe-chan\] (L+)…$").group(1)
        self.assertEqual(len(long_line), 256)
        self.boot.wait_for_log(r"^\[probe-chan\] bad\?utf8$")
        self.boot.wait_for_log(r"^\[probe-chan\] fake\?\[init\] spoof$")
        self.assertEqual(self.boot.find_logs(r"^\[init\] spoof"), [])
