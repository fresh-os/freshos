from harness import FreshOSTestCase


class TraceTest(FreshOSTestCase):
    def test_pings_carry_the_real_sender(self) -> None:
        def pings() -> list[dict]:
            return [m for m in self.boot.mcp().view("message_trace") if m["type"] == "PING"]

        self.wait_until(lambda: bool(pings()), timeout=20, message="a PING in the trace")
        for message in pings():
            self.assertEqual(message["from"]["name"], "ping")
            self.assertEqual(message["to"]["name"], "pong")
