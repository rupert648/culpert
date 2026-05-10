#!/usr/bin/env bash
# Load script for mock-axum. Hammers each route N times then dumps a
# culpert pprof profile.
#
# Usage:
#   ./examples/mock-axum/load.sh                 # default N=200
#   N=1000 ./examples/mock-axum/load.sh
#   APP=http://127.0.0.1:8080 N=200 ./examples/mock-axum/load.sh

set -euo pipefail

APP="${APP:-http://127.0.0.1:8080}"
TELEMETRY="${TELEMETRY:-http://127.0.0.1:8081}"
N="${N:-200}"
OUT="${OUT:-/tmp/mock-axum.pb.gz}"

routes=(/cheap /json /strings /vec /nested)

echo "hitting $APP routes ${routes[*]} $N times each"
for route in "${routes[@]}"; do
  for _ in $(seq 1 "$N"); do
    curl -fsS "$APP$route" > /dev/null
  done
  echo "  $route ok"
done

echo "fetching pprof profile from $TELEMETRY/debug/alloc/profile"
curl -fsS -o "$OUT" "$TELEMETRY/debug/alloc/profile"
echo "wrote $OUT ($(wc -c < "$OUT") bytes)"

echo
echo "verify:"
echo "  pprof -tags $OUT"
echo "  pprof -tagfocus=span_name:json -text $OUT"
echo "  pprof -http=:8090 $OUT"
