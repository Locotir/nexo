#!/bin/sh
# Prints back fields of the JSON nexo sends on stdin, in colour.
read -r j
field() { printf '%s' "$j" | sed -n "s/.*\"$1\":\"*\([^\",}]*\).*/\1/p"; }
printf '\033[1;35m%s\033[0m on %s | ctx \033[32m%s%%\033[0m | $%s | %s | turns %s\n' \
  "$(field id)" "$(field provider)" "$(field percent)" "$(field usd)" "$(field session_id)" "$(field turns)"
echo "a second line nexo leaves out"
