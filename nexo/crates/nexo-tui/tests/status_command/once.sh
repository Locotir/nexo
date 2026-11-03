#!/bin/sh
# Works the first time, fails after: the bar keeps the last good line.
# $1 is a file that marks the first run as done.
if [ -e "$1" ]; then exit 2; fi
: > "$1"
echo "first and last good line"
