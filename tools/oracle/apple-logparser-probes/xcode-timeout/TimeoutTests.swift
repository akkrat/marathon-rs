import XCTest
final class TimeoutTests: XCTestCase {
    func testHang() {
        executionTimeAllowance = 60
        Thread.sleep(forTimeInterval: 240)
    }
}
