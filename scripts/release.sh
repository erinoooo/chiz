#!/usr/bin/env bash
# Local release: verify, build Linux/Windows/Android artifacts, publish with gh.
#   scripts/release.sh v0.5.0          build + publish
#   scripts/release.sh v0.5.0 --dry    build only (artifacts land in dist/)
set -euo pipefail
cd "$(dirname "$0")/.."
VER="${1:?usage: scripts/release.sh vX.Y.Z [--dry]}"
DRY="${2:-}"
. .toolchain/env.sh
[ -f .toolchain/signing.env ] && . .toolchain/signing.env   # CHIZ_KEYSTORE* vars

echo "==> verify"
(cd companion && cargo fmt --all -- --check \
  && cargo clippy --workspace --exclude chiz-platform-windows --all-targets --locked -- -D warnings \
  && cargo test --workspace --exclude chiz-platform-windows --locked)
python3 protocol/verify_m0.py | tail -1
python3 protocol/verify_m1m2.py | tail -1

echo "==> kotlin protocol tests"
KT_OUT="${TMPDIR:-/tmp}/chiz-kt" && rm -rf "$KT_OUT" && mkdir -p "$KT_OUT"
KCP=.toolchain/jvmlibs/kotlin-stdlib.jar:.toolchain/jvmlibs/json.jar
.toolchain/kotlinc/bin/kotlinc android/app/src/main/java/xyz/chiz/tablet/proto/*.kt \
  android/jvmtest/TestMain.kt -cp "$KCP" -d "$KT_OUT" 2>&1 | grep -v '^warning' || true
java -cp "$KT_OUT:$KCP" xyz.chiz.tablet.jvmtest.TestMainKt | tail -1 | grep -q "FAILURES: none"

echo "==> end-to-end: tablet connection code vs the PC binary"
scripts/e2e.sh | tail -1 | grep -q "ALL PASSED"

rm -rf dist && mkdir dist

echo "==> linux"
(cd companion && cargo build --release --locked -p chiz-ui)
cp companion/target/release/chiz-ui dist/chiz-ui-linux-x86_64

echo "==> windows"
if command -v cargo-zigbuild >/dev/null 2>&1; then
  (cd companion && cargo zigbuild --release --locked -p chiz-ui --target x86_64-pc-windows-gnu)
  cp companion/target/x86_64-pc-windows-gnu/release/chiz-ui.exe dist/chiz-ui-windows-x86_64.exe
else
  echo "   skipped (cargo-zigbuild not installed)"
fi

echo "==> android"
if [ -n "${CHIZ_KEYSTORE:-}" ]; then
  (cd android && gradle :app:assembleRelease --no-daemon -q)
  cp android/app/build/outputs/apk/release/app-release.apk dist/chiz-tablet.apk
else
  echo "   no signing keystore: shipping the debug-signed APK"
  (cd android && gradle :app:assembleDebug --no-daemon -q)
  cp android/app/build/outputs/apk/debug/app-debug.apk dist/chiz-tablet.apk
fi

(cd dist && sha256sum * > SHA256SUMS && cat SHA256SUMS)
[ "$DRY" = "--dry" ] && { echo "dry run: artifacts in dist/"; exit 0; }

echo "==> publish $VER"
git tag -a "$VER" -m "Chiz $VER" 2>/dev/null || true
git push origin HEAD "$VER"
gh release create "$VER" dist/* --title "Chiz $VER" --generate-notes
