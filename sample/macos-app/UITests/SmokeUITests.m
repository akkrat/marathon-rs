#import <XCTest/XCTest.h>
@interface SmokeUITests : XCTestCase
@end
@implementation SmokeUITests
- (void)testLaunch { [[[XCUIApplication alloc] init] launch]; }
@end
