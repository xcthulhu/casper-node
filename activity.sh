#!/usr/bin/env bash

set -euo pipefail

trap 'kill $(jobs -p)' EXIT

# nctl-assets-setup delay=600 nodes=50
# nctl-start
# nctl-view-node-status node=1
# nctl-do-transfers node=all transfers=10000 interval=0.01 amount=1

python -c '
import time
while True:
  print("foo")
  time.sleep(1)
' &
PID=$!

sleep 3
kill $PID

python -c '
import time
while True:
  print("bar")
  time.sleep(1)
' &
PID=$!

sleep 3
kill $PID

python -c '
import time
while True:
  print("baz")
  time.sleep(1)
' &
PID=$!

sleep 3
kill $PID
