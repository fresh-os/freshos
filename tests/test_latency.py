from harness import FreshOSTestCase

TARGET_NS = 100_000  # spec: median ping/pong round trip under 100 us on QEMU + HVF


class LatencyTest(FreshOSTestCase):
    def test_median_round_trip_is_under_target(self) -> None:
        match = self.boot.wait_for_log(r"\[ping\] rtt_median_ns=(\d+) samples=100", timeout=30)
        median = int(match.group(1))
        print(f"\nping/pong median round trip: {median} ns (target {TARGET_NS})")
        self.assertLess(median, TARGET_NS)

    def test_kernel_measures_every_delivery(self) -> None:
        def latest() -> int:
            return self.boot.mcp().view("metrics")["ipc_delivery"]["latest_ns"]

        self.wait_until(lambda: latest() > 0, timeout=20, message="a measured delivery")
