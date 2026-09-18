#!/usr/bin/env bash
set -euo pipefail

# Summarize one explicitly captured desktop log and one phone log. The script
# never reads the global append-only anchor.log: callers must provide fresh
# files created for a single experiment.

desktop_log="${1:?usage: $0 <fresh-desktop.log> <fresh-phone.log> [report.md]}"
phone_log="${2:?usage: $0 <fresh-desktop.log> <fresh-phone.log> [report.md]}"
report="${3:-/tmp/sideboat-trace-summary-$(date +%Y%m%d-%H%M%S).md}"

[[ -f "$desktop_log" ]] || { echo "desktop log not found: $desktop_log" >&2; exit 2; }
[[ -f "$phone_log" ]] || { echo "phone log not found: $phone_log" >&2; exit 2; }

desktop_run_ids="$(sed -n 's/.*\[SIDEBOAT_RUN_START\].*run_id=\([^ ]*\).*/\1/p' "$desktop_log" | sort -u)"
desktop_run_count="$(printf '%s\n' "$desktop_run_ids" | sed '/^$/d' | wc -l)"
if [[ "$desktop_run_count" != 1 ]]; then
    echo "expected exactly one desktop run marker in $desktop_log; found $desktop_run_count" >&2
    echo "capture a new desktop log per run rather than grepping the append-only global log" >&2
    exit 2
fi
desktop_run_id="$(printf '%s\n' "$desktop_run_ids" | head -1)"

phone_run_ids="$(sed -n 's/.*run_id=\([^ ]*\).*/\1/p' "$phone_log" | sort -u)"
phone_run_count="$(printf '%s\n' "$phone_run_ids" | sed '/^$/d' | wc -l)"
event_count() { rg -c "$1" "$2" || true; }

{
    printf '%s\n\n' '# Sideboat trace summary'
    printf '%s\n' "- desktop run: \`$desktop_run_id\`"
    printf '%s\n' "- phone run IDs observed: ${phone_run_ids:-none}"
    printf '%s\n' "- desktop trace file: \`$desktop_log\`"
    printf '%s\n\n' "- phone trace file: \`$phone_log\`"
    printf '%s\n' '| event | desktop | phone |'
    printf '%s\n' '| --- | ---: | ---: |'
    printf '| capture ready / stream chunks | %s | %s |\n' \
        "$(event_count 'event":"capture_ready' "$desktop_log")" \
        "$(event_count 'event=stream_chunk' "$phone_log")"
    printf '| sender accepted / assembler complete | %s | %s |\n' \
        "$(event_count 'sender_outcome.*accepted' "$desktop_log")" \
        "$(event_count 'event=assembler_complete' "$phone_log")"
    printf '| broadcaster coalesced / receiver replacement | %s | %s |\n' \
        "$(event_count 'event":"broadcaster_coalesce' "$desktop_log")" \
        "$(event_count 'event=frame_dropped.*outcome=queue_replace' "$phone_log")"
    printf '| sender rejected / assembler rejected | %s | %s |\n' \
        "$(event_count 'sender_outcome.*rejected' "$desktop_log")" \
        "$(event_count 'event=assembler_rejected' "$phone_log")"
    printf '| decoder inputs / decoder outputs | — | %s / %s |\n' \
        "$(event_count 'event=codec_input' "$phone_log")" \
        "$(event_count 'event=codec_output' "$phone_log")"
    printf '\n%s\n' 'The clocks are device-local. Use this report to find missing stages and queue pressure; do not subtract a desktop timestamp from an Android timestamp as a latency measurement.'
    if [[ "$phone_run_count" != 1 ]]; then
        printf '\n%s\n' "Warning: the phone log contains $phone_run_count run IDs. Re-capture with the app's current PID if you need one unambiguous session."
    fi
    printf '\n%s\n' 'Latest Android ingress metrics:'
    rg 'event=ingress_metrics' "$phone_log" | tail -1 || true
} >"$report"

printf 'report=%s\n' "$report"
