#import <XCTest/XCTest.h>
@interface SmokeTests : XCTestCase
@end
@implementation SmokeTests
- (void)testPass { XCTAssertEqual(2 + 2, 4); }
- (void)testFail { XCTFail(@"scripted failure"); }
- (void)testSkip { XCTSkip(@"scripted skip"); }
@end
