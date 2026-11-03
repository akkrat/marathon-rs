# Testing

## Test timing rules

Tests run in parallel on small hosted runners (two cores) and on loaded build boxes, so any real-time margin will eventually be exceeded. The fixes behind U99, U116 and U120–U143 in [found-bugs.md](found-bugs.md) give these rules:

1. A positive real-time deadline (`timeout`, `recv_timeout`, a polling deadline) only catches hangs: use 60 s, whatever the expected time is.
2. Wait for work outside the current task (processes, sockets, blocking threads) by polling with a short `sleep` or a notification, never by spinning on `yield_now`.
3. Do not bind port 0, drop the socket and reuse the port; keep the socket, or serialise the tests that bind ports (U128).
4. Do not assume an interleaving of independent tasks. Hold the work you want to order on a gate, assert, then release it (U99, U124, U127); read ids and log positions from what the fake recorded (U129); join tasks before counting what they left behind (U126).
5. Prefer a paused or virtual clock (`start_paused`, `run_virtual`, `advance`) to real time whenever the code under test lets the test drive it.
6. A real-time upper bound (`elapsed < x`) must only separate two outcomes; give the slower one a wide margin, for example by making the competing event much later, instead of bounding how fast the host is.
7. A negative check (a timeout that must elapse) passes on a slow host; where possible, wait for an event that ends the window (the client returning) instead of a fixed time.
