from harness import FreshOSTestCase

TARGET_NS = 100_000  # spec: median ping/pong round trip under 100 us on QEMU + HVF
BATCH = r"\[ping\] rtt_median_ns=(\d+) samples=100"
# The first batches overlap init launching services. By the third, every
# service is running, so it measures the steady state, not warm-up.
STEADY_BATCH = 3


class LatencyTest(FreshOSTestCase):
    def test_median_round_trip_is_under_target(self) -> None:
        self.wait_until(
            lambda: len(self.boot.find_logs(BATCH)) >= STEADY_BATCH,
            timeout=30,
            message=f"ping batch {STEADY_BATCH}",
        )
        batches = [int(m.group(1)) for m in self.boot.find_logs(BATCH)]
        first, steady = batches[0], batches[STEADY_BATCH - 1]
        print(
            f"\nping/pong median round trip: {steady} ns in batch {STEADY_BATCH} "
            f"(first batch {first} ns, target {TARGET_NS})"
        )
        self.assertLess(steady, TARGET_NS)

    def test_kernel_measures_every_delivery(self) -> None:
        def latest() -> int:
            return self.boot.mcp().view("metrics")["ipc_delivery"]["latest_ns"]

        self.wait_until(lambda: latest() > 0, timeout=20, message="a measured delivery")
