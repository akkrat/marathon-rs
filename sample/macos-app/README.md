# AppleWorld macOS sample

A small maintained app, hosted XCTest target (pass/fail/skip) and UI-test target
(launch), with a committed shared scheme. The Objective-C sources avoid external
dependencies. `project.yml` is the XcodeGen 2.46.0 input; ordinary builds use the
committed `AppleWorld.xcodeproj` and do not need XcodeGen.

```sh
xcodebuild build-for-testing -project sample/macos-app/AppleWorld.xcodeproj \
  -scheme AppleWorld -sdk macosx -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath tools/fake-worker/runs/macos-derived CODE_SIGNING_ALLOWED=NO
```

WP 0.4b builds the products and drives the Kotlin runner against the fake SSH
world. It does not run tests against the host. WP 7.2 real macOS execution follows the approved direct-host protocol in
[the runbook](../../docs/design/real-e2e.md), including unchanged SIP/TCC evidence.
Shadow/farm runs retain PLAN §6.7 isolation. The real build uses signing defaults
and a separate bounded cancellation-test overlay; this unsigned fake-world
recipe is not proof of real UI launch compatibility.
