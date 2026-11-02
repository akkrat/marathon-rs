#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
: "${MARATHON_KT:?Set MARATHON_KT to the read-only Kotlin checkout containing e47352f2}"
STAGE="$ROOT/tools/fake-worker/runs/bundle-rebuild-$(date -u +%Y%m%dT%H%M%S)"
mkdir -p "$STAGE"
git -C "$MARATHON_KT" archive e47352f2 sample/ios-app | tar -x -C "$STAGE"
(cd "$STAGE/sample/ios-app" && xcodebuild build-for-testing -workspace sample-app.xcworkspace -scheme UITesting -sdk iphonesimulator -destination 'generic/platform=iOS Simulator' -derivedDataPath "$STAGE/ios-derived" CODE_SIGNING_ALLOWED=NO)
xcodebuild build-for-testing -project "$ROOT/sample/macos-app/AppleWorld.xcodeproj" -scheme AppleWorld -sdk macosx -destination 'platform=macOS,arch=arm64' -derivedDataPath "$STAGE/macos-derived" CODE_SIGNING_ALLOWED=NO
printf 'Products retained at %s\n' "$STAGE"
