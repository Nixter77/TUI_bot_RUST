#!/usr/bin/env bash
# Print current UTC hour and desk_owner (table matches DESK_SCHEDULE.md / desk_schedule.rs).
# No trading side effects.
set -euo pipefail
HOUR=$(date -u +%H)
HOUR=$((10#$HOUR))

owner_for() {
  local h=$1
  case $h in
    0|1|7|8|9|13|14|15) echo S4 ;;
    22|23) echo S3 ;;
    *) echo S2 ;;
  esac
}

OWNER=$(owner_for "$HOUR")
printf 'UTC hour: %02d → desk_owner: %s\n' "$HOUR" "$OWNER"
echo 'Table (UTC):'
echo '  00-02 S4 | 02-07 S2 | 07-10 S4 | 10-13 S2 | 13-16 S4 | 16-22 S2 | 22-24 S3'
echo "DESK_SCHEDULE=${DESK_SCHEDULE:-<unset>} (on only if 1/true/yes)"
