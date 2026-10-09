#!/usr/bin/env bash
# End-to-end test: the tablet's real connection code (Kotlin, on the JVM)
# against the real PC binary. Needs .toolchain (see README).
set -euo pipefail
cd "$(dirname "$0")/.."
. .toolchain/env.sh
WORK="${TMPDIR:-/tmp}/chiz-e2e"
mkdir -p "$WORK"
KOTLINC="${KOTLINC:-.toolchain/kotlinc/bin/kotlinc}"
LIBS=.toolchain/jvmlibs
CP="$LIBS/kotlin-stdlib.jar:$LIBS/json.jar:$LIBS/coroutines.jar"

(cd companion && cargo build -q -p chiz-ui)
A=android/app/src/main/java/xyz/chiz/tablet
rm -rf "$WORK/classes" && mkdir -p "$WORK/classes"
"$KOTLINC" $A/proto/*.kt $A/net/ControlLink.kt $A/net/Connector.kt \
  android/jvmtest/ConnectorE2E.kt -cp "$CP" -d "$WORK/classes" 2>&1 | grep -v '^warning' || true
java -cp "$WORK/classes:$CP" xyz.chiz.tablet.jvmtest.ConnectorE2EKt \
  "$PWD/companion/target/debug/chiz-ui" "$WORK/run"
