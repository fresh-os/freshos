from harness import FreshOSTestCase


class LatencyBaseline(FreshOSTestCase):
    def test_ping_pong_round_trip_is_measured(self) -> None:
        def latest_us() -> int:
            return self.boot.mcp().view("metrics")["ipc_round_trip"]["latest_us"]

        self.wait_until(lambda: latest_us() > 0, timeout=20, message="a measured round trip")
        print(f"\nBASELINE ping/pong round trip: {latest_us()} us")
