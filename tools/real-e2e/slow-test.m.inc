// Applied only to the disposable sample build, never to a copied fixture.
- (void)testBoundedSlow {
    fprintf(stderr, "MRS_REAL_E2E_SLOW_STARTED\n");
    fflush(stderr);
    [NSThread sleepForTimeInterval:120.0];
}
