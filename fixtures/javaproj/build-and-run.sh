#!/bin/bash
# PrincessIDE Java fixture — build and run script
#
# Usage: bash fixtures/javaproj/build-and-run.sh
#
# This script demonstrates the Java build (javac) and run (JVM) pipeline.
# It is the canonical verification for P-F1 Step 2.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

# Clean previous build
rm -rf build

# Build
echo "=== Building with javac ==="
mkdir -p build
javac -d build Main.java
echo "Build succeeded: build/Main.class"

# Run
echo ""
echo "=== Running with java ==="
java -cp build Main
echo ""
echo "=== Java fixture verification complete ==="
